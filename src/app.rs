//! Estado, acciones y lógica de Xool Files.

use crate::i18n::tr;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::{channel::oneshot, future::{Either, select}};
use gpui::prelude::*;
use gpui::{
    Along, Focusable, Bounds, Context, Entity, FocusHandle, KeyBinding, KeyDownEvent, KeystrokeEvent,
    MouseDownEvent, MouseMoveEvent, NoAction, Pixels, RenderImage, Subscription, Window, actions,
};
use gpui_component::input::{InputEvent, InputState};
use notify::{Config, PollWatcher, RecursiveMode, Watcher};

use crate::fs_model::{self, Entry, FileKind, SortKey};
use crate::media::{self, AudioPlayback, VideoPlayer};
use crate::sounds::{ActionSound, SoundAction};
use crate::pane_layout::{Axis, DIVIDER, PaneLayout, PaneSession, SavedPane};
use crate::platform::{self, DriveInfo};
use crate::settings::{Accent, DETAIL_COLUMNS, DetailColumn, DetailLabel, Settings, ThemeMode, ViewMode};
use crate::shell_menu::{ShellClient, ShellRow, ShellRows, ShellTarget};
use crate::shortcuts;
use crate::theme;
use crate::thumbs::{self, PreviewContent};

actions!(
    xool_files,
    [
        NewTab,
        CloseTab,
        NextTab,
        PrevTab,
        Back,
        Forward,
        Up,
        Refresh,
        FocusAddress,
        FocusSearch,
        ToggleRecursiveSearch,
        ToggleHidden,
        ToggleSplit,
        TogglePreview,
        ToggleSidebar,
        NewFolder,
        RenameSelected,
        DeleteSelected,
        CopySelected,
        CutSelected,
        PasteFiles,
        OpenSelected,
        SelectAllFiles,
        OpenSettings,
        ToggleViewMode,
        FavoriteCurrent,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        EscapeOverlay,
        SortByName,
        SortBySize,
        SortByKind,
        SortByModified,
    ]
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastTone {
    Info,
    Success,
    Error,
}

pub(crate) const ABOUT_SETTINGS_PAGE: usize = 4;

/// La cuadrícula usa tamaños de icono entre el 100 % y el 200 % originales.
pub(crate) fn icon_size_for_scale(scale: f32) -> f32 {
    54.0 * scale.clamp(1.0, 2.0).powf(1.9)
}

pub(crate) fn compact_list_columns(pane_width: f32) -> usize {
    if pane_width >= 560.0 { 2 } else { 1 }
}

#[derive(Clone)]
pub struct Toast {
    pub text: String,
    pub tone: ToastTone,
}

struct FileUndo {
    from: PathBuf,
    to: PathBuf,
    recycle_meta: Option<PathBuf>,
    identity: platform::FileIdentity,
}

impl FileUndo {
    fn new(from: PathBuf, to: PathBuf, recycle_meta: Option<PathBuf>) -> Option<Self> {
        let identity = platform::file_identity(&from).ok()?;
        Some(Self { from, to, recycle_meta, identity })
    }

    fn moves(paths: &[PathBuf], destination: &Path) -> Vec<Self> {
        paths.iter().filter_map(|source| {
            let target = destination.join(source.file_name()?);
            if !matches!(std::fs::symlink_metadata(&target), Err(error) if error.kind() == std::io::ErrorKind::NotFound) {
                return None;
            }
            let identity = platform::file_identity(source).ok()?;
            Some(Self { from: target, to: source.clone(), recycle_meta: None, identity })
        }).collect()
    }
}

#[derive(Clone)]
pub enum Dialog {
    NewFolder(PathBuf),
    SidebarGroupName(u64),
    Settings,
    PermanentDelete(Vec<fs_model::RecycleItem>),
    OpenMultiple(Vec<PathBuf>),
    #[allow(dead_code)]
    Message {
        title: String,
        body: String,
    },
}

pub(crate) enum PreviewImageTool {
    Convert(PathBuf),
    Resize(PathBuf, Option<crate::image_edits::ResizeControls>),
}

impl PreviewImageTool {
    fn path(&self) -> &Path {
        match self {
            Self::Convert(path) | Self::Resize(path, _) => path,
        }
    }
}

#[derive(Clone)]
pub enum MenuTarget {
    Item(PathBuf),
    Background,
    Sidebar(PathBuf),
    Address(PathBuf),
    AddressAncestors(Vec<(String, PathBuf)>),
    SidebarFavorite(PathBuf),
    SidebarRecentFile(PathBuf),
    Tab(u64),
}

pub struct ContextMenu {
    pub id: u64,
    pub x: f32,
    pub y: f32,
    pub target: MenuTarget,
    pub pages: Vec<ContextPage>,
    pub submenu_transition: Option<SubmenuTransition>,
    pub scroll: gpui::ScrollHandle,
    pub reveal_expanded: std::cell::Cell<bool>,
    pub system_rows: Option<ShellRows>,
    pub submenus: HashMap<u32, ShellRows>,
    shell: Option<ShellClient>,
    loading: Option<gpui::Task<()>>,
}

pub struct SubmenuTransition {
    pub previous: Vec<ContextPage>,
    pub started: Instant,
}

impl ContextMenu {
    fn change_submenu(&mut self, depth: usize, page: Option<ContextPage>) {
        let previous = self.pages.clone();
        self.pages.truncate(depth);
        self.pages.extend(page);
        if previous != self.pages {
            self.submenu_transition = Some(SubmenuTransition { previous, started: Instant::now() });
            self.reveal_expanded.set(!self.pages.is_empty());
        }
    }
}

struct PreparedContextMenu {
    target: ShellTarget,
    started: Instant,
    shell: ShellClient,
    rows: oneshot::Receiver<ShellRows>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ContextPage {
    More,
    View,
    Sort,
    FavoriteColor,
    Shell(u32),
}

#[derive(Clone)]
pub enum MenuAction {
    Open(PathBuf),
    OpenLocation(PathBuf),
    OpenWith(PathBuf),
    OpenInNewTab(PathBuf),
    OpenInNewWindow(PathBuf),
    AddToWorkspace(PathBuf),
    EditAddress(PathBuf),
    ToggleCompactPaths,
    TogglePathBreadcrumbs,
    OpenInOtherPane(PathBuf),
    Properties(PathBuf),
    OpenTerminal(PathBuf),
    CopyPath(PathBuf),
    CopySelection,
    CutSelection,
    Paste,
    DeleteSelection,
    Rename(PathBuf),
    NewFolder,
    Refresh,
    SelectAll,
    SetSort(SortKey),
    ToggleSortDesc,
    SetView(ViewMode),
    ToggleHidden,
    ToggleSplit,
    TogglePreview,
    AddFavorite(PathBuf),
    RemoveFavorite(PathBuf),
    SetFavoriteColor(PathBuf, usize),
    RevealRecentFile(PathBuf),
    ClearRecentFiles,
    RestoreRecycle,
    EmptyRecycleBin,
    Settings,
    CloseTab,
    MoveTab(u64, isize),
    CloseTabFromMenu(u64),
    CloseOtherTabs(u64),
    CloseTabsToRight(u64),
    ContextSubmenu(ContextPage, usize),
    ShellCommand(u32),
    RotateImage(u32),
}

#[derive(Clone)]
pub struct MenuEntry {
    pub icon: &'static str,
    pub label: String,
    pub hint: &'static str,
    pub action: MenuAction,
    pub danger: bool,
    pub checked: bool,
    pub enabled: bool,
    pub image: Option<Arc<RenderImage>>,
}

#[derive(Clone)]
pub enum MenuRow {
    Item(MenuEntry),
    Separator,
}

impl MenuRow {
    pub fn item(
        icon: &'static str,
        label: impl Into<String>,
        hint: &'static str,
        action: MenuAction,
    ) -> Self {
        Self::Item(MenuEntry {
            icon,
            label: label.into(),
            hint,
            action,
            danger: false,
            checked: false,
            enabled: true,
            image: None,
        })
    }

    pub fn danger(
        icon: &'static str,
        label: impl Into<String>,
        hint: &'static str,
        action: MenuAction,
    ) -> Self {
        Self::Item(MenuEntry {
            icon,
            label: label.into(),
            hint,
            action,
            danger: true,
            checked: false,
            enabled: true,
            image: None,
        })
    }

    pub fn checked(
        icon: &'static str,
        label: impl Into<String>,
        action: MenuAction,
        checked: bool,
    ) -> Self {
        Self::Item(MenuEntry {
            icon,
            label: label.into(),
            hint: "",
            action,
            danger: false,
            checked,
            enabled: true,
            image: None,
        })
    }
}

pub struct Tab {
    pub id: u64,
    pub path: PathBuf,
    pub history: Vec<PathBuf>,
    pub hist_index: usize,
    pub entries: Vec<Entry>,
    pub visible: Vec<usize>,
    pub selection: BTreeSet<PathBuf>,
    pub anchor: Option<PathBuf>,
    pub cursor: Option<PathBuf>,
    pub scroll: gpui::UniformListScrollHandle,
    pub loading: bool,
    pub error: Option<String>,
    pub sort: SortKey,
    pub sort_desc: bool,
    pub view: ViewMode,
    pub icon_scale: f32,
    pub compact: bool,
    pub detail_widths: [f32; DETAIL_COLUMNS],
    pub folder_counts: HashMap<PathBuf, Option<usize>>,
    pub search_query: String,
    pub recursive_search: bool,
    pub show_folders: bool,
    pub show_files: bool,
    pub scan_token: u64,
    needs_scan: bool,
    tree_root_count: Option<usize>,
    pub tree_expanded: HashSet<PathBuf>,
    tree_requests: HashMap<PathBuf, u64>,
    next_tree_request: u64,
    thumbnail_range: std::ops::Range<usize>,
    thumbnail_task: Option<gpui::Task<()>>,
}

impl Tab {
    fn saved_view(&self) -> crate::pane_layout::SavedTabView {
        crate::pane_layout::SavedTabView {
            view: self.view,
            icon_scale: self.icon_scale,
            compact: self.compact,
            sort: self.sort,
            sort_desc: self.sort_desc,
            detail_widths: self.detail_widths,
        }
    }

    fn restore_view(&mut self, saved: &crate::pane_layout::SavedTabView) {
        self.view = saved.view;
        self.icon_scale = saved.icon_scale.clamp(1.0, 2.0);
        self.compact = saved.compact;
        self.sort = saved.sort;
        self.sort_desc = saved.sort_desc;
        self.detail_widths = saved.detail_widths;
    }

    fn new(id: u64, path: PathBuf, settings: &Settings) -> Self {
        let saved = settings.folder_view(&path);
        Self {
            id,
            history: vec![path.clone()],
            hist_index: 0,
            path,
            entries: Vec::new(),
            visible: Vec::new(),
            selection: BTreeSet::new(),
            anchor: None,
            cursor: None,
            scroll: gpui::UniformListScrollHandle::new(),
            loading: true,
            error: None,
            sort: saved.map_or(settings.sort, |view| view.sort),
            sort_desc: saved.map_or(settings.sort_descending, |view| view.descending),
            view: saved.map_or(settings.view, |view| view.view),
            icon_scale: saved.map_or(settings.icon_scale, |view| view.scale),
            compact: saved.map_or(settings.compact_density, |view| view.compact),
            detail_widths: settings.detail_widths,
            folder_counts: HashMap::new(),
            search_query: String::new(),
            recursive_search: false,
            show_folders: true,
            show_files: true,
            scan_token: 0,
            needs_scan: true,
            tree_root_count: None,
            tree_expanded: HashSet::new(),
            tree_requests: HashMap::new(),
            next_tree_request: 0,
            thumbnail_range: 0..0,
            thumbnail_task: None,
        }
    }

    fn reset_thumbnails(&mut self) {
        self.thumbnail_task = None;
        self.thumbnail_range = 0..0;
    }

    fn change_path(&mut self, path: PathBuf) {
        if self.path != path {
            self.clear_tree();
            self.entries.clear();
            self.visible.clear();
            self.scroll = gpui::UniformListScrollHandle::new();
            self.reset_thumbnails();
        }
        self.path = path;
    }

    fn clear_tree(&mut self) {
        if let Some(count) = self.tree_root_count.take() {
            self.entries.truncate(count);
        }
        self.tree_expanded.clear();
        self.tree_requests.clear();
    }

    fn collapse_tree_folder(&mut self, path: &Path) {
        self.tree_expanded.retain(|expanded| !expanded.starts_with(path));
        self.tree_requests.retain(|pending, _| !pending.starts_with(path));
        if let Some(roots) = self.tree_root_count {
            let mut index = 0;
            self.entries.retain(|entry| {
                let retain = index < roots || !entry.path.starts_with(path);
                index += 1;
                retain
            });
        }
    }

    pub fn root_entries(&self) -> &[Entry] {
        &self.entries[..self.tree_root_count.unwrap_or(self.entries.len())]
    }

    fn sort_entries(&mut self) {
        let roots = self.root_entries().len();
        let (roots, children) = self.entries.split_at_mut(roots);
        fs_model::sort_entries(roots, self.sort, self.sort_desc);
        fs_model::sort_entries(children, self.sort, self.sort_desc);
    }

    pub fn tree_depth(&self, entry: &Entry) -> usize {
        entry.path.parent()
            .and_then(|parent| parent.strip_prefix(&self.path).ok())
            .map_or(0, |relative| relative.components().count())
    }

    fn refresh_filter(&mut self, ignore_accents: bool) {
        let query = fs_model::search_text(self.search_query.trim(), ignore_accents);
        let show_folders = self.show_folders;
        let show_files = self.show_files;
        self.reset_thumbnails();
        self.visible = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| fs_model::matches_query(entry, &query, ignore_accents))
            .filter(|(_, entry)| {
                if entry.is_dir {
                    show_folders
                } else {
                    show_files
                }
            })
            .map(|(index, _)| index)
            .collect();

        if let Some(root_count) = self.tree_root_count {
            let mut children = HashMap::<&Path, Vec<usize>>::new();
            let mut roots = Vec::new();
            for index in self.visible.drain(..) {
                if index < root_count {
                    roots.push(index);
                } else if let Some(parent) = self.entries[index].path.parent() {
                    children.entry(parent).or_default().push(index);
                }
            }
            roots.reverse();
            while let Some(index) = roots.pop() {
                self.visible.push(index);
                let path = self.entries[index].path.as_path();
                if self.tree_expanded.contains(path) && let Some(branch) = children.get(path) {
                    roots.extend(branch.iter().rev().copied());
                }
            }
        }

        if self.selection.is_empty() && self.anchor.is_none() && self.cursor.is_none() {
            return;
        }
        let visible_paths: HashSet<&Path> = self
            .visible
            .iter()
            .filter_map(|index| self.entries.get(*index))
            .map(|entry| entry.path.as_path())
            .collect();
        self.selection.retain(|path| visible_paths.contains(path.as_path()));
        if let Some(anchor) = self.anchor.as_ref()
            && !visible_paths.contains(anchor.as_path())
        {
            self.anchor = None;
        }
        if let Some(cursor) = self.cursor.as_ref()
            && !visible_paths.contains(cursor.as_path())
        {
            self.cursor = None;
        }
    }

    pub fn selected_paths(&self) -> Vec<PathBuf> {
        self.selection.iter().cloned().collect()
    }

    pub fn primary_path(&self) -> Option<PathBuf> {
        if self.selection.len() == 1 {
            return self.selection.iter().next().cloned();
        }
        if let Some(cursor) = self.cursor.as_ref()
            && self.selection.contains(cursor)
        {
            return Some(cursor.clone());
        }
        self.selection.iter().next().cloned()
    }

    pub fn entry_for(&self, path: &Path) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.path == path)
    }

    pub fn visible_index_of(&self, path: &Path) -> Option<usize> {
        self.visible
            .iter()
            .position(|index| self.entries.get(*index).is_some_and(|entry| entry.path == path))
    }

    pub fn selected_count(&self) -> usize {
        self.selection.len()
    }

    pub fn selected_size(&self) -> u64 {
        self.entries
            .iter()
            .filter(|entry| self.selection.contains(&entry.path) && !entry.is_dir)
            .map(|entry| entry.size)
            .sum()
    }
}

pub struct Pane {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub tab_scroll: gpui::ScrollHandle,
}

impl Pane {
    fn new(tab: Tab) -> Self {
        Self::with_tabs(vec![tab], 0)
    }

    fn with_tabs(tabs: Vec<Tab>, active: usize) -> Self {
        let tab_scroll = gpui::ScrollHandle::new();
        tab_scroll.scroll_to_item(active);
        Self { tabs, active, tab_scroll }
    }

    pub fn active_tab(&self) -> &Tab {
        &self.tabs[self.active.min(self.tabs.len().saturating_sub(1))]
    }

    pub fn active_tab_mut(&mut self) -> &mut Tab {
        let index = self.active.min(self.tabs.len().saturating_sub(1));
        self.active = index;
        &mut self.tabs[index]
    }
}

pub struct QuickItem {
    pub label: String,
    pub path: PathBuf,
    pub glyph: &'static str,
    pub color: u32,
    pub icon: Option<Arc<RenderImage>>,
    /// Acciones que no corresponden a una carpeta real del disco.
    pub action: Option<QuickAction>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuickAction {
    RecycleBin,
}

struct AudioSeekDrag {
    bounds: Bounds<Pixels>,
    ratio: f32,
    was_playing: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WheelScrollTarget {
    Files(u64),
    SettingsPage(usize),
    SettingsNavigation,
}

#[derive(Clone, Copy)]
struct WheelScrollMotion {
    target: WheelScrollTarget,
    axis: gpui::Axis,
    started: Instant,
    from: Pixels,
    to: Pixels,
}

pub struct XoolFiles {
    pub(crate) recently_closed_open: bool,
    recent_files_generation: u64,
    pub(crate) marquee_layouts: HashMap<u64, crate::marquee::Layout>,
    pub(crate) marquee: Option<crate::marquee::Selection>,
    pub settings: Settings,
    pub(crate) workspace_ui: crate::workspaces::PanelState,
    /// Escala que se está dibujando ahora mismo. La configuración guarda el
    /// destino y esta se interpola para que el zoom se mueva con suavidad.
    pub icon_scale_display: f32,
    zoom_animation: Option<(Instant, f32)>,
    pub panes: Vec<Pane>,
    pub pane_layout: PaneLayout,
    pub active_pane: usize,
    pub focus_handle: FocusHandle,
    pub address: Entity<InputState>,
    pub search: Entity<InputState>,
    pub dialog_input: Entity<InputState>,
    pub creating_sidebar_group: bool,
    pub settings_search: Entity<InputState>,
    pub(crate) tab_gradient_reveal: crate::motion::SettingsReveal,
    pub(crate) tab_close_reveal: crate::motion::SettingsReveal,
    pub shortcut_search: Entity<InputState>,
    quick_window: Option<crate::quick_search::QuickSearchWindow>,
    pub(crate) search_index: Arc<crate::search_index::SearchIndex>,
    quick_hotkey_task: Option<gpui::Task<()>>,
    search_index_task: Option<gpui::Task<()>>,
    pub folder_default: bool,
    pub(crate) windows_startup: bool,
    tray_task: Option<gpui::Task<()>>,
    pub integration_message: Option<String>,
    pub quick_search_message: Option<String>,
    pub settings_nav_drag: Option<(f32, f32)>,
    pub settings_nav_collapsed: bool,
    pub settings_nav_progress: f32,
    settings_nav_animation: Option<(Instant, f32)>,
    wheel_scroll: Option<WheelScrollMotion>,
    /// Nombre que se está editando directamente en la cuadrícula o la lista.
    pub rename_path: Option<PathBuf>,
    pub rename_tab: Option<u64>,
    pub rename_input: Entity<InputState>,
    pub address_editing: bool,
    pub address_bounds: Option<Bounds<Pixels>>,
    pub address_history_open: bool,
    pub(crate) address_history_selection: Option<usize>,
    pub(crate) address_history_scroll: gpui::ScrollHandle,
    pub sidebar_resizing: bool,
    pub sidebar_group_target: Option<(crate::settings::SidebarGroup, usize)>,
    pub sidebar_favorite_target: Option<(usize, usize)>,
    sidebar_favorite_offsets: HashMap<PathBuf, (Instant, f32)>,
    pub(crate) menu_tool_target: Option<(usize, usize)>,
    menu_tool_offsets: HashMap<String, (Instant, f32)>,
    sidebar_group_offsets: HashMap<crate::settings::SidebarGroup, (Instant, f32)>,
    sidebar_group_animations: HashMap<crate::settings::SidebarGroup, (Instant, f32)>,
    pub sidebar_drive_nodes: HashMap<PathBuf, Option<Vec<PathBuf>>>,
    sidebar_progress: f32,
    sidebar_animation: Option<(Instant, f32)>,
    pub preview_resizing: bool,
    split_resize: Option<SplitResize>,
    pub column_resize: Option<ColumnResize>,
    pub columns_menu: Option<(f32, f32)>,
    pub(crate) detail_label_menu: Option<(PathBuf, DetailColumn, f32, f32)>,
    pub(crate) detail_label_arrival: Option<(usize, PathBuf, PathBuf, DetailColumn)>,
    pub column_drag_target: Option<(DetailColumn, usize)>,
    column_offsets: HashMap<DetailColumn, (Instant, f32)>,
    pub tab_reorder_target: Option<(u64, u64)>,
    tab_reorder_offsets: HashMap<u64, (Instant, f32)>,
    pub tab_drop_zone: Option<TabDropZone>,
    pub tab_drop_pane: Option<usize>,
    pub folder_drop_bounds: Option<Bounds<Pixels>>,
    pub(crate) native_drag_task: Option<gpui::Task<()>>,
    folder_drag: Option<(Option<u64>, PathBuf, gpui::Task<()>)>,
    file_drag_tab: Option<(u64, gpui::Task<()>)>,
    audio_seek_drag: Option<AudioSeekDrag>,
    pub video_seek_dragging: bool,
    pub video_seek_bounds: Option<Bounds<Pixels>>,
    media_volume_bounds: Option<Bounds<Pixels>>,
    pub preview: Option<PreviewContent>,
    pub(crate) text_edit: Option<(PathBuf, String, Entity<InputState>)>,
    file_undo: Vec<Vec<FileUndo>>,
    undo_in_progress: bool,

    pub preview_path: Option<PathBuf>,
    pub preview_loading: bool,
    pub(crate) image_editing: bool,
    pub(crate) preview_image_tool: Option<PreviewImageTool>,
    image_resize_generation: u64,
    preview_fits: bool,
    pub preview_token: u64,
    pub audio: Option<AudioPlayback>,
    pub video: Option<VideoPlayer>,
    /// Texturas que ya no se dibujan y deben retirarse del atlas de GPUI.
    pub image_drops: Vec<Arc<RenderImage>>,
    pub(crate) background_image: Option<Arc<RenderImage>>,
    background_image_task: Option<gpui::Task<()>>,
    pending_background_path: Option<PathBuf>,
    background_uses_acrylic: bool,
    context_menu_icons: VecDeque<Arc<RenderImage>>,
    pub context_menu: Option<ContextMenu>,
    context_menu_id: u64,
    menu_hover: Option<(ShellTarget, gpui::Task<()>)>,
    prepared_menu: Option<PreparedContextMenu>,
    pub sort_menu: bool,
    pub view_menu: bool,
    pub dialog: Option<Dialog>,
    pub preferences_page: usize,
    pub settings_animation: u64,
    pub settings_page_scroll: gpui::ScrollHandle,
    pub settings_nav_scroll: gpui::ScrollHandle,
    pub settings_closing: bool,
    pub settings_theme_open: bool,
    pub storage_info: Option<StorageInfo>,
    pub toast: Option<Toast>,
    pub toast_token: u64,
    pub quick: Vec<QuickItem>,
    pub recent_icons: HashMap<PathBuf, Option<Arc<RenderImage>>>,
    pub drives: Vec<DriveInfo>,
    cut_clipboard: Option<(u32, HashSet<PathBuf>)>,
    drive_usage_task: Option<gpui::Task<()>>,
    drive_monitor_task: Option<gpui::Task<()>>,
    pub next_tab_id: u64,
    /// Acción cuyo atajo se está grabando desde Ajustes.
    pub recording_shortcut: Option<&'static str>,
    /// Combinaciones que están aplicadas ahora mismo en el mapa de teclas.
    pub applied_shortcuts: HashMap<String, String>,
    pub watcher: Option<Box<dyn Watcher>>,
    pub watched_paths: Vec<PathBuf>,
    watch_retry_at: Option<Instant>,
    pub _subscriptions: Vec<Subscription>,
}

#[derive(Clone)]
pub struct DragFiles {
    pub paths: Arc<[PathBuf]>,
    pub folder: bool,
}

/// Pestaña que se está arrastrando desde la barra de título.
#[derive(Clone)]
pub struct DragTab {
    pub tab_id: u64,
    pub label: String,
}

/// Zona de la carpeta actual donde se suelta una pestaña arrastrada.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabDropZone {
    Left,
    Center,
    Right,
    Top,
    Bottom,
}

impl TabDropZone {
    fn split(self) -> Option<(Axis, bool)> {
        match self {
            Self::Left => Some((Axis::Across, true)),
            Self::Right => Some((Axis::Across, false)),
            Self::Top => Some((Axis::Down, true)),
            Self::Bottom => Some((Axis::Down, false)),
            Self::Center => None,
        }
    }

    pub fn fits(self, width: f32, height: f32) -> bool {
        self.split().is_some_and(|(axis, _)| axis.can_split(width, height))
    }
}

struct SplitResize {
    branch: Vec<bool>,
    axis: Axis,
    start: f32,
    extent: f32,
    ratio: f32,
}

#[derive(Clone, Debug, Default)]
pub struct StorageInfo {
    pub total: u64,
    pub free: u64,
    pub app: u64,
    pub settings: u64,
    pub cache: u64,
    pub cache_files: usize,
    pub cache_path: Option<PathBuf>,
}

pub struct ColumnResize {
    pub tab_id: u64,
    pub index: usize,
    x: f32,
    width: f32,
    max_width: f32,
}

const DETAIL_NAME_RESERVE: f32 = 146.0;
const DETAIL_COLUMN_GAP: f32 = 10.0;
pub(crate) fn detail_widths(settings: &Settings, preferred: [f32; DETAIL_COLUMNS], pane_width: f32) -> [f32; DETAIL_COLUMNS] {
    let mut remaining = pane_width - DETAIL_NAME_RESERVE;
    let mut widths = [0.; DETAIL_COLUMNS];
    for column in settings.detail_column_order.iter().rev() {
        let index = column.index();
        let minimum = DetailColumn::MIN_WIDTHS[index];
        let threshold = match column {
            DetailColumn::Modified => 420.,
            DetailColumn::Kind => 590.,
            DetailColumn::Size => 275.,
            DetailColumn::Status => 550.,
            DetailColumn::Priority => 650.,
            _ => 0.,
        };
        if settings.detail_column_visible[index] && pane_width >= threshold && remaining >= minimum + DETAIL_COLUMN_GAP {
            widths[index] = minimum;
            remaining -= minimum + DETAIL_COLUMN_GAP;
        }
    }
    for column in settings.detail_column_order.iter().rev() {
        let index = column.index();
        if widths[index] > 0. {
            let extra = (preferred[index] - widths[index]).clamp(0., remaining);
            widths[index] += extra;
            remaining -= extra;
        }
    }
    widths
}

#[cfg(test)]
mod detail_width_tests {
    use super::*;

    #[test]
    fn details_keep_space_for_the_name_and_hide_columns_on_narrow_panes() {
        let settings = Settings::default();
        let defaults = DetailColumn::DEFAULT_WIDTHS;
        assert_eq!(detail_widths(&settings, defaults, 900.), [138., 142., 90., 0., 0., 0., 106., 92.]);
        assert_eq!(detail_widths(&settings, defaults, 410.), [0., 0., 90., 0., 0., 0., 0., 0.]);
        let widths = detail_widths(&settings, [1000.; DETAIL_COLUMNS], 590.);
        assert!(widths[..3].iter().zip(DetailColumn::MIN_WIDTHS).all(|(width, minimum)| *width >= minimum));
        let gaps = widths.iter().filter(|&&width| width > 0.).count() as f32;
        assert!(widths.iter().sum::<f32>() + gaps * DETAIL_COLUMN_GAP + DETAIL_NAME_RESERVE <= 590.);
    }
}


impl XoolFiles {
    pub fn new(mut settings: Settings, initial_folder: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        use crate::settings::StartupMode;
        if settings.workspaces.enabled && !settings.temporary_window {
            settings.workspaces.selected = settings.workspaces.startup_index();
        }
        let mode = settings.startup_mode();
        let saved_session = settings.pane_session.as_ref().filter(|session| {
            mode == StartupMode::LastSession && !session.panes.is_empty()
                && session.layout.valid_for(session.panes.len())
        }).cloned();
        let initial_icon_scale = settings.icon_scale;
        let mut quick = platform::known_folders()
            .into_iter()
            .enumerate()
            .map(|(index, (label, path))| QuickItem {
                label,
                path,
                glyph: quick_glyph(index),
                color: quick_color(index),
                icon: None,
                action: None,
            })
            .collect::<Vec<_>>();
        // La Papelera es un espacio de nombres del Shell, no una carpeta
        // normal: se abre con `shell:RecycleBinFolder`.
        quick.push(QuickItem {
            label: tr("Papelera", "Recycle Bin").to_string(),
            path: PathBuf::from("shell:RecycleBinFolder"),
            glyph: "\u{E74D}",
            color: 0x9aa7b8,
            icon: None,
            action: Some(QuickAction::RecycleBin),
        });

        let home = quick
            .first()
            .map(|item| item.path.clone())
            .unwrap_or_else(|| PathBuf::from("C:\\"));

        let session = if let Some(path) = initial_folder {
            PaneSession {
                layout: PaneLayout::Leaf(0),
                panes: vec![SavedPane { paths: vec![path], active: 0, views: Vec::new() }],
                active: 0,
            }
        } else { saved_session.unwrap_or_else(|| {
            if mode != StartupMode::LastSession {
                let paths = if mode == StartupMode::Folders { settings.startup_folders.clone() } else { Vec::new() };
                return PaneSession {
                    layout: PaneLayout::Leaf(0),
                    panes: vec![SavedPane { paths, active: 0, views: Vec::new() }],
                    active: 0,
                };
            }
            let mut panes = vec![SavedPane { paths: settings.last_left.clone(), active: settings.active_left, views: Vec::new() }];
            let mut layout = PaneLayout::Leaf(0);
            if settings.split {
                panes.push(SavedPane { paths: settings.last_right.clone(), active: settings.active_right, views: Vec::new() });
                let axis = if settings.split_vertical { Axis::Down } else { Axis::Across };
                layout = PaneLayout::pair(axis, settings.split_ratio, layout, PaneLayout::Leaf(1));
            }
            PaneSession { layout, panes, active: 0 }
        }) };
        let fallback = session.panes[0].paths.iter()
            .find(|path| path.is_dir() || fs_model::is_recycle_bin(path))
            .unwrap_or(&home).clone();
        let mut next_tab_id = 1;
        let mut panes = Vec::with_capacity(session.panes.len());
        for saved in session.panes {
            let paths = restored_tabs(&saved.paths[..saved.paths.len().min(32)], &fallback);
            let active = paths.iter().rposition(|(index, _)| index.is_some_and(|index| index <= saved.active)).unwrap_or(0);
            let tabs = paths.into_iter().map(|(index, path)| {
                record_recent(&mut settings.recent_paths, &path);
                let mut tab = Tab::new(next_tab_id, path, &settings);
                if let Some(view) = index.and_then(|index| saved.views.get(index)) {
                    tab.restore_view(view);
                }
                next_tab_id += 1;
                tab
            }).collect();
            panes.push(Pane::with_tabs(tabs, active));
        }
        let active_pane = session.active.min(panes.len() - 1);
        let pane_layout = session.layout;

        let address = cx.new(|cx| {
            InputState::new(window, cx).placeholder(tr("Escribe una ruta y pulsa Enter…", "Type a path and press Enter…"))
        });
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder(tr("Buscar en esta carpeta…", "Search this folder…"))
        });
        let settings_search = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Buscar en Ajustes", "Search Settings")));
        let shortcut_search = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Buscar acción o combinación", "Search for an action or shortcut")));
        let shortcut_search_sub = cx.subscribe(&shortcut_search, |this: &mut Self, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.recording_shortcut = None;
                cx.notify();
            }
        });
        let settings_search_sub = cx.subscribe(&settings_search, |_, _, event, cx| {
            if matches!(event, InputEvent::Change) { cx.notify(); }
        });
        let dialog_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(tr("Escribe un nombre…", "Enter a name…"))
        });
        let rename_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(tr("Nombre del elemento…", "Item name…"))
        });

        let address_sub = cx.subscribe_in(&address, window, |this, input, event, window, cx| {
            if matches!(event, InputEvent::Change) && this.address_editing {
                this.address_history_selection = None;
                this.address_history_open = true;
                cx.notify();
            }
            if matches!(event, InputEvent::PressEnter { .. }) {
                if this.address_history_open
                    && let Some(path) = this.address_history_selection.take()
                        .and_then(|index| this.settings.recent_paths.get(index).cloned())
                {
                    this.navigate_recent(path, window, cx);
                } else {
                    this.commit_address(input.read(cx).value().to_string(), window, cx);
                }
            }
        });
        let search_sub = cx.subscribe_in(&search, window, |this, _input, event, window, cx| {
            match event {
                InputEvent::Change => {
                    let value = _input.read(cx).value().to_string();
                    let tab = this.active_tab_mut();
                    if tab.search_query == value { return; }
                    tab.search_query = value;
                    if tab.search_query.trim().is_empty() && tab.recursive_search {
                        tab.recursive_search = false;
                        this.start_scan_active(cx);
                    } else {
                        this.apply_filter(cx);
                    }
                }
                InputEvent::PressEnter { .. } => {
                    this.start_recursive_search(cx);
                }
                _ => {}
            }
            let _ = window;
        });
        let dialog_sub = cx.subscribe_in(&dialog_input, window, |this, _input, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                if this.creating_sidebar_group && matches!(this.dialog, Some(Dialog::Settings)) {
                    this.create_sidebar_group(window, cx);
                } else {
                    this.submit_dialog(window, cx);
                }
            }
        });
        let rename_sub = cx.subscribe_in(&rename_input, window, |this, _input, event, window, cx| {
            match event {
                InputEvent::PressEnter { .. } => this.commit_inline_rename(window, cx),
                InputEvent::Blur => this.cancel_inline_rename(window, cx),
                _ => {}
            }
        });

        let activation_sub = cx.observe_window_activation(window, |this, window, cx| {
            if !window.is_window_active() {
                this.end_workspace_resize(cx);
                let had_marquee = this.marquee.take().is_some();
                if std::mem::take(&mut this.recently_closed_open) || had_marquee { cx.notify(); }
                this.end_audio_seek_drag(None, cx);
                this.end_media_volume_drag(cx);
                this.menu_hover = None;
                this.prepared_menu = None;
                if this.context_menu.is_some() { this.close_context_menu(cx); }
            } else if !this.drives.is_empty() {
                this.refresh_drive_usage(cx);
            }
        });
        let applied_shortcuts: HashMap<String, String> = shortcuts::SHORTCUTS
            .iter()
            .map(|def| (def.id.to_string(), settings.shortcut(def.id)))
            .collect();

        let sidebar_progress = if settings.sidebar_visible { 1.0 } else { 0.0 };
        let this = Self {
            workspace_ui: crate::workspaces::PanelState::new(&settings.workspaces, window, cx),
            search_index: Arc::new(crate::search_index::SearchIndex::new(settings.search_live_index)),
            tab_gradient_reveal: crate::motion::SettingsReveal::new(settings.active_tab_gradient),
            tab_close_reveal: crate::motion::SettingsReveal::new(settings.show_tab_close),
            settings,
            icon_scale_display: initial_icon_scale,
            zoom_animation: None,
            panes,
            pane_layout,
            active_pane,
            focus_handle: cx.focus_handle(),
            address,
            search,
            dialog_input,
            creating_sidebar_group: false,
            settings_search,
            shortcut_search,
            quick_window: None,
            quick_hotkey_task: None,
            search_index_task: None,
            folder_default: crate::integration::folder_default(),
            windows_startup: crate::integration::starts_with_windows().unwrap_or(false),
            tray_task: None,
            integration_message: None,
            quick_search_message: None,
            settings_nav_drag: None,
            settings_nav_collapsed: false,
            settings_nav_progress: 1.,
            settings_nav_animation: None,
            wheel_scroll: None,
            rename_path: None,
            rename_tab: None,
            rename_input,
            address_editing: false,
            address_bounds: None,
            address_history_open: false,
            address_history_selection: None,
            address_history_scroll: gpui::ScrollHandle::new(),
            sidebar_resizing: false,
            sidebar_group_target: None,
            sidebar_favorite_target: None,
            sidebar_favorite_offsets: HashMap::new(),
            menu_tool_target: None,
            menu_tool_offsets: HashMap::new(),
            sidebar_group_offsets: HashMap::new(),
            sidebar_group_animations: HashMap::new(),
            sidebar_drive_nodes: HashMap::new(),
            sidebar_progress,
            sidebar_animation: None,
            preview_resizing: false,
            split_resize: None,
            column_resize: None,
            columns_menu: None,
            detail_label_menu: None,
            detail_label_arrival: None,
            column_drag_target: None,
            column_offsets: HashMap::new(),
            tab_reorder_target: None,
            tab_reorder_offsets: HashMap::new(),
            tab_drop_zone: None,
            tab_drop_pane: None,
            folder_drop_bounds: None,
            native_drag_task: None,
            folder_drag: None,
            file_drag_tab: None,
            audio_seek_drag: None,
            video_seek_dragging: false,
            video_seek_bounds: None,
            media_volume_bounds: None,
            preview: None,
            text_edit: None,
            file_undo: Vec::new(),
            undo_in_progress: false,
            preview_path: None,
            preview_loading: false,
            image_editing: false,
            preview_image_tool: None,
            image_resize_generation: 0,
            preview_fits: f32::from(window.viewport_size().width) >= 760.0,
            preview_token: 0,
            audio: None,
            video: None,
            image_drops: Vec::new(),
            background_image: None,
            background_image_task: None,
            pending_background_path: None,
            background_uses_acrylic: false,
            context_menu_icons: VecDeque::new(),
            context_menu: None,
            context_menu_id: 0,
            menu_hover: None,
            prepared_menu: None,
            sort_menu: false,
            view_menu: false,
            recently_closed_open: false,
            recent_files_generation: 0,
            marquee_layouts: HashMap::new(),
            marquee: None,
            dialog: None,
            preferences_page: 1,
            settings_animation: 0,
            settings_page_scroll: gpui::ScrollHandle::new(),
            settings_nav_scroll: gpui::ScrollHandle::new(),
            settings_closing: false,
            settings_theme_open: false,
            storage_info: None,
            toast: None,
            toast_token: 0,
            quick,
            recent_icons: HashMap::new(),
            drives: Vec::new(),
            cut_clipboard: None,
            drive_usage_task: None,
            drive_monitor_task: None,
            next_tab_id,
            recording_shortcut: None,
            applied_shortcuts,
            watcher: None,
            watched_paths: Vec::new(),
            watch_retry_at: None,
            _subscriptions: vec![address_sub, search_sub, dialog_sub, rename_sub, activation_sub, settings_search_sub, shortcut_search_sub],
        };

        let path = this.active_tab().path.clone();
        this.address.update(cx, |state, cx| {
            state.set_value(address_text(&path), window, cx);
        });
        this
    }

    /// Avanza el reloj de audio y sincroniza el frame de vídeo pendiente.
    /// Devuelve true mientras haya reproducción activa.
    fn tick_media(&mut self) -> bool {
        let mut busy = false;
        if let Some(audio) = &mut self.audio {
            audio.tick();
            busy |= audio.playing;
        }
        if let Some(video) = &mut self.video {
            let playing = video.is_playing();
            if let Some(previous) = video.sync_frame() {
                self.image_drops.push(previous);
            }
            video.sync_audio();
            busy = busy || playing;
        }
        busy
    }

    pub fn start_initial_scans(&mut self, cx: &mut Context<Self>) {
        self.refresh_background_image(cx);
        self.restore_folder_view();
        self.start_watcher(cx);
        self.start_drive_monitor(cx);
        let weak = cx.entity().downgrade();
        let capture = cx.intercept_keystrokes(move |event, window, cx| {
            let mut handled = false;
            let _ = weak.update(cx, |this, cx| {
                if event.keystroke.key == "s" && event.keystroke.modifiers.control
                    && !event.keystroke.modifiers.shift && !event.keystroke.modifiers.alt
                    && this.dialog.is_none()
                    && this.text_edit.as_ref().is_some_and(|(_, _, input)| input.read(cx).focus_handle(cx).is_focused(window))
                {
                    this.save_preview_text(window, cx);
                    handled = true;
                } else if event.keystroke.key.eq_ignore_ascii_case("escape") && cx.has_active_drag() {
                    this.cancel_active_drag(window, cx);
                    handled = true;
                } else if this.recording_shortcut.is_some() {
                    handled = this.capture_shortcut(event, window, cx);
                } else if event.keystroke.key == "enter"
                    && !event.keystroke.modifiers.control
                    && !event.keystroke.modifiers.alt
                    && !event.keystroke.modifiers.platform
                    && this.rename_path.is_some()
                    && this.rename_input.read(cx).focus_handle(cx).is_focused(window)
                {
                    // Confirm before the input can also propagate Enter to OpenSelected.
                    this.commit_inline_rename(window, cx);
                    handled = true;
                } else if event.keystroke.key == "enter" && !event.keystroke.modifiers.modified()
                    && matches!(this.workspace_ui.edit, Some(crate::workspaces::NameEdit::Web(_)))
                    && this.workspace_ui.name.read(cx).focus_handle(cx).is_focused(window)
                {
                    let url = this.workspace_ui.name.read(cx).value().to_string();
                    this.commit_workspace_name(url, window, cx);
                    handled = true;
                } else if this.address_editing && this.address_history_open && this.dialog.is_none()
                    && this.address.read(cx).focus_handle(cx).is_focused(window)
                    && !event.keystroke.modifiers.modified()
                {
                    handled = match event.keystroke.key.as_str() {
                        "up" => this.move_address_history_selection(-1, cx),
                        "down" => this.move_address_history_selection(1, cx),
                        _ => false,
                    };
                } else if this.dialog.is_none() && this.search.read(cx).focus_handle(cx).is_focused(window)
                    && let Some(keys) = shortcuts::keystroke_to_string(&event.keystroke)
                    && shortcuts::normalize(&keys) == shortcuts::normalize(&this.settings.shortcut("recursive_search"))
                {
                    // El campo de texto también asigna Ctrl+E a mover el cursor.
                    this.toggle_recursive_search(window, cx);
                    handled = true;
                }
            });
            if handled {
                cx.stop_propagation();
            }
        });
        self._subscriptions.push(capture);
        let ids: Vec<u64> = self
            .panes
            .iter()
            .map(|pane| pane.active_tab().id)
            .collect();
        for id in ids {
            self.start_scan_tab(id, cx);
        }
        self.load_sidebar_resources(cx);
        self.load_recent_icons(cx);
    }

    pub fn load_recent_icons(&mut self, cx: &mut Context<Self>) {
        if !self.settings.sidebar_groups.contains(&crate::settings::SidebarGroup::Recent) { return; }
        self.recent_icons.retain(|path, _| self.settings.recent_file_paths.contains(path));
        let mut missing = Vec::new();
        for path in &self.settings.recent_file_paths {
            if !fs_model::is_recycle_bin(path) && let std::collections::hash_map::Entry::Vacant(slot) =
                self.recent_icons.entry(path.clone())
            {
                slot.insert(None);
                missing.push(path.clone());
            }
        }
        if missing.is_empty() { return; }

        cx.spawn(async move |this, cx| {
            let icons = cx.background_executor().spawn(async move {
                missing.into_iter().map(|path| {
                    let icon = platform::shell_icon(&path, false, 64);
                    (path, icon)
                }).collect::<Vec<_>>()
            }).await;
            let _ = this.update(cx, |this, cx| {
                for (path, icon) in icons {
                    if let Some(slot) = this.recent_icons.get_mut(&path) {
                        *slot = icon;
                    }
                }
                cx.notify();
            });
        }).detach();
    }

    fn load_sidebar_resources(&self, cx: &mut Context<Self>) {
        let folders: Vec<_> = self.quick.iter()
            .map(|item| (item.path.clone(), item.action))
            .collect();
        cx.spawn(async move |this, cx| {
            let icons = cx.background_executor().spawn(async move {
                folders.into_iter().map(|(path, action)| match action {
                    Some(QuickAction::RecycleBin) => platform::recycle_bin_icon(),
                    None => platform::shell_icon(&path, true, 64),
                }).collect::<Vec<_>>()
            }).await;
            let _ = this.update(cx, |this, cx| {
                for (item, icon) in this.quick.iter_mut().zip(icons) {
                    if item.icon.is_none() {
                        item.icon = icon;
                    }
                }
                cx.notify();
            });
        }).detach();
        cx.background_executor().spawn(async {
            platform::preload_menu_application_icons();
        }).detach();
    }

    // ------------------------------------------------------------------
    // Vigilancia de carpetas (auto-refresco)
    // ------------------------------------------------------------------

    fn active_watch_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = Vec::new();
        for pane in &self.panes {
            let path = pane.active_tab().path.clone();
            if !fs_model::is_recycle_bin(&path) && !paths.contains(&path) {
                paths.push(path);
            }
        }
        paths
    }

    fn start_watcher(&mut self, cx: &mut Context<Self>) {
        if self.watcher.is_some() {
            return;
        }
        let dirty = Arc::new(Mutex::new((HashSet::new(), false)));
        let callback_dirty = dirty.clone();
        let on_change = move |result: notify::Result<notify::Event>| {
            let Ok(mut dirty) = callback_dirty.lock() else { return };
            match result {
                Ok(event) if !event.kind.is_access() => {
                    dirty.1 |= event.need_rescan();
                    for path in event.paths {
                        if let Some(parent) = path.parent() {
                            dirty.0.insert(parent.to_path_buf());
                        }
                        dirty.0.insert(path);
                    }
                }
                Err(_) => dirty.1 = true,
                _ => {}
            }
        };
        self.watcher = notify::recommended_watcher(on_change.clone())
            .map(|watcher| Box::new(watcher) as Box<dyn Watcher>)
            .or_else(|_| PollWatcher::new(on_change, Config::default().with_poll_interval(Duration::from_secs(1)))
                .map(|watcher| Box::new(watcher) as Box<dyn Watcher>)).ok();
        self.sync_watcher();

        cx.spawn(async move |this, cx| {
            let mut pending = HashSet::new();
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(240))
                    .await;
                let (directories, rescan) = {
                    let Ok(mut dirty) = dirty.lock() else {
                        break;
                    };
                    std::mem::take(&mut *dirty)
                };
                if this.update(cx, |this, cx| {
                    this.sync_watcher();
                    if this.cut_clipboard.as_ref().is_some_and(|(sequence, _)| *sequence != platform::clipboard_sequence()) {
                        this.cut_clipboard = None;
                        cx.notify();
                    }
                    let mut ready = Vec::new();
                    for pane in &this.panes {
                        let tab = pane.active_tab();
                        if rescan || directories.contains(&tab.path) {
                            pending.insert(tab.id);
                        }
                        if !tab.loading && pending.remove(&tab.id) {
                            ready.push(tab.id);
                        }
                    }
                    pending.retain(|id| this.panes.iter().any(|pane| pane.active_tab().id == *id));
                    for id in ready { this.start_scan_tab(id, cx); }
                }).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Sincroniza las carpetas vigiladas con las pestañas activas.
    pub fn sync_watcher(&mut self) {
        let paths = self.active_watch_paths();
        if paths == self.watched_paths
            || self.watch_retry_at.is_some_and(|retry| Instant::now() < retry)
        {
            return;
        }
        let Some(watcher) = &mut self.watcher else {
            return;
        };
        self.watched_paths.retain(|path| {
            if paths.contains(path) { return true; }
            let _ = watcher.unwatch(path);
            false
        });
        self.watch_retry_at = None;
        for path in paths {
            if self.watched_paths.contains(&path) { continue; }
            if watcher.watch(&path, RecursiveMode::NonRecursive).is_ok() {
                self.watched_paths.push(path);
            } else {
                self.watch_retry_at = Some(Instant::now() + Duration::from_secs(2));
            }
        }
    }

    fn tabs_for_directories(&self, directories: &[PathBuf]) -> Vec<u64> {
        let mut ids: Vec<u64> = Vec::new();
        for pane in &self.panes {
            for tab in &pane.tabs {
                if directories.iter().any(|directory| directory == &tab.path || tab.tree_expanded.contains(directory))
                    && !ids.contains(&tab.id)
                {
                    ids.push(tab.id);
                }
            }
        }
        ids
    }

    // ------------------------------------------------------------------
    // Navegación con teclado
    // ------------------------------------------------------------------

    pub fn move_cursor(
        &mut self,
        delta: isize,
        extend: bool,
        keep_selection: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let columns = self.view_columns(window);

        let tab = self.active_tab_mut();
        let count = tab.visible.len();
        if count == 0 {
            return;
        }
        let current = tab
            .cursor
            .as_ref()
            .and_then(|path| {
                tab.visible.iter().position(|index| {
                    tab.entries
                        .get(*index)
                        .map(|entry| &entry.path == path)
                        .unwrap_or(false)
                })
            })
            .unwrap_or(0);
        let next = (current as isize + delta).clamp(0, count as isize - 1) as usize;

        let anchor = if extend {
            tab.anchor.clone().or_else(|| {
                tab.visible
                    .get(current)
                    .and_then(|index| tab.entries.get(*index))
                    .map(|entry| entry.path.clone())
            })
        } else {
            None
        };

        let next_path = tab
            .visible
            .get(next)
            .and_then(|index| tab.entries.get(*index))
            .map(|entry| entry.path.clone());
        let Some(next_path) = next_path else {
            return;
        };
        tab.cursor = Some(next_path.clone());
        tab.anchor = anchor;

        if extend {
            let start = if let Some(anchor) = &tab.anchor {
                tab.visible
                    .iter()
                    .position(|index| {
                        tab.entries
                            .get(*index)
                            .map(|entry| &entry.path == anchor)
                            .unwrap_or(false)
                    })
                    .unwrap_or(current)
            } else {
                current
            };
            let (low, high) = if start <= next {
                (start, next)
            } else {
                (next, start)
            };
            tab.selection = tab.visible[low..=high]
                .iter()
                .filter_map(|index| tab.entries.get(*index).map(|entry| entry.path.clone()))
                .collect();
        } else if !keep_selection {
            tab.selection.clear();
            tab.selection.insert(next_path);
        }

        let row = if columns > 1 { next / columns } else { next };
        tab.scroll
            .scroll_to_item(row, gpui::ScrollStrategy::Center);
        self.refresh_preview(cx);
        cx.notify();
    }

    pub fn toggle_cursor_selection(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        let Some(cursor) = tab.cursor.clone() else {
            return;
        };
        if !tab.selection.remove(&cursor) {
            tab.selection.insert(cursor.clone());
        }
        tab.anchor = Some(cursor);
        self.refresh_preview(cx);
        cx.notify();
    }

    pub fn cursor_to_edge(&mut self, end: bool, extend: bool, window: &Window, cx: &mut Context<Self>) {
        let columns = self.view_columns(window);
        let tab = self.active_tab_mut();
        let count = tab.visible.len();
        if count == 0 {
            return;
        }
        let target = if end { count - 1 } else { 0 };
        let path = tab
            .visible
            .get(target)
            .and_then(|index| tab.entries.get(*index))
            .map(|entry| entry.path.clone());
        let Some(path) = path else {
            return;
        };
        if extend {
            let anchor = tab.anchor.clone().or_else(|| tab.cursor.clone());
            tab.anchor = anchor;
            let start = tab
                .anchor
                .as_ref()
                .and_then(|anchor| {
                    tab.visible.iter().position(|index| {
                        tab.entries
                            .get(*index)
                            .map(|entry| &entry.path == anchor)
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(0);
            let (low, high) = if start <= target {
                (start, target)
            } else {
                (target, start)
            };
            tab.selection = tab.visible[low..=high]
                .iter()
                .filter_map(|index| tab.entries.get(*index).map(|entry| entry.path.clone()))
                .collect();
        } else {
            tab.selection.clear();
            tab.selection.insert(path.clone());
        }
        tab.cursor = Some(path);
        let row = if columns > 1 { target / columns } else { target };
        tab.scroll.scroll_to_item(row, gpui::ScrollStrategy::Center);
        self.refresh_preview(cx);
        cx.notify();
    }

    fn view_columns(&self, window: &Window) -> usize {
        let (_, available) = self.files_area(window);
        let pane_w = self.pane_layout.pane_width(self.active_pane, available).unwrap_or(available);
        match self.settings.view {
            ViewMode::Grid => {
                let icon_size = icon_size_for_scale(self.icon_scale_display);
                let min_cell = (icon_size + 46.0).max(96.0);
                let inner_w = (pane_w - 24.0).max(120.0);
                ((inner_w / min_cell).floor() as usize).max(1)
            }
            ViewMode::List => compact_list_columns(pane_w),
            _ => 1,
        }
    }

    // ------------------------------------------------------------------
    // Arrastrar y soltar
    // ------------------------------------------------------------------

    pub fn drop_files(
        &mut self,
        paths: Vec<PathBuf>,
        destination: PathBuf,
        copy: bool,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() || !destination.is_dir() {
            return;
        }
        let undo = if !copy && self.settings.undo_file_operations {
            FileUndo::moves(&paths, &destination)
        } else { Vec::new() };
        let result = if copy {
            platform::copy_items(&paths, &destination)
        } else {
            platform::move_items(&paths, &destination)
        };
        self.record_file_undo(undo.into_iter().filter(|step|
            !step.to.exists() && platform::file_identity(&step.from).ok() == Some(step.identity)).collect());
        match result {
            Ok(0) => self.toast(tr("Los elementos ya están en esta carpeta.", "The items are already in this folder."), ToastTone::Info, cx),
            Ok(count) => {
                let label = if count == 1 {
                    tr("1 elemento", "1 item").to_string()
                } else {
                    crate::localized!(format, "{count} elementos", "{count} items")
                };
                self.toast(
                    if copy {
                        crate::localized!(format, "{label} copiado(s)", "{label} copied")
                    } else {
                        crate::localized!(format, "{label} movido(s)", "{label} moved")
                    },
                    ToastTone::Success,
                    cx,
                );
                self.refresh_directories(&transfer_directories(&paths, &destination, !copy), cx);
            }
            Err(error) => self.toast(error, ToastTone::Error, cx),
        }
        cx.notify();
    }

    pub fn handle_internal_drop(
        &mut self,
        files: &DragFiles,
        destination: PathBuf,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let copy = window.modifiers().shift;
        self.drop_files(files.paths.to_vec(), destination, copy, cx);
    }

    pub fn handle_external_drop(
        &mut self,
        files: &gpui::ExternalPaths,
        destination: PathBuf,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let copy = window.modifiers().shift;
        self.drop_files(files.paths().to_vec(), destination, copy, cx);
    }

    pub(crate) fn persist_on_close(&mut self, window: &Window) {
        if let Some(placement) = platform::window_placement(window) {
            self.settings.window_placement = Some(placement);
        }
        self.persist();
    }

    pub fn persist(&mut self) {
        self.release_inactive_thumbnails();
        if let Some(tab) = self.panes.first().and_then(|pane| pane.tabs.get(pane.active)) {
            self.settings.sort = tab.sort;
            self.settings.sort_descending = tab.sort_desc;
        }
        self.settings.last_left = self
            .panes
            .first()
            .map(|pane| pane.tabs.iter().map(|tab| tab.path.clone()).collect())
            .unwrap_or_default();
        self.settings.active_left = self.panes.first().map(|pane| pane.active).unwrap_or(0);
        self.settings.last_right = self
            .panes
            .get(1)
            .map(|pane| pane.tabs.iter().map(|tab| tab.path.clone()).collect())
            .unwrap_or_default();
        self.settings.active_right = self.panes.get(1).map(|pane| pane.active).unwrap_or(0);
        self.settings.split = self.panes.len() > 1;
        self.settings.last_split = self.settings.split;
        if let PaneLayout::Split { axis, ratio, .. } = &self.pane_layout {
            self.settings.split_vertical = *axis == Axis::Down;
            self.settings.split_ratio = *ratio;
        } else {
            self.settings.split_vertical = false;
        }
        self.settings.pane_session = Some(PaneSession {
            layout: self.pane_layout.clone(),
            panes: self.panes.iter().map(|pane| SavedPane {
                paths: pane.tabs.iter().map(|tab| tab.path.clone()).collect(),
                active: pane.active,
                views: pane.tabs.iter().map(Tab::saved_view).collect(),
            }).collect(),
            active: self.active_pane,
        });
        self.settings.save();
    }

    /// Guarda una ruta en el historial reciente (sin duplicados, máximo 12).
    fn remember_path(&mut self, path: &Path, _cx: &mut Context<Self>) {
        record_recent(&mut self.settings.recent_paths, path);
    }

    pub fn active_pane(&self) -> &Pane {
        &self.panes[self.active_pane.min(self.panes.len().saturating_sub(1))]
    }

    pub fn active_pane_mut(&mut self) -> &mut Pane {
        let index = self.active_pane.min(self.panes.len().saturating_sub(1));
        self.active_pane = index;
        &mut self.panes[index]
    }

    pub fn active_tab(&self) -> &Tab {
        self.active_pane().active_tab()
    }

    pub fn active_tab_mut(&mut self) -> &mut Tab {
        self.active_pane_mut().active_tab_mut()
    }

    pub fn activate_pane(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.panes.len() {
            return;
        }
        self.active_pane = index;
        let path = self.active_tab().path.clone();
        self.address.update(cx, |state, cx| {
            state.set_value(address_text(&path), window, cx);
        });
        self.restore_folder_view();
        self.sync_search_field(window, cx);
        self.refresh_preview(cx);
        self.release_inactive_thumbnails();
        self.focus_root(window);
        cx.notify();
    }

    pub fn tab_by_id_mut(&mut self, id: u64) -> Option<&mut Tab> {
        for pane in &mut self.panes {
            if let Some(tab) = pane.tabs.iter_mut().find(|tab| tab.id == id) {
                return Some(tab);
            }
        }
        None
    }

    pub fn tab_by_id(&self, id: u64) -> Option<&Tab> {
        for pane in &self.panes {
            if let Some(tab) = pane.tabs.iter().find(|tab| tab.id == id) {
                return Some(tab);
            }
        }
        None
    }

    pub(crate) fn tab_position(&self, id: u64) -> Option<(usize, usize)> {
        for (pane_index, pane) in self.panes.iter().enumerate() {
            if let Some(tab_index) = pane.tabs.iter().position(|tab| tab.id == id) {
                return Some((pane_index, tab_index));
            }
        }
        None
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        id
    }

    // ------------------------------------------------------------------
    // Navegación
    // ------------------------------------------------------------------

    pub fn navigate(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if !fs_model::is_recycle_bin(&path) && !path.is_dir() {
            self.toast(crate::localized!(format, "No se encontró la carpeta: {}", "Folder not found: {}", path.display()), ToastTone::Error, cx);
            return;
        }
        Self::set_navigation_path(self.active_tab_mut(), &path);
        self.active_tab_mut().search_query.clear();
        self.active_tab_mut().recursive_search = false;
        self.search.update(cx, |state, cx| {
            state.set_value("", window, cx);
        });
        self.address.update(cx, |state, cx| {
            state.set_value(address_text(&path), window, cx);
        });
        self.address_editing = false;
        self.address_history_open = false;
        self.remember_path(&path, cx);
        self.apply_saved_folder_view();
        self.discard_preview();
        self.preview_path = None;
        self.dismiss_context_menu();
        self.sort_menu = false;
        self.persist();
        self.start_scan_active(cx);
        self.focus_root(window);
        cx.notify();
    }

    fn set_navigation_path(tab: &mut Tab, path: &Path) {
        if tab.path != path {
            tab.history.truncate(tab.hist_index + 1);
            tab.history.push(path.to_path_buf());
            tab.hist_index = tab.history.len() - 1;
        }
        tab.change_path(path.to_path_buf());
        tab.selection.clear();
        tab.anchor = None;
        tab.cursor = None;
        tab.loading = true;
        tab.error = None;

    }

    pub fn travel(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let target = {
            let tab = self.active_tab();
            let index = tab.hist_index as isize + delta;
            if index < 0 || index >= tab.history.len() as isize {
                None
            } else {
                Some((index as usize, tab.history[index as usize].clone()))
            }
        };
        let Some((index, path)) = target else {
            return;
        };
        {
            let tab = self.active_tab_mut();
            tab.hist_index = index;
            tab.change_path(path.clone());
            tab.selection.clear();
            tab.anchor = None;
            tab.cursor = None;
            tab.loading = true;
            tab.error = None;
        }
        self.active_tab_mut().search_query.clear();
        self.active_tab_mut().recursive_search = false;
        self.search.update(cx,|state,cx|state.set_value("",window,cx));
        self.address.update(cx, |state, cx| {
            state.set_value(address_text(&path), window, cx);
        });
        self.remember_path(&path, cx);
        self.apply_saved_folder_view();
        self.persist();
        self.start_scan_active(cx);
        self.focus_root(window);
        cx.notify();
    }

    pub fn go_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if fs_model::is_recycle_bin(&self.active_tab().path) {
            return;
        }
        if let Some(parent) = self.active_tab().path.parent() {
            let parent = parent.to_path_buf();
            self.navigate(parent, window, cx);
        }
    }

    pub fn start_scan_active(&mut self, cx: &mut Context<Self>) {
        self.restore_folder_view();
        let id = self.active_tab().id;
        self.start_scan_tab(id, cx);
    }

    pub fn start_scan_tab(&mut self, tab_id: u64, cx: &mut Context<Self>) {
        let show_hidden = self.settings.show_hidden;
        let ignore_accents = self.settings.search_ignore_accents;
        let (path, token, sort, sort_desc, query, recursive) = {
            let Some(tab) = self.tab_by_id_mut(tab_id) else {
                return;
            };
            tab.needs_scan = false;
            tab.folder_counts.clear();
            tab.scan_token = tab.scan_token.wrapping_add(1);
            tab.loading = true;
            tab.error = None;
            let token = tab.scan_token;
            (tab.path.clone(), token, tab.sort, tab.sort_desc, tab.search_query.clone(),
                tab.recursive_search && !tab.search_query.trim().is_empty())
        };

        let this = cx.entity().downgrade();
        let recycle = fs_model::is_recycle_bin(&path);
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut result = if recycle {
                        fs_model::scan_recycle_bin()
                    } else {
                        let scan_query = if recursive { &query } else { "" };
                        fs_model::scan(&path, show_hidden, recursive, scan_query, ignore_accents)
                    };
                    if recycle || sort != SortKey::Name || sort_desc {
                        fs_model::sort_entries(&mut result.entries, sort, sort_desc);
                    }
                    result
                })
                .await;

            this.update(cx, |this, cx| {
                let truncated = result.truncated;
                let error = result.error.clone();
                let preview_path = this.preview_path.clone();
                let ignore_accents = this.settings.search_ignore_accents;
                let refresh_image_preview;
                {
                    let Some(tab) = this.tab_by_id_mut(tab_id) else {
                        return;
                    };
                    if tab.scan_token != token {
                        return;
                    }
                    let old_visuals: HashMap<_, _> = tab.entries.iter()
                        .filter(|entry| entry.thumb.is_some() || entry.icon.is_some())
                        .map(|entry| (entry.path.as_path(), entry))
                        .collect();
                    let mut entries = result.entries;
                    refresh_image_preview = preview_path.as_ref().is_some_and(|path| {
                        let previous = tab.entry_for(path);
                        let next = entries.iter().find(|entry| &entry.path == path);
                        matches!((previous, next), (Some(old), Some(new))
                            if old.kind == FileKind::Image
                                && (old.modified != new.modified || old.size != new.size))
                    });
                    for entry in &mut entries {
                        if let Some(old) = old_visuals.get(entry.path.as_path())
                            && old.modified == entry.modified && old.size == entry.size
                            && old.is_dir == entry.is_dir
                        {
                            entry.thumb = old.thumb.clone();
                            entry.icon = old.icon.clone();
                        }
                    }
                    if tab.sort != sort || tab.sort_desc != sort_desc {
                        fs_model::sort_entries(&mut entries, tab.sort, tab.sort_desc);
                    }
                    tab.clear_tree();
                    tab.entries = entries;
                    tab.refresh_filter(ignore_accents);
                    tab.loading = false;
                    tab.error = result.error;
                }
                if truncated {
                    this.toast(
                        tr("Búsqueda parcial: se alcanzó el límite de 4000 resultados o 12 niveles.", "Partial search: the limit of 4,000 results or 12 levels was reached."),
                        ToastTone::Info,
                        cx,
                    );
                } else if let Some(error) = error {
                    this.toast(error, ToastTone::Error, cx);
                }
                if refresh_image_preview { this.preview_path = None; }
                this.refresh_preview(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn refresh_active(&mut self, cx: &mut Context<Self>) {
        self.start_scan_active(cx);
    }

    fn refresh_directories(&mut self, directories: &[PathBuf], cx: &mut Context<Self>) {
        if directories.iter().any(|path| fs_model::is_recycle_bin(path))
            && let Some(item) = self.quick.iter_mut()
                .find(|item| fs_model::is_recycle_bin(&item.path))
        {
            item.icon = platform::recycle_bin_icon();
            cx.notify();
        }
        for id in self.tabs_for_directories(directories) {
            self.start_scan_tab(id, cx);
        }
    }

    pub fn refresh_all(&mut self, cx: &mut Context<Self>) {
        let ids = self
            .panes
            .iter()
            .flat_map(|pane| pane.tabs.iter().map(|tab| tab.id))
            .collect::<Vec<_>>();
        for id in ids {
            self.start_scan_tab(id, cx);
        }
    }

    pub fn toggle_search_ignore_accents(&mut self, cx: &mut Context<Self>) {
        let ignore_accents = !self.settings.search_ignore_accents;
        self.settings.search_ignore_accents = ignore_accents;
        self.settings.save();

        let mut recursive_tabs = Vec::new();
        for tab in self.panes.iter_mut().flat_map(|pane| &mut pane.tabs) {
            if tab.search_query.trim().is_empty() {
                continue;
            }
            if tab.recursive_search {
                recursive_tabs.push(tab.id);
            } else {
                tab.refresh_filter(ignore_accents);
            }
        }
        for id in recursive_tabs {
            self.start_scan_tab(id, cx);
        }
        self.refresh_preview(cx);
        cx.notify();
    }

    pub fn apply_filter(&mut self, cx: &mut Context<Self>) {
        self.refresh_active_filter();
        self.refresh_preview(cx);
        cx.notify();
    }

    fn refresh_active_filter(&mut self) {
        let ignore_accents = self.settings.search_ignore_accents;
        self.active_tab_mut().refresh_filter(ignore_accents);
    }

    /// Muestra u oculta las carpetas manteniendo el resto del filtro.
    pub fn toggle_folders(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        tab.show_folders = !tab.show_folders;
        if !tab.show_folders && !tab.show_files { tab.show_files = true; }
        self.apply_filter(cx);
    }

    /// Muestra u oculta los archivos; al ocultarlos quedan solo las carpetas.
    pub fn toggle_files(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        tab.show_files = !tab.show_files;
        if !tab.show_files && !tab.show_folders { tab.show_folders = true; }
        self.apply_filter(cx);
    }

    pub fn start_recursive_search(&mut self, cx: &mut Context<Self>) {
        if self.active_tab().search_query.trim().is_empty() {
            return;
        }
        self.active_tab_mut().recursive_search = true;
        self.start_scan_active(cx);
        cx.notify();
    }

    pub fn toggle_recursive_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_tab().recursive_search {
            self.active_tab_mut().recursive_search = false;
            self.start_scan_active(cx);
            cx.notify();
        } else if self.active_tab().search_query.trim().is_empty() {
            self.focus_search(window, cx);
        } else {
            self.start_recursive_search(cx);
        }
    }

    pub fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.active_tab_mut().search_query.clear();
        self.active_tab_mut().recursive_search = false;
        self.search.update(cx, |state, cx| {
            state.set_value("", window, cx);
        });
        self.start_scan_active(cx);
        cx.notify();
    }

    pub fn commit_address(&mut self, value: String, window: &mut Window, cx: &mut Context<Self>) {
        self.address_editing = false;
        // La dirección muestra «Papelera»; al confirmar esa misma palabra se
        // vuelve a abrir la vista virtual en lugar de buscar una carpeta real.
        let trimmed = value.trim();
        if trimmed.eq_ignore_ascii_case("Papelera") || trimmed.eq_ignore_ascii_case("Recycle Bin")
            || trimmed.eq_ignore_ascii_case(fs_model::RECYCLE_BIN_PATH)
        {
            self.navigate(PathBuf::from(fs_model::RECYCLE_BIN_PATH), window, cx);
            cx.notify();
            return;
        }
        let base = self.active_tab().path.clone();
        let path = expand_path(&value, &base);
        if path.is_file() {
            let parent = path.parent().map(|path| path.to_path_buf()).unwrap_or(base);
            let name = path.file_name().map(|name| name.to_string_lossy().into_owned());
            self.navigate(parent, window, cx);
            if let Some(name) = name {
                let tab = self.active_tab_mut();
                if let Some(entry) = tab.entries.iter().find(|entry| entry.name == name) {
                    let path = entry.path.clone();
                    tab.selection.insert(path.clone());
                    tab.cursor = Some(path);
                }
            }
        } else if path.is_dir() || fs_model::is_recycle_bin(&path) {
            self.navigate(path, window, cx);
        } else {
            self.toast(tr("Esa ruta no existe o no es accesible.", "This path does not exist or cannot be accessed."), ToastTone::Error, cx);
            self.focus_root(window);
        }
        cx.notify();
    }

    pub fn begin_address_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_address(self.active_tab().path.clone(), window, cx);
    }

    fn edit_address(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.address_history_selection = None;
        self.address_history_scroll = gpui::ScrollHandle::new();
        self.address_editing = true;
        self.address_history_open = true;
        self.address.update(cx, |state, cx| {
            state.set_value(address_text(&path), window, cx);
            state.focus(window, cx);
        });
        // La ruta actual queda seleccionada para poder escribir una nueva de inmediato.
        // El input se registra en el árbol de acciones tras el primer frame, así que
        // se lanza la selección con un pequeño retardo.
        let task = cx.spawn_in(window, async move |_this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(40))
                .await;
            let _ = cx.update(|window, cx| {
                window.dispatch_action(Box::new(gpui_component::input::SelectAll), cx);
            });
        });
        task.detach();
        cx.notify();
    }

    pub fn focus_root(&self, window: &mut Window) {
        window.focus(&self.focus_handle);
    }

    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |state, cx| state.focus(window, cx));
    }

    // ------------------------------------------------------------------
    // Selección y apertura
    // ------------------------------------------------------------------

    pub fn toggle_tree_folder(&mut self, tab_id: u64, path: PathBuf, cx: &mut Context<Self>) {
        let show_hidden = self.settings.show_hidden;
        let ignore_accents = self.settings.search_ignore_accents;
        let Some(tab) = self.tab_by_id_mut(tab_id) else { return };
        if tab.view != ViewMode::Tree || tab.recursive_search { return; }
        if fs_model::is_recycle_bin(&tab.path) {
            return;
        }
        if tab.tree_expanded.contains(&path) {
            tab.collapse_tree_folder(&path);
            tab.refresh_filter(ignore_accents);
            self.refresh_preview(cx);
            cx.notify();
            return;
        }
        tab.tree_expanded.insert(path.clone());
        tab.next_tree_request = tab.next_tree_request.wrapping_add(1);
        let request = tab.next_tree_request;
        tab.tree_requests.insert(path.clone(), request);
        let entity = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let folder = path.clone();
            let result = cx.background_executor()
                .spawn(async move { fs_model::scan(&folder, show_hidden, false, "", false) }).await;
            let _ = entity.update(cx, |app, cx| {
                let ignore_accents = app.settings.search_ignore_accents;
                let Some(tab) = app.tab_by_id_mut(tab_id) else { return };
                if tab.tree_requests.get(&path) != Some(&request) {
                    return;
                }
                tab.tree_requests.remove(&path);
                if let Some(error) = result.error {
                    tab.tree_expanded.remove(&path);
                    app.toast(error, ToastTone::Error, cx);
                } else {
                    tab.tree_root_count.get_or_insert(tab.entries.len());
                    tab.entries.extend(result.entries);
                    tab.sort_entries();
                    tab.refresh_filter(ignore_accents);
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }

    pub(crate) fn click_file_view_entry(
        &mut self,
        path: PathBuf,
        modifiers: gpui::Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let file = self.settings.action_sounds_enabled
            && self.active_tab().entry_for(&path).is_some_and(|entry| !entry.is_dir && !entry.folder_shortcut);
        self.click_entry(path.clone(), modifiers, window, cx);
        if file && self.active_tab().selection.contains(&path) {
            self.play_action_sound(SoundAction::SelectFile);
        }
    }

    pub fn click_entry(
        &mut self,
        path: PathBuf,
        modifiers: gpui::Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        {
            let tab = self.active_tab_mut();
            if modifiers.shift {
                let anchor = tab.anchor.clone().unwrap_or_else(|| path.clone());
                let start = tab.visible_index_of(&anchor);
                let end = tab.visible_index_of(&path);
                if let (Some(start), Some(end)) = (start, end) {
                    let (start, end) = if start <= end { (start, end) } else { (end, start) };
                    tab.selection.clear();
                    for index in start..=end {
                        if let Some(entry) = tab.visible.get(index).and_then(|index| tab.entries.get(*index)) {
                            tab.selection.insert(entry.path.clone());
                        }
                    }
                }
                tab.cursor = Some(path);
            } else if modifiers.control || modifiers.platform {
                if !tab.selection.remove(&path) {
                    tab.selection.insert(path.clone());
                }
                tab.anchor = Some(path.clone());
                tab.cursor = Some(path);
            } else {
                tab.selection.clear();
                tab.selection.insert(path.clone());
                tab.anchor = Some(path.clone());
                tab.cursor = Some(path);
            }
        }
        self.restore_folder_view();
        self.sync_search_field(window, cx);
        self.refresh_preview(cx);
        self.focus_root(window);
        cx.notify();
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        tab.selection = tab
            .visible
            .iter()
            .filter_map(|index| tab.entries.get(*index).map(|entry| entry.path.clone()))
            .collect();
        if let Some(first) = tab.selection.iter().next().cloned() {
            tab.cursor = Some(first);
        }
        self.refresh_preview(cx);
        cx.notify();
    }

    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        tab.selection.clear();
        tab.cursor = None;
        self.refresh_preview(cx);
        cx.notify();
    }

    pub fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.in_recycle_bin() && !fs_model::is_recycle_bin(&path) {
            self.restore_recycle_paths(vec![path], cx);
        } else {
            self.open_location(path, window, cx);
        }
    }

    pub fn open_location(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if fs_model::is_recycle_bin(&path) || path.is_dir() {
            self.navigate(path, window, cx);
        } else {
            self.open_file(path, cx);
        }
    }

    fn open_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let opened = platform::open(&path);
        let generation = self.recent_files_generation;
        cx.spawn(async move |this, cx| {
            if opened.await == Ok(true) {
                let _ = this.update(cx, |this, cx| {
                    if this.recent_files_generation != generation { return; }
                    let recent = &mut this.settings.recent_file_paths;
                    recent.retain(|existing| !same_favorite_path(existing, &path));
                    record_recent(recent, &path);
                    this.settings.save();
                    this.load_recent_icons(cx);
                    cx.notify();
                });
            }
        }).detach();
    }

    pub fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() { return; }
        if self.in_recycle_bin() {
            let paths = self.active_tab().selected_paths();
            if paths.is_empty() {
                self.toast(tr("Selecciona un elemento para restaurarlo.", "Select an item to restore."), ToastTone::Info, cx);
            } else {
                self.restore_recycle_paths(paths, cx);
            }
            return;
        }
        let paths = self.active_tab().selected_paths();
        if paths.is_empty() {
            return;
        }
        for path in paths {
            if path.is_dir() {
                self.navigate(path, window, cx);
                return;
            }
        }
        let paths = self.active_tab().selected_paths();
        if self.settings.confirm_open_multiple && paths.len() >= self.settings.confirm_open_count.max(2) {
            self.dialog = Some(Dialog::OpenMultiple(paths));
            self.focus_root(window);
            cx.notify();
        } else {
            for path in paths { self.open_file(path, cx); }
        }
    }

    pub fn activate_entry(
        &mut self,
        path: PathBuf,
        modifiers: gpui::Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.click_entry(path.clone(), modifiers, window, cx);
        self.open_path(path, window, cx);
    }

    // ------------------------------------------------------------------
    // Miniaturas y vista previa
    // ------------------------------------------------------------------

    pub(crate) fn can_edit_preview_text(path: &Path) -> bool {
        path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext|
            matches!(ext.to_ascii_lowercase().as_str(), "txt" | "md" | "log" | "json" | "csv"))
    }

    pub(crate) fn begin_preview_text(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_edit.is_some() || !Self::can_edit_preview_text(&path) { return; }
        use std::io::Read;
        let loaded = (|| -> std::io::Result<String> {
            if !std::fs::symlink_metadata(&path)?.file_type().is_file() {
                return Err(std::io::Error::other(tr("La ruta no corresponde a un archivo normal.", "The path does not point to a regular file.")));
            }
            let mut text = String::new();
            std::fs::File::open(&path)?.take(1_048_577).read_to_string(&mut text)?;
            Ok(text)
        })();
        let Ok(original) = loaded else {
            self.toast(tr("No se pudo leer el archivo como texto UTF-8.", "The file could not be read as UTF-8 text."), ToastTone::Error, cx);
            return;
        };
        if original.len() > 1_048_576 || original.contains('\0') {
            self.toast(tr("El editor admite texto de hasta 1 MB.", "The editor supports text files up to 1 MB."), ToastTone::Info, cx);
            return;
        }
        let visible = preview_editor_text(&original);
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).multi_line(true).rows(12);
            state.set_value(visible, window, cx);
            state
        });
        input.read(cx).focus_handle(cx).focus(window);
        self.text_edit = Some((path, original, input));
        cx.notify();
    }

    fn reveal_pending_text_edit(&mut self, cx: &mut Context<Self>) {
        self.settings.show_preview = true;
        self.toast(tr("Guarda o cancela la edición pendiente en Vista previa.", "Save or cancel your pending edit in Preview."), ToastTone::Info, cx);
        cx.notify();
    }

    pub(crate) fn cancel_preview_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.text_edit = None;
        self.focus_root(window);
        cx.notify();
    }

    pub(crate) fn save_preview_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, original, input)) = &self.text_edit else { return; };
        let mut text = input.read(cx).value().to_string();
        if text == preview_editor_text(original) {
            self.cancel_preview_text(window, cx);
            return;
        }
        if original.contains("\r\n") { text = text.replace("\r\n", "\n").replace('\n', "\r\n"); }
        if original.starts_with('\u{feff}') { text.insert(0, '\u{feff}'); }
        use std::io::Read;
        let mut on_disk = Vec::new();
        let unchanged = std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file())
            && std::fs::File::open(path).and_then(|file|
                file.take(original.len() as u64 + 1).read_to_end(&mut on_disk)).is_ok()
            && on_disk == original.as_bytes();
        if !unchanged {
            self.toast(tr("El archivo cambió fuera de Xool Files. Tu edición sigue abierta.", "The file changed outside Xool Files. Your edit is still open."), ToastTone::Error, cx);
            return;
        }
        match crate::settings::atomic_write(path, text.as_bytes()) {
            Ok(()) => {
                self.cancel_preview_text(window, cx);
                self.refresh_preview(cx);
                self.toast(tr("Archivo guardado.", "File saved."), ToastTone::Success, cx);
            }
            Err(error) => self.toast(crate::localized!(format, "No se pudo guardar: {error}", "Could not save: {error}"), ToastTone::Error, cx),
        }
    }

    pub(crate) fn toggle_image_conversion(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let expanded = matches!(&self.preview_image_tool, Some(PreviewImageTool::Convert(current)) if current == &path);
        self.preview_image_tool = (!expanded).then_some(PreviewImageTool::Convert(path));
        cx.notify();
    }

    pub(crate) fn toggle_image_resize(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.image_editing { return; }
        if matches!(&self.preview_image_tool, Some(PreviewImageTool::Resize(current, _)) if current == &path) {
            self.close_image_tool(cx);
            return;
        }
        self.image_resize_generation = self.image_resize_generation.wrapping_add(1);
        let generation = self.image_resize_generation;
        self.preview_image_tool = Some(PreviewImageTool::Resize(path.clone(), None));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move {
                crate::image_edits::oriented_dimensions(&path)
            }).await;
            let _ = this.update(cx, |this, cx| {
                if this.image_resize_generation != generation
                    || !matches!(this.preview_image_tool, Some(PreviewImageTool::Resize(..))) { return; }
                match result {
                    Ok(dimensions) => {
                        if let Some(PreviewImageTool::Resize(_, controls)) = &mut this.preview_image_tool {
                            *controls = Some(crate::image_edits::ResizeControls::new(dimensions));
                        }
                    }
                    Err(error) => {
                        this.preview_image_tool = None;
                        this.toast(crate::localized!(format, "No se pudo leer el tamaño: {error}", "Could not read image size: {error}"),
                            ToastTone::Error, cx);
                    }
                }
                cx.notify();
            });
        }).detach();
    }

    pub(crate) fn close_image_tool(&mut self, cx: &mut Context<Self>) {
        if self.preview_image_tool.take().is_some() { cx.notify(); }
    }

    pub(crate) fn save_image_resize(&mut self, cx: &mut Context<Self>) {
        let Some(PreviewImageTool::Resize(path, Some(controls))) = &self.preview_image_tool else { return; };
        let [width, height] = controls.dimensions;
        if controls.dimensions != controls.original
            && crate::image_edits::resize_allowed(controls.original[0], width, height)
        {
            self.save_image_copy(path.clone(), crate::image_edits::Edit::Resize(width, height), cx);
        }
    }

    pub(crate) fn image_resize_controls_mut(&mut self) -> Option<&mut crate::image_edits::ResizeControls> {
        match &mut self.preview_image_tool {
            Some(PreviewImageTool::Resize(_, Some(controls))) => Some(controls),
            _ => None,
        }
    }

    pub(crate) fn drag_image_resize(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(controls) = self.image_resize_controls_mut() else { return; };
        let Some((axis, origin, initial)) = controls.drag else { return; };
        if !event.dragging() {
            controls.drag = None;
            return;
        }
        let before = controls.dimensions;
        let step = (controls.original[axis] / 400).clamp(1, 40) as f32;
        let delta = (f32::from(event.position.x) - origin) * step;
        if delta.abs() >= 1. || before[axis] != initial {
            controls.set_dimension(axis, initial as i32 + delta.round() as i32);
        }
        if controls.dimensions != before { cx.notify(); }
    }

    pub(crate) fn save_image_copy(&mut self, path: PathBuf, edit: crate::image_edits::Edit, cx: &mut Context<Self>) {
        if self.image_editing { return; }
        self.preview_image_tool = None;
        self.image_editing = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move {
                crate::image_edits::save_copy(&path, edit)
            }).await;
            let _ = this.update(cx, |this, cx| {
                this.image_editing = false;
                match result {
                    Ok(path) => {
                        let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy();
                        this.toast(crate::localized!(format, "Copia guardada: {name}", "Copy saved: {name}"), ToastTone::Success, cx);
                        if let Some(parent) = path.parent() {
                            this.refresh_directories(&[parent.to_path_buf()], cx);
                        }
                    }
                    Err(error) => this.toast(crate::localized!(format, "No se pudo crear la copia: {error}", "Could not create the copy: {error}"), ToastTone::Error, cx),
                }
                cx.notify();
            });
        }).detach();
    }

    pub fn refresh_preview(&mut self, cx: &mut Context<Self>) {
        let path = (self.settings.show_preview && self.preview_fits).then(|| self.active_tab().primary_path()).flatten();
        if self.preview_image_tool.as_ref().map(PreviewImageTool::path) != path.as_deref() {
            self.preview_image_tool = None;
        }
        let Some(path) = path else {
            if self.preview_loading {
                self.preview_token = self.preview_token.wrapping_add(1);
            }
            self.stop_media();
            self.discard_preview();
            self.preview_path = None;
            self.preview_loading = false;
            return;
        };
        if self.preview_path.as_ref() == Some(&path) && (self.preview.is_some() || self.preview_loading) {
            return;
        }
        let Some(entry) = self.active_tab().entry_for(&path).cloned() else {
            return;
        };
        self.stop_media();
        self.preview_token = self.preview_token.wrapping_add(1);
        let token = self.preview_token;
        let preview_visible = self.settings.show_preview;
        self.preview_path = Some(path.clone());
        self.discard_preview();
        self.preview_loading = true;

        if entry.kind == FileKind::Audio {
            self.audio = Some(AudioPlayback::new(
                entry.path.clone(),
                self.settings.audio_volume,
            ));
            let audio_path = entry.path.clone();
            let this = cx.entity().downgrade();
            cx.spawn(async move |_, cx| {
                let track = cx
                    .background_executor()
                    .spawn(async move { media::analyze_audio(&audio_path) })
                    .await;
                this.update(cx, |this, cx| {
                    if this.preview_token == token {
                        match (this.audio.as_mut(), track) {
                            (Some(audio), Some(track)) => {
                                audio.set_track(track);
                                if this.settings.preview_autoplay && this.settings.show_preview {
                                    audio.play();
                                }
                            }
                            (Some(audio), None) => {
                                audio.loading = false;
                                audio.error =
                                    Some(tr("No se pudo analizar el archivo de audio.", "Could not analyze the audio file.").to_string());
                            }
                            _ => {}
                        }
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        } else if entry.kind == FileKind::Video {
            let edge = (self.settings.preview_width * 1.35).clamp(400.0, 640.0) as u32;
            let mut video = VideoPlayer::open(entry.path.clone(), self.settings.audio_volume, edge);
            if preview_visible && self.settings.video_autoplay {
                video.toggle();
            }
            self.video = Some(video);
            self.preview = Some(PreviewContent::Media { thumb: entry.thumb, kind: entry.kind });
            self.preview_loading = false;
            cx.notify();
            return;
        }

        let this = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let content = cx
                .background_executor()
                .spawn(async move {
                    thumbs::preview(&entry.path, entry.is_dir, entry.kind, 720)
                })
                .await;
            this.update(cx, |this, cx| {
                if this.preview_token == token {
                    this.preview = Some(content);
                    this.preview_loading = false;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn stop_media(&mut self) {
        self.audio_seek_drag = None;
        if let Some(mut audio) = self.audio.take() {
            audio.stop();
        }
        if let Some(video) = self.video.take() {
            if let Some(frame) = video.frame.clone() {
                self.image_drops.push(frame);
            }
            video.stop();
        }
    }

    /// Suelta el contenido de la vista previa y libera su textura.
    fn discard_preview(&mut self) {
        if let Some(PreviewContent::Image(image)) = self.preview.take() {
            self.image_drops.push(image);
        }
    }

    pub fn toggle_audio(&mut self, cx: &mut Context<Self>) {
        if let Some(audio) = &mut self.audio {
            audio.toggle();
            cx.notify();
        }
    }

    pub fn audio_seek_ratio(&self) -> Option<f32> {
        self.audio_seek_drag.as_ref().map(|drag| drag.ratio)
    }

    pub fn begin_audio_seek_drag(&mut self, bounds: Bounds<Pixels>, x: Pixels, cx: &mut Context<Self>) {
        let Some(audio) = &mut self.audio else { return };
        if bounds.size.width <= gpui::px(0.) { return; }

        let ratio = audio_seek_at(bounds, x);
        self.audio_seek_drag = Some(AudioSeekDrag {
            bounds, ratio, was_playing: audio.playing,
        });
        if ratio < 1. { audio.seek_ratio(ratio); }
        cx.notify();
    }

    pub fn drag_audio_seek(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(drag) = &mut self.audio_seek_drag else { return };
        let ratio = audio_seek_at(drag.bounds, x);
        if drag.ratio != ratio {
            drag.ratio = ratio;
            if ratio < 1. && let Some(audio) = &mut self.audio {
                audio.seek_ratio(ratio);
                if drag.was_playing && !audio.playing { audio.play(); }
            }
            cx.notify();
        }
    }

    pub fn end_audio_seek_drag(&mut self, x: Option<Pixels>, cx: &mut Context<Self>) {
        if let Some(drag) = self.audio_seek_drag.take() && let Some(audio) = &mut self.audio {
            let ratio = x.map_or(drag.ratio, |x| audio_seek_at(drag.bounds, x));
            audio.seek_ratio(ratio);
            if ratio < 1. && drag.was_playing && !audio.playing { audio.play(); }
            cx.notify();
        }
    }

    pub fn set_media_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        self.apply_media_volume(volume, cx);
        self.settings.save();
    }

    fn apply_media_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        let volume = volume.clamp(0.0, 1.0);
        if let Some(audio) = &mut self.audio {
            audio.set_volume(volume);
        }
        if let Some(video) = &mut self.video {
            video.set_volume(volume);
        }
        self.settings.audio_volume = volume;
        cx.notify();
    }

    pub fn toggle_video(&mut self, cx: &mut Context<Self>) {
        if let Some(video) = &mut self.video {
            video.toggle();
            cx.notify();
        }
    }

    pub fn seek_video(&mut self, ratio: f32, cx: &mut Context<Self>) {
        if let Some(video) = &self.video {
            video.seek_ratio(ratio);
            cx.notify();
        }
    }

    pub fn begin_video_seek_drag(
        &mut self,
        bounds: Option<Bounds<Pixels>>,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        self.video_seek_bounds = bounds;
        self.video_seek_dragging = true;
        self.drag_video_seek_at(f32::from(event.position.x), cx);
    }

    pub fn drag_video_seek(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.video_seek_dragging {
            return;
        }
        self.drag_video_seek_at(f32::from(event.position.x), cx);
    }

    fn drag_video_seek_at(&mut self, x: f32, cx: &mut Context<Self>) {
        let Some(bounds) = self.video_seek_bounds else {
            return;
        };
        let width = f32::from(bounds.size.width);
        if width <= 1.0 {
            return;
        }
        let ratio = ((x - f32::from(bounds.origin.x)) / width).clamp(0.0, 1.0);
        self.seek_video(ratio, cx);
    }

    pub fn end_video_seek_drag(&mut self, cx: &mut Context<Self>) {
        if !self.video_seek_dragging {
            return;
        }
        self.video_seek_dragging = false;
        self.video_seek_bounds = None;
        cx.notify();
    }

    pub fn begin_media_volume_drag(
        &mut self,
        bounds: Bounds<Pixels>,
        x: Pixels,
        cx: &mut Context<Self>,
    ) {
        self.media_volume_bounds = Some(bounds);
        self.drag_media_volume(x, cx);
    }

    pub fn drag_media_volume(&mut self, x: Pixels, cx: &mut Context<Self>) {
        if let Some(bounds) = self.media_volume_bounds && bounds.size.width > gpui::px(1.) {
            self.apply_media_volume(f32::from(x - bounds.origin.x) / f32::from(bounds.size.width), cx);
        }
    }

    pub fn end_media_volume_drag(&mut self, cx: &mut Context<Self>) {
        if self.media_volume_bounds.take().is_some() {
            self.settings.save();
            cx.notify();
        }
    }

    /// Carga solo las filas que la lista virtual va a dibujar y un margen pequeño.
    pub fn load_visible_thumbnails(
        &mut self,
        tab_id: u64,
        range: std::ops::Range<usize>,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tab_by_id_mut(tab_id) else { return };
        let grid = tab.view == ViewMode::Grid;
        if range.is_empty() || range.start >= tab.visible.len() {
            return;
        }
        if tab.thumbnail_range.start <= range.start && tab.thumbnail_range.end >= range.end {
            return;
        }

        let start = range.start.saturating_sub(12);
        let end = range.end.saturating_add(12).min(tab.visible.len());
        tab.thumbnail_range = start..end;
        let retained: HashSet<_> = tab.visible[start..end].iter().copied().collect();
        let mut expired = Vec::new();
        for (index, entry) in tab.entries.iter_mut().enumerate() {
            if (entry.thumb.is_some() || entry.icon.is_some()) && !retained.contains(&index) {
                expired.extend(entry.thumb.take());
                entry.icon = None;
            }
        }

        let requests: Vec<_> = tab.visible[range.start..range.end.min(end)].iter()
            .chain(tab.visible[range.end.min(end)..end].iter())
            .chain(tab.visible[start..range.start].iter())
            .filter_map(|&index| {
                let entry = &tab.entries[index];
                ((grid || !entry.is_dir) && entry.thumb.is_none() && entry.icon.is_none())
                    .then(|| (index, entry.path.clone(), entry.is_dir, entry.kind))
            })
            .collect();
        tab.thumbnail_task = None;
        self.image_drops.extend(expired);
        if requests.is_empty() {
            return;
        }

        let entity = cx.entity().downgrade();
        let task = cx.spawn(async move |_, cx| {
            // Entrega primero los elementos visibles. Cada lote comparte un
            // viaje al ejecutor y una sola actualización de la interfaz.
            for batch in requests.chunks(6) {
                let batch = batch.to_vec();
                let visuals = cx.background_executor().spawn(async move {
                    batch.into_iter().map(|(index, path, is_dir, kind)| {
                        let (thumb, icon) = thumbs::entry_visual(&path, is_dir, kind, 256);
                        (index, path, thumb, icon)
                    }).collect::<Vec<_>>()
                }).await;

                if entity.update(cx, |this, cx| {
                    let Some(tab) = this.tab_by_id_mut(tab_id) else { return };
                    let mut changed = false;
                    for (index, path, thumb, icon) in visuals {
                        if let Some(entry) = tab.entries.get_mut(index)
                            && entry.path == path && (thumb.is_some() || icon.is_some())
                        {
                            entry.thumb = thumb;
                            entry.icon = icon;
                            changed = true;
                        }
                    }
                    if changed {
                        cx.notify();
                    }
                }).is_err() {
                    break;
                }
            }
        });
        if let Some(tab) = self.tab_by_id_mut(tab_id) {
            tab.thumbnail_task = Some(task);
        }
    }

    fn release_inactive_thumbnails(&mut self) {
        const INACTIVE_ENTRY_BUDGET: usize = 12_000;
        let mut retained = 0usize;
        for pane in &mut self.panes {
            for (index, tab) in pane.tabs.iter_mut().enumerate() {
                if index == pane.active { continue; }
                if !tab.thumbnail_range.is_empty() {
                    tab.reset_thumbnails();
                    for entry in &mut tab.entries {
                        self.image_drops.extend(entry.thumb.take());
                        entry.icon = None;
                    }
                }
                if retained.saturating_add(tab.entries.len()) > INACTIVE_ENTRY_BUDGET
                    && !tab.recursive_search && !tab.loading && tab.error.is_none()
                    && tab.selection.is_empty() && tab.tree_expanded.is_empty()
                    && tab.tree_requests.is_empty()
                {
                    tab.entries = Vec::new();
                    tab.visible = Vec::new();
                    tab.loading = true;
                    tab.needs_scan = true;
                } else {
                    retained += tab.entries.len();
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Pestañas y paneles
    // ------------------------------------------------------------------

    pub fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let home = self.settings.new_tab_home.then(|| self.quick.first()).flatten();
        let path = home.map_or_else(|| self.active_tab().path.clone(), |item| item.path.clone());
        self.open_in_new_tab(path, window, cx);
    }

    pub fn close_tab(&mut self, pane_index: usize, tab_index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tabs(pane_index, |index| index == tab_index, window, cx);
    }

    pub(crate) fn middle_click_tab(&mut self, tab_id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some((pane, tab)) = self.tab_position(tab_id) else { return };
        if self.panes.len() == 1 && self.panes[pane].tabs.len() == 1 {
            if self.request_window_close(window, cx) {
                window.remove_window();
            }
        } else {
            self.close_tab(pane, tab, window, cx);
        }
    }

    fn close_tabs(
        &mut self, pane_index: usize, close: impl Fn(usize) -> bool,
        window: &mut Window, cx: &mut Context<Self>,
    ) {
        let Some(pane) = self.panes.get(pane_index) else { return };
        let mut closed: Vec<_> = pane.tabs.iter().enumerate()
            .filter(|(index, _)| close(*index))
            .map(|(_, tab)| tab.path.clone())
            .collect();
        if closed.is_empty() { return; }
        let close_pane = self.panes.len() > 1 && closed.len() == pane.tabs.len();
        closed.reverse();
        self.settings.closed_tabs.splice(0..0, closed);
        self.settings.closed_tabs.truncate(15);

        // Cerrar la única pestaña de un panel dividido cierra ese panel y
        // devuelve la división a una sola vista. Antes se creaba una pestaña
        // nueva en el panel vacío y la división se quedaba abierta.
        if close_pane {
            let active = if self.active_pane == pane_index {
                self.pane_layout.sibling(pane_index).unwrap_or(0)
            } else {
                self.active_pane
            };
            self.remove_pane(pane_index);
            self.active_pane = active - usize::from(active > pane_index);
            self.toast(tr("Panel dividido cerrado.", "Split pane closed."), ToastTone::Info, cx);
        } else {
            let pane = &mut self.panes[pane_index];
            let active = (0..pane.active).filter(|&index| !close(index)).count();
            let mut index = 0;
            pane.tabs.retain(|_| {
                let keep = !close(index);
                index += 1;
                keep
            });
            pane.active = active.min(pane.tabs.len().saturating_sub(1));
            if pane.tabs.is_empty() {
                let home = self.quick.first().map(|item| item.path.clone())
                    .unwrap_or_else(|| PathBuf::from("C:\\"));
                let id = self.next_id();
                self.panes[pane_index].tabs.push(Tab::new(id, home, &self.settings));
                self.start_scan_tab(id, cx);
            }
        }

        self.active_pane = self.active_pane.min(self.panes.len().saturating_sub(1));
        let path = self.active_tab().path.clone();
        self.address.update(cx, |state, cx| {
            state.set_value(address_text(&path), window, cx);
        });
        self.persist();
        self.restore_folder_view();
        self.sync_search_field(window, cx);
        self.refresh_preview(cx);
        self.release_inactive_thumbnails();
        self.focus_root(window);
        cx.notify();
    }

    pub(crate) fn clear_closed_tabs(&mut self, cx: &mut Context<Self>) {
        self.settings.closed_tabs.clear();
        self.settings.save();
        cx.notify();
    }

    pub(crate) fn reopen_closed_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.settings.closed_tabs.get(index).cloned() else { return; };
        if !path.is_dir() && !fs_model::is_recycle_bin(&path) {
            self.toast(tr("La ubicación de esta pestaña ya no está disponible.", "This tab’s location is no longer available."), ToastTone::Info, cx);
            return;
        }
        self.settings.closed_tabs.remove(index);
        self.recently_closed_open = false;
        self.open_in_new_tab(path, window, cx);
        self.refresh_preview(cx);
    }

    pub fn close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane;
        let tab = self.active_pane().active;
        self.close_tab(pane, tab, window, cx);
    }

    pub fn wheel_tab(&mut self, pane: usize, direction: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.panes.get(pane) else { return };
        if self.dialog.is_some() || self.context_menu.is_some() || target.tabs.len() < 2 { return; }
        if let Some(index) = target.active.checked_add_signed(direction).filter(|index| *index < target.tabs.len()) {
            self.switch_tab(pane, index, window, cx);
        }
    }

    pub fn cycle_tab(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane_mut();
        let count = pane.tabs.len();
        if count <= 1 {
            return;
        }
        let previous = pane.active;
        pane.active = ((pane.active as isize + delta).rem_euclid(count as isize)) as usize;
        let changed = pane.active != previous;
        let path = pane.active_tab().path.clone();
        if changed {
            self.play_action_sound(SoundAction::SwitchTab);
        }
        self.address.update(cx, |state, cx| {
            state.set_value(address_text(&path), window, cx);
        });
        self.restore_folder_view();
        self.sync_search_field(window, cx);
        self.refresh_preview(cx);
        self.release_inactive_thumbnails();
        self.focus_root(window);
        cx.notify();
    }

    pub fn switch_tab(&mut self, pane_index: usize, tab_index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if pane_index >= self.panes.len() {
            return;
        }
        let previous = self.active_tab().id;
        self.active_pane = pane_index;
        let pane = &mut self.panes[pane_index];
        if tab_index < pane.tabs.len() {
            pane.active = tab_index;
        }
        let path = pane.active_tab().path.clone();
        if pane.active_tab().id != previous {
            self.play_action_sound(SoundAction::SwitchTab);
        }
        self.address.update(cx, |state, cx| {
            state.set_value(address_text(&path), window, cx);
        });
        self.remember_path(&path, cx);
        self.persist();
        self.restore_folder_view();
        self.sync_search_field(window, cx);
        self.refresh_preview(cx);
        self.release_inactive_thumbnails();
        self.focus_root(window);
        cx.notify();
    }

    pub fn toggle_split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.panes.len() > 1 {
            let active_id = self.active_tab().id;
            let mut order = Vec::new();
            self.pane_layout.leaves(&mut order);
            let tabs: Vec<_> = order.into_iter()
                .flat_map(|index| std::mem::take(&mut self.panes[index].tabs))
                .collect();
            let active = tabs.iter().position(|tab| tab.id == active_id).unwrap_or(0);
            self.panes = vec![Pane::with_tabs(tabs, active)];
            self.pane_layout = PaneLayout::Leaf(0);
            self.active_pane = 0;
            self.persist();
            self.toast(tr("Panel dividido cerrado.", "Split pane closed."), ToastTone::Info, cx);
        } else {
            // La pestaña activa se queda a la izquierda y el resto pasa al
            // panel derecho. Si solo hay una pestaña, el panel derecho abre
            // una copia de la misma carpeta.
            let (right_pane, duplicate_id) = {
                let pane = &mut self.panes[0];
                let active = pane.active.min(pane.tabs.len().saturating_sub(1));
                pane.active = active;
                if pane.tabs.len() > 1 {
                    let mut all: Vec<Tab> = pane.tabs.drain(..).collect();
                    let keep = all.remove(active);
                    pane.tabs.push(keep);
                    pane.active = 0;
                    let right_active = active.min(all.len().saturating_sub(1));
                    (Pane::with_tabs(all, right_active), None)
                } else {
                    let path = pane.tabs[0].path.clone();
                    let id = self.next_id();
                    let tab = Tab::new(id, path, &self.settings);
                    (Pane::new(tab), Some(id))
                }
            };
            self.panes.push(right_pane);
            self.pane_layout = PaneLayout::pair(
                if self.settings.split_below { Axis::Down } else { Axis::Across },
                self.settings.split_ratio, PaneLayout::Leaf(0), PaneLayout::Leaf(1),
            );
            self.active_pane = 0;
            if let Some(id) = duplicate_id {
                self.start_scan_tab(id, cx);
            }
            self.persist();
            self.toast(tr("Panel dividido activado.", "Split pane enabled."), ToastTone::Info, cx);
        }
        self.restore_folder_view();
        self.sync_search_field(window, cx);
        self.refresh_preview(cx);
        self.release_inactive_thumbnails();
        self.focus_root(window);
        cx.notify();
    }

    pub fn open_in_new_tab(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.play_action_sound(SoundAction::SwitchTab);
        let id = self.next_id();
        let tab = Tab::new(id, path.clone(), &self.settings);
        let pane = self.active_pane_mut();
        pane.tabs.push(tab);
        pane.active = pane.tabs.len() - 1;
        self.release_inactive_thumbnails();
        self.start_scan_active(cx);
        self.sync_search_field(window, cx);
        self.address.update(cx, |state, cx| {
            state.set_value(address_text(&path), window, cx);
        });
        self.persist();
        self.focus_root(window);
        cx.notify();
    }

    pub fn open_with_middle_click(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.focus_middle_tab {
            self.open_in_new_tab(path, window, cx);
        } else {
            self.play_action_sound(SoundAction::SwitchTab);
            let id = self.next_id();
            let tab = Tab::new(id, path, &self.settings);
            self.active_pane_mut().tabs.push(tab);
            self.persist();
            cx.notify();
        }
    }

    pub fn open_from_file_pane(
        &mut self, source_tab: u64, path: PathBuf, window: &mut Window, cx: &mut Context<Self>,
    ) {
        let Some((pane, _)) = self.tab_position(source_tab) else { return };
        if self.active_pane != pane { self.activate_pane(pane, window, cx); }
        self.open_with_middle_click(path, window, cx);
    }
    pub fn open_in_other_pane(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.panes.len() < 2 {
            self.toggle_split(window, cx);
        }
        if let Some(other) = self.pane_layout.sibling(self.active_pane) {
            self.active_pane = other;
        }
        self.navigate(path, window, cx);
    }

    /// Suelta una pestaña arrastrada en una zona de la carpeta actual.
    fn tab_preview_position(&self, id: u64, target: Option<(u64, u64)>) -> f32 {
        let Some((source, target)) = target else { return 0.0 };
        let Some((pane, from)) = self.tab_position(source) else { return 0.0 };
        let Some((dest, to)) = self.tab_position(target) else { return 0.0 };
        let Some((owner, index)) = self.tab_position(id) else { return 0.0 };
        if owner != pane || dest != pane || id == source { return 0.0; }
        let stride = crate::ui::TAB_WIDTH + crate::ui::TAB_GAP;
        if from < index && index <= to { -stride }
        else if to <= index && index < from { stride }
        else { 0.0 }
    }

    pub fn tab_reorder_offset(&self, id: u64) -> f32 {
        self.tab_preview_position(id, self.tab_reorder_target)
            + self.tab_reorder_offsets.get(&id).map_or(0.0, |(started, offset)| {
                offset * (1.0 - started.elapsed().as_secs_f32() / 0.16).clamp(0.0, 1.0).powi(3)
            })
    }

    pub fn preview_tab_reorder(&mut self, target: Option<(u64, u64)>) -> bool {
        if self.tab_reorder_target == target { return false; }
        let offsets: Vec<_> = self.panes.iter().flat_map(|pane| &pane.tabs).filter_map(|tab| {
            let before = self.tab_preview_position(tab.id, self.tab_reorder_target);
            let after = self.tab_preview_position(tab.id, target);
            (before != after).then(|| (tab.id, self.tab_reorder_offset(tab.id) - after))
        }).collect();
        self.tab_reorder_target = target;
        let now = Instant::now();
        self.tab_reorder_offsets.extend(offsets.into_iter().map(|(id, offset)| (id, (now, offset))));
        true
    }

    pub fn reorder_tab(
        &mut self, source_id: u64, target_id: u64, window: &mut Window, cx: &mut Context<Self>,
    ) {
        if source_id == target_id { return; }
        let Some((source, from)) = self.tab_position(source_id) else { return };
        let Some((target, _)) = self.tab_position(target_id) else { return };
        if source != target {
            let label = folder_name(&self.panes[source].tabs[from].path);
            self.drop_tab(&DragTab { tab_id: source_id, label }, target, TabDropZone::Center, window, cx);
        }
        let Some((pane_index, from)) = self.tab_position(source_id) else { return };
        let Some((target_pane, to)) = self.tab_position(target_id) else { return };
        if pane_index != target_pane { return; }

        let stride = crate::ui::TAB_WIDTH + crate::ui::TAB_GAP;
        let positions: Vec<_> = self.panes[pane_index].tabs.iter().enumerate()
            .map(|(index, tab)| (tab.id, index as f32 * stride + self.tab_reorder_offset(tab.id)))
            .collect();
        self.tab_reorder_target = None;
        let pane = &mut self.panes[pane_index];
        let active_id = pane.active_tab().id;
        let tab = pane.tabs.remove(from);
        pane.tabs.insert(to, tab);
        pane.active = pane.tabs.iter().position(|tab| tab.id == active_id).unwrap_or(0);
        let now = Instant::now();
        for (index, tab) in pane.tabs.iter().enumerate() {
            if let Some((_, before)) = positions.iter().find(|(id, _)| *id == tab.id) {
                self.tab_reorder_offsets.insert(tab.id, (now, before - index as f32 * stride));
            }
        }
        self.persist();
        cx.notify();
    }

    fn remove_pane(&mut self, index: usize) {
        self.panes.remove(index);
        let layout = std::mem::replace(&mut self.pane_layout, PaneLayout::Leaf(0));
        self.pane_layout = layout.without(index).expect("at least one pane remains");
    }

    fn add_split_pane(&mut self, target: usize, tab: Tab, axis: Axis, before: bool) {
        let index = self.panes.len();
        let split = self.pane_layout.split(target, index, axis, before);
        debug_assert!(split);
        self.panes.push(Pane::new(tab));
        self.active_pane = index;
    }

    pub fn drop_folder_split(
        &mut self, files: &DragFiles, target: usize, zone: TabDropZone,
        window: &mut Window, cx: &mut Context<Self>,
    ) {
        let (Some((axis, before)), [path]) = (zone.split(), files.paths.as_ref()) else { return };
        if !files.folder || !path.is_dir() || target >= self.panes.len() { return; }
        let id = self.next_id();
        let tab = Tab::new(id, path.clone(), &self.settings);
        self.add_split_pane(target, tab, axis, before);
        self.remember_path(path, cx);
        self.start_scan_tab(id, cx);
        self.finish_tab_drop(window, cx);
    }

    pub(crate) fn can_drop_tab_in_pane(&self, source: usize, target: usize) -> bool {
        target < self.panes.len() && (source != target || self.panes[source].tabs.len() > 1)
    }

    pub fn drop_tab(
        &mut self, drag: &DragTab, mut target: usize, zone: TabDropZone,
        window: &mut Window, cx: &mut Context<Self>,
    ) {
        let Some((source, index)) = self.tab_position(drag.tab_id) else { return };
        if !self.can_drop_tab_in_pane(source, target) { return; }
        let split = zone.split();
        if source == target && split.is_none() {
            self.switch_tab(source, index, window, cx);
            return;
        }

        let pane = &mut self.panes[source];
        let tab = pane.tabs.remove(index);
        pane.active = pane.active.saturating_sub(usize::from(index < pane.active))
            .min(pane.tabs.len().saturating_sub(1));
        if pane.tabs.is_empty() {
            self.remove_pane(source);
            target -= usize::from(source < target);
        }

        if let Some((axis, before)) = split {
            self.add_split_pane(target, tab, axis, before);
        } else {
            let pane = &mut self.panes[target];
            pane.tabs.push(tab);
            pane.active = pane.tabs.len() - 1;
            self.active_pane = target;
        }
        self.finish_tab_drop(window, cx);
    }

    fn finish_tab_drop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_drag();
        let path = self.active_tab().path.clone();
        self.address.update(cx, |state, cx| {
            state.set_value(address_text(&path), window, cx);
        });
        self.persist();
        self.restore_folder_view();
        self.sync_search_field(window, cx);
        self.refresh_preview(cx);
        self.release_inactive_thumbnails();
        self.focus_root(window);
        cx.notify();
    }

    // ------------------------------------------------------------------
    // Operaciones de archivos
    // ------------------------------------------------------------------

    pub fn is_cut(&self, path: &Path) -> bool {
        self.cut_clipboard.as_ref().is_some_and(|(_, paths)| paths.contains(path))
    }

    pub fn copy_selection(&mut self, cut: bool, window: &mut Window, cx: &mut Context<Self>) {
        let paths = self.active_tab().selected_paths();
        if paths.is_empty() {
            self.toast(tr("Selecciona al menos un elemento.", "Select at least one item."), ToastTone::Info, cx);
            return;
        }
        self.copy_paths(paths, cut, window, cx);
    }

    pub(crate) fn copy_paths(&mut self, paths: Vec<PathBuf>, cut: bool, window: &mut Window, cx: &mut Context<Self>) {
        match platform::clipboard_write_files(window, &paths, cut) {
            Ok(()) => {
                let count = paths.len();
                self.cut_clipboard = cut.then(|| (platform::clipboard_sequence(), paths.into_iter().collect()));
                self.toast(
                    if cut {
                        crate::localized!(format, "{count} elemento(s) listos para mover.", "Items ready to move: {count}.")
                    } else {
                        crate::localized!(format, "{count} elemento(s) copiados.", "Items copied: {count}.")
                    },
                    ToastTone::Success,
                    cx,
                );
            }
            Err(error) => self.toast(error, ToastTone::Error, cx),
        }
    }

    pub fn paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.paste_into(self.active_tab().path.clone(), window, cx);
    }

    pub(crate) fn paste_into(&mut self, destination: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let (paths, cut) = match platform::clipboard_read_files(window) {
            Ok(value) => value,
            Err(error) => {
                self.toast(error, ToastTone::Error, cx);
                return;
            }
        };
        if paths.is_empty() {
            self.toast(tr("El portapapeles no contiene archivos.", "The clipboard does not contain files."), ToastTone::Info, cx);
            return;
        }
        let clipboard_sequence = platform::clipboard_sequence();
        let directories = transfer_directories(&paths, &destination, cut);
        let track_undo = cut && self.settings.undo_file_operations;
        let this = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let undo = if track_undo { FileUndo::moves(&paths, &destination) } else { Vec::new() };
                    let result = if cut {
                        platform::move_items(&paths, &destination)
                    } else {
                        platform::copy_items(&paths, &destination)
                    };
                    let undo = undo.into_iter().filter(|step|
                        !step.to.exists() && platform::file_identity(&step.from).ok() == Some(step.identity)).collect();
                    (result, undo)
                })
                .await;
            this.update(cx, |this, cx| {
                let (result, undo) = result;
                this.record_file_undo(undo);
                match result {
                    Ok(0) => this.toast(tr("Los elementos ya están en esta carpeta.", "The items are already in this folder."), ToastTone::Info, cx),
                    Ok(_) => {
                        if cut && this.cut_clipboard.as_ref().is_some_and(|(sequence, _)| *sequence == clipboard_sequence) {
                            this.cut_clipboard = None;
                        }

                        this.toast(
                            if cut { tr("Elementos movidos.", "Items moved.") } else { tr("Elementos pegados.", "Items pasted.") },
                            ToastTone::Success,
                            cx,
                        );
                        this.refresh_directories(&directories, cx);
                    }
                    Err(error) => this.toast(error, ToastTone::Error, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn copy_path(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        match platform::clipboard_write_text(window, &fs_model::normalize_path(path)) {
            Ok(()) => self.toast(tr("Ruta copiada.", "Path copied."), ToastTone::Success, cx),
            Err(error) => self.toast(error, ToastTone::Error, cx),
        }
    }

    fn record_file_undo(&mut self, operation: Vec<FileUndo>) {
        if self.settings.undo_file_operations && !operation.is_empty() {
            if self.file_undo.len() >= 32 { self.file_undo.remove(0); }
            self.file_undo.push(operation);
        }
    }

    pub(crate) fn toggle_file_undo(&mut self, cx: &mut Context<Self>) {
        self.settings.undo_file_operations = !self.settings.undo_file_operations;
        if !self.settings.undo_file_operations { self.file_undo.clear(); }
        self.settings.save();
        cx.notify();
    }

    fn undo_file_operation(&mut self, cx: &mut Context<Self>) {
        if self.undo_in_progress { return; }
        let Some(operation) = self.file_undo.pop() else {
            self.toast(tr("No hay operaciones que deshacer.", "There are no operations to undo."), ToastTone::Info, cx);
            return;
        };
        self.undo_in_progress = true;
        cx.spawn(async move |this, cx| {
            let (pending, directories, restored) = cx.background_executor().spawn(async move {
                let mut pending = Vec::new();
                let mut directories = Vec::new();
                let mut restored = 0;
                for step in operation.into_iter().rev() {
                    if platform::file_identity(&step.from).ok() != Some(step.identity)
                        || platform::undo_move(&step.from, &step.to).is_err()
                    {
                        pending.push(step);
                        continue;
                    }
                    if let Some(meta) = step.recycle_meta {
                        let _ = std::fs::remove_file(meta);
                        directories.push(PathBuf::from(fs_model::RECYCLE_BIN_PATH));
                    }
                    directories.extend([step.from.parent(), step.to.parent()].into_iter().flatten().map(Path::to_path_buf));
                    restored += 1;
                }
                pending.reverse();
                (pending, directories, restored)
            }).await;
            let _ = this.update(cx, |this, cx| {
                this.undo_in_progress = false;
                if pending.is_empty() {
                    this.toast(tr("Operación deshecha.", "Operation undone."), ToastTone::Success, cx);
                } else {
                    if this.settings.undo_file_operations { this.file_undo.insert(0, pending); }
                    this.toast(crate::localized!(format, "Elementos recuperados: {restored}. Alguna ruta cambió o está ocupada.", "Items recovered: {restored}. A path changed or is in use."), ToastTone::Error, cx);
                }
                this.refresh_directories(&directories, cx);
                this.refresh_preview(cx);
                cx.notify();
            });
        }).detach();
    }

    pub fn delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return;
        }
        if self.in_recycle_bin() {
            let items = self.selected_recycle_items();
            if items.is_empty() {
                self.toast(tr("No hay nada seleccionado.", "Nothing is selected."), ToastTone::Info, cx);
                return;
            }
            self.perform_permanent_delete(items, window, cx);
            return;
        }
        let paths = self.active_tab().selected_paths();
        if paths.is_empty() {
            self.toast(tr("No hay nada seleccionado.", "Nothing is selected."), ToastTone::Info, cx);
            return;
        }
        self.perform_delete(paths, window, cx);
    }

    /// Mueve a la Papelera usando el diálogo nativo de Windows. El aviso
    /// propio de Xool Files ya no se interpone entre el usuario y el sistema.
    pub fn perform_delete(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owner = platform::hwnd_value(window);
        let confirm = self.settings.confirm_delete;
        let track_undo = self.settings.undo_file_operations;
        let mut directories: Vec<_> = paths.iter().filter_map(|path| path.parent())
            .map(Path::to_path_buf).collect();
        directories.push(PathBuf::from(fs_model::RECYCLE_BIN_PATH));
        let this = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let originals: Vec<_> = if track_undo {
                        paths.iter().filter_map(|path| platform::file_identity(path).ok()
                            .map(|identity| (path, identity))).collect()
                    } else { Vec::new() };
                    let result = platform::delete_to_recycle(&paths, owner, confirm);
                    let mut undo = Vec::new();
                    if !originals.is_empty() {
                        for entry in fs_model::scan_recycle_bin().entries {
                            let Some(item) = recycle_item_from_entry(&entry) else { continue; };
                            let Some((_, identity)) = originals.iter().find(|(path, _)|
                                path.as_os_str().eq_ignore_ascii_case(item.origin.as_os_str()))
                            else { continue; };
                            if !item.origin.exists()
                                && platform::file_identity(&item.recycled).ok() == Some(*identity)
                            {
                                undo.push(FileUndo { from: item.recycled, to: item.origin,
                                    recycle_meta: Some(item.meta), identity: *identity });
                            }
                        }
                    }
                    (result, paths, undo)
                })
                .await;
            this.update(cx, |this, cx| {
                let (result, paths, undo) = result;
                this.record_file_undo(undo);
                match result {
                    Ok(true) => {
                        this.remove_confirmed_entries(&paths);
                        this.refresh_preview(cx);
                        this.refresh_quick_search(cx);
                        this.toast(tr("Elementos movidos a la Papelera.", "Items moved to the Recycle Bin."), ToastTone::Success, cx);
                    }
                    Ok(false) => {}
                    Err(error) => this.toast(error, ToastTone::Error, cx),
                }
                this.refresh_directories(&directories, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ------------------------------------------------------------------
    // Papelera
    // ------------------------------------------------------------------

    fn remove_confirmed_entries(&mut self, paths: &[PathBuf]) {
        let ignore_accents = self.settings.search_ignore_accents;
        let removed: HashSet<&Path> = paths.iter().map(PathBuf::as_path).collect();
        for pane in &mut self.panes {
            for tab in &mut pane.tabs {
                // Tree entries carry indexes into the original root list. A scan
                // reconciles the whole tree in one step when it is expanded.
                if tab.tree_root_count.is_some() { continue; }
                let count = tab.entries.len();
                tab.entries.retain_mut(|entry| {
                    if !removed.contains(entry.path.as_path()) { return true; }
                    self.image_drops.extend(entry.thumb.take());
                    false
                });
                if tab.entries.len() != count {
                    tab.refresh_filter(ignore_accents);
                }
            }
        }
    }

    pub fn in_recycle_bin(&self) -> bool {
        fs_model::is_recycle_bin(&self.active_tab().path)
    }

    fn selected_recycle_items(&self) -> Vec<fs_model::RecycleItem> {
        let tab = self.active_tab();
        tab.entries
            .iter()
            .filter(|entry| tab.selection.contains(&entry.path))
            .filter_map(recycle_item_from_entry)
            .collect()
    }

    fn recycle_items_for_paths(&self, paths: &[PathBuf]) -> Vec<fs_model::RecycleItem> {
        let tab = self.active_tab();
        paths
            .iter()
            .filter_map(|path| tab.entry_for(path))
            .filter_map(recycle_item_from_entry)
            .collect()
    }

    pub fn restore_recycle_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let items = self.recycle_items_for_paths(&paths);
        if items.is_empty() {
            self.toast(tr("No se pudo preparar la restauración.", "Could not prepare the restore operation."), ToastTone::Error, cx);
            return;
        }
        self.perform_restore(items, cx);
    }

    pub fn restore_selected(&mut self, cx: &mut Context<Self>) {
        let items = self.selected_recycle_items();
        if items.is_empty() {
            self.toast(tr("Selecciona un elemento para restaurarlo.", "Select an item to restore."), ToastTone::Info, cx);
            return;
        }
        self.perform_restore(items, cx);
    }

    pub fn restore_all(&mut self, cx: &mut Context<Self>) {
        let items: Vec<fs_model::RecycleItem> = self
            .active_tab()
            .entries
            .iter()
            .filter_map(recycle_item_from_entry)
            .collect();
        if items.is_empty() {
            self.toast(tr("La Papelera está vacía.", "The Recycle Bin is empty."), ToastTone::Info, cx);
            return;
        }
        self.perform_restore(items, cx);
    }

    fn perform_restore(&mut self, items: Vec<fs_model::RecycleItem>, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { fs_model::restore_recycle_items(&items) })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(count) => {
                        this.toast(
                            if count == 1 {
                                tr("1 elemento restaurado.", "1 item restored.").to_string()
                            } else {
                                crate::localized!(format, "{count} elementos restaurados.", "{count} items restored.")
                            },
                            ToastTone::Success,
                            cx,
                        );
                    }
                    Err(error) => this.toast(error, ToastTone::Error, cx),
                }
                this.refresh_all(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn perform_permanent_delete(
        &mut self,
        items: Vec<fs_model::RecycleItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if items.is_empty() {
            return;
        }
        if self.settings.confirm_delete {
            self.dialog = Some(Dialog::PermanentDelete(items));
            self.focus_root(window);
            cx.notify();
        } else {
            self.delete_recycled_items(items, cx);
        }
    }

    fn delete_recycled_items(&mut self, items: Vec<fs_model::RecycleItem>, cx: &mut Context<Self>) {
        let entity = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (removed, error) = cx.background_executor().spawn(async move {
                fs_model::delete_recycle_items(&items)
            }).await;

            let _ = entity.update(cx, |this, cx| {
                let ignore_accents = this.settings.search_ignore_accents;
                for pane in &mut this.panes {
                    for tab in &mut pane.tabs {
                        if fs_model::is_recycle_bin(&tab.path) && !removed.is_empty() {
                            tab.entries.retain(|entry| !removed.contains(&entry.path));
                            tab.refresh_filter(ignore_accents);
                        }
                    }
                }
                if let Some(error) = error {
                    this.toast(error, ToastTone::Error, cx);
                } else {
                    let count = removed.len();
                    let message = if count == 1 {
                        tr("1 elemento eliminado definitivamente.", "1 item permanently deleted.").to_string()
                    } else {
                        crate::localized!(format, "{count} elementos eliminados definitivamente.", "{count} items permanently deleted.")
                    };
                    this.toast(message, ToastTone::Success, cx);
                }
                if !removed.is_empty() { this.refresh_drive_usage(cx); }
                // Una lectura anterior podría terminar con datos obsoletos.
                let recycle = [PathBuf::from(fs_model::RECYCLE_BIN_PATH)];
                this.refresh_directories(&recycle, cx);
                this.refresh_preview(cx);
                cx.notify();
            });
        }).detach();
    }

    pub fn confirm_empty_recycle_bin(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_tab().entries.is_empty() {
            self.toast(tr("La Papelera ya está vacía.", "The Recycle Bin is already empty."), ToastTone::Info, cx);
            return;
        }
        self.perform_empty_recycle_bin(window, cx);
    }

    /// Vacía la Papelera con la ventana nativa de Windows (confirmación y
    /// progreso incluidos).
    pub fn perform_empty_recycle_bin(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let owner = platform::hwnd_value(window);
        let confirm = self.settings.confirm_delete;
        let this = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { platform::empty_recycle_bin(owner, confirm) })
                .await;
            this.update(cx, |this, cx| {
                let changed = !matches!(result, Ok(false));
                match result {
                    Ok(true) => {
                        this.toast(tr("Papelera vaciada.", "Recycle Bin emptied."), ToastTone::Success, cx);
                    }
                    Ok(false) => {}
                    Err(error) => this.toast(error, ToastTone::Error, cx),
                }
                if changed {
                    this.refresh_drive_usage(cx);
                    let recycle = [PathBuf::from(fs_model::RECYCLE_BIN_PATH)];
                    this.refresh_directories(&recycle, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn begin_inline_rename(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let is_dir = self.active_tab().entry_for(&path).map_or_else(|| path.is_dir(), |entry| entry.is_dir);
        let stem_end = if !is_dir && path.extension().is_some_and(|extension| !extension.is_empty()) {
            path.file_stem().map_or(name.len(), |stem| stem.to_string_lossy().len())
        } else {
            name.len()
        };
        self.dialog = None;
        self.rename_tab = Some(self.active_tab().id);
        self.rename_path = Some(path);
        self.rename_input.update(cx, |state, cx| {
            state.set_value(name, window, cx);
            state.select_prefix(stem_end, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    /// Confirma el nombre escrito en la celda o fila. Si el nombre no cambia,
    /// simplemente se cierra la edición.
    pub fn commit_inline_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rename_tab != Some(self.active_tab().id) {
            self.cancel_inline_rename(window, cx);
            return;
        }
        let Some(path) = self.rename_path.clone() else {
            return;
        };
        let name = self.rename_input.read(cx).value().to_string();
        let name = name.trim().to_string();
        if !valid_name(&name) {
            self.toast(tr("El nombre no es válido.", "The name is invalid."), ToastTone::Error, cx);
            return;
        }
        let Some(parent) = path.parent() else {
            return;
        };
        let destination = parent.join(&name);
        if destination == path {
            self.rename_path = None;
            self.focus_root(window);
            cx.notify();
            return;
        }
        if destination.exists() {
            self.toast(tr("Ya existe un elemento con ese nombre.", "An item with that name already exists."), ToastTone::Error, cx);
            return;
        }
        match std::fs::rename(&path, &destination) {
            Ok(()) => {
                self.rename_path = None;
                self.focus_root(window);
                if let Some(labels) = self.settings.file_labels.remove(&path) {
                    self.settings.file_labels.insert(destination.clone(), labels);
                    self.settings.save();
                }
                if self.settings.undo_file_operations {
                    self.record_file_undo(FileUndo::new(destination.clone(), path, None).into_iter().collect());
                }
                self.toast(tr("Elemento renombrado.", "Item renamed."), ToastTone::Success, cx);
                self.refresh_active(cx);
                self.select_path_later(destination);
            }
            Err(error) => {
                self.toast(crate::localized!(format, "No se pudo renombrar: {error}", "Could not rename: {error}"), ToastTone::Error, cx)
            }
        }
    }

    /// Cancela la edición sin tocar el nombre. No roba el foco porque también
    /// se llama cuando el usuario hace clic en otro control.
    pub fn cancel_inline_rename(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.rename_tab = None;
        if self.rename_path.take().is_some() {
            cx.notify();
        }
    }

    pub fn rename_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.active_tab().primary_path() {
            self.begin_inline_rename(path, window, cx);
        } else {
            self.toast(tr("Selecciona un elemento para renombrarlo.", "Select an item to rename."), ToastTone::Info, cx);
        }
    }

    pub fn open_new_folder_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let parent = self.active_tab().path.clone();
        self.dialog = Some(Dialog::NewFolder(parent));
        self.dialog_input.update(cx, |state, cx| {
            state.set_value(tr("Nueva carpeta", "New folder"), window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    pub(crate) fn begin_sidebar_group_creation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.creating_sidebar_group = true;
        self.dialog_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    pub(crate) fn cancel_sidebar_group_creation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.creating_sidebar_group = false;
        self.focus_root(window);
        cx.notify();
    }

    pub(crate) fn create_sidebar_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::settings::{CustomSidebarGroup, SidebarGroup};

        if !self.creating_sidebar_group || self.settings_closing { return; }
        let name = self.dialog_input.read(cx).value();
        let name = name.trim();
        if name.is_empty() {
            self.toast(tr("Escribe un nombre para el grupo.", "Enter a group name."), ToastTone::Error, cx);
            self.dialog_input.update(cx, |state, cx| state.focus(window, cx));
            return;
        }
        let id = self.settings.next_sidebar_group_id;
        let Some(next) = id.checked_add(1) else {
            self.toast(tr("No se pudo crear el grupo.", "Could not create the group."), ToastTone::Error, cx);
            return;
        };
        self.settings.next_sidebar_group_id = next;
        self.settings.custom_sidebar_groups.push(CustomSidebarGroup {
            id, name: name.to_owned(), folders: Vec::new(), collapsed: false,
        });
        self.settings.sidebar_groups.push(SidebarGroup::Custom(id));
        self.settings.save();
        self.cancel_sidebar_group_creation(window, cx);
    }

    pub(crate) fn name_sidebar_group(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(group) = self.settings.custom_sidebar_group(id) else { return };
        let name = group.name.clone();
        self.creating_sidebar_group = false;
        self.dialog = Some(Dialog::SidebarGroupName(id));
        self.dialog_input.update(cx, |state, cx| {
            state.set_value(name, window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    pub(crate) fn add_sidebar_group_folders(&mut self, group: u64, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false, directories: true, multiple: true,
            prompt: Some(tr("Añadir carpetas al grupo", "Add folders to the group").into()),
        });
        cx.spawn(async move |this, cx| match receiver.await {
            Ok(Ok(Some(paths))) => {
                let _ = this.update(cx, |this, cx| this.pin_sidebar_group_folders(group, &paths, cx));
            }
            Ok(Err(error)) => {
                let _ = this.update(cx, |this, cx| this.toast(
                    crate::localized!(format, "No se pudo abrir el selector de carpetas: {error}", "Could not open the folder picker: {error}"), ToastTone::Error, cx));
            }
            _ => {}
        }).detach();
    }

    pub(crate) fn pin_sidebar_group_folders(&mut self, id: u64, paths: &[PathBuf], cx: &mut Context<Self>) {
        let Some(group) = self.settings.custom_sidebar_groups.iter_mut().find(|group| group.id == id) else { return };
        let before = group.folders.len();
        for path in paths {
            if path.is_absolute() && path.is_dir() && !group.folders.iter().any(|folder| same_favorite_path(folder, path)) {
                group.folders.push(path.clone());
            }
        }
        if group.folders.len() != before {
            group.collapsed = false;
            self.settings.save();
            cx.notify();
        }
    }

    pub fn submit_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.dialog.clone() else {
            return;
        };
        match dialog {
            Dialog::NewFolder(parent) => {
                let name = self.dialog_input.read(cx).value().to_string();
                let name = name.trim().to_string();
                if !valid_name(&name) {
                    self.toast(tr("El nombre no es válido.", "The name is invalid."), ToastTone::Error, cx);
                    return;
                }
                let destination = fs_model::unique_path(&parent, &name);
                match std::fs::create_dir(&destination) {
                    Ok(()) => {
                        self.dialog = None;
                        self.focus_root(window);
                        self.toast(tr("Carpeta creada.", "Folder created."), ToastTone::Success, cx);
                        self.refresh_active(cx);
                        self.select_path_later(destination);
                    }
                    Err(error) => self.toast(crate::localized!(format, "No se pudo crear la carpeta: {error}", "Could not create the folder: {error}"), ToastTone::Error, cx),
                }
            }
            Dialog::SidebarGroupName(id) => {
                let name = self.dialog_input.read(cx).value();
                let name = name.trim();
                if name.is_empty() {
                    self.toast(tr("Escribe un nombre para el grupo.", "Enter a group name."), ToastTone::Error, cx);
                    return;
                }
                if let Some(group) = self.settings.custom_sidebar_groups.iter_mut().find(|group| group.id == id) {
                    group.name = name.to_owned();
                    self.settings.save();
                }
                self.close_dialog(window, cx);
            }
            Dialog::OpenMultiple(paths) => {
                self.close_dialog(window, cx);
                for path in paths { self.open_file(path, cx); }
            }
            Dialog::PermanentDelete(items) => {
                self.close_dialog(window, cx);
                self.delete_recycled_items(items, cx);
            }
            Dialog::Settings | Dialog::Message { .. } => self.close_dialog(window, cx),
        }
    }

    fn select_path_later(&mut self, path: PathBuf) {
        let tab = self.active_tab_mut();
        tab.selection.clear();
        tab.selection.insert(path.clone());
        tab.cursor = Some(path);
    }

    pub fn initialize_integration(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use futures::StreamExt;
        let mut tray = crate::tray::events(window);
        if self.settings.show_tray_icon {
            if let Err(error) = crate::tray::set_visible(window, true) {
                self.integration_message = Some(error);
            }
        }
        self.tray_task = Some(cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = tray.next().await {
                let exiting = matches!(event, crate::tray::Event::Exit);
                if this.update_in(cx, |this, window, cx| match event {
                    crate::tray::Event::Show(folder) => {
                        crate::tray::show(window);
                        if let Some(path) = folder.filter(|path| path.is_dir()) {
                            this.navigate(path, window, cx);
                        }
                    }
                    crate::tray::Event::Exit => {
                        this.persist_on_close(window);
                        this.close_quick_search(cx);
                        window.remove_window();
                    }
                }).is_err() { break; }
                if exiting { break; }
            }
        }));
        let index = self.search_index.clone();
        if self.settings.search_live_index { index.set_live(true); }
        self.search_index_task = Some(cx.spawn(async move |this, cx| {
            let mut previous = index.status();
            loop {
                if previous.ready { index.schedule(false); }
                // Check status without making the UI thread scan a volume.
                for _ in 0..60 {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    let status = index.status();
                    if status != previous && this.update(cx, |_, cx| cx.notify()).is_err() {
                        return;
                    }
                    previous = status;
                }
            }
        }));
        let mut receiver = platform::quick_search_events(window);
        if let Err(error) = platform::register_quick_search(window, self.settings.quick_search_shortcut.as_deref()) {
            self.quick_search_message = Some(error);
        }
        self.quick_hotkey_task = Some(cx.spawn_in(window, async move |this, cx| {
            while receiver.next().await.is_some() {
                if this.update_in(cx, |this, window, cx| {
                    if this.recording_shortcut == Some(shortcuts::QUICK_SEARCH_RECORDING) {
                        this.recording_shortcut = None;
                        this.quick_search_message = None;
                        cx.notify();
                        return;
                    }
                    this.open_quick_search(window, cx);
                }).is_err() { break; }
            }
        }));
    }

    pub(crate) fn reindex_search(&mut self, cx: &mut Context<Self>) {
        self.search_index.schedule(true);
        cx.notify();
    }

    pub(crate) fn toggle_live_search_index(&mut self, cx: &mut Context<Self>) {
        self.settings.search_live_index = !self.settings.search_live_index;
        self.settings.save();
        self.search_index.set_live(self.settings.search_live_index);
        cx.notify();
    }

    pub(crate) fn set_quick_key(&mut self, keys: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        match platform::register_quick_search(window, keys) {
            Ok(()) => {
                self.quick_search_message = None;
                self.recording_shortcut = None;
                self.settings.quick_search_shortcut = keys.map(shortcuts::normalize);
                self.settings.save();
            }
            Err(error) => {
                self.quick_search_message = Some(error);
            }
        }
        cx.notify();
    }

    pub(crate) fn request_window_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let keep_in_background = !self.settings.temporary_window && self.settings.background_on_close;
        if !keep_in_background
            && self.text_edit.as_ref().is_some_and(|(_, original, input)|
                input.read(cx).value().as_str() != preview_editor_text(original))
        {
            self.reveal_pending_text_edit(cx);
            return false;
        }
        self.persist_on_close(window);
        self.close_quick_search(cx);
        if !keep_in_background { return true; }
        if !crate::tray::hide_if_reopenable(window) {
            window.minimize_window();
        }
        false
    }

    pub(crate) fn toggle_windows_startup(&mut self, cx: &mut Context<Self>) {
        self.integration_message = crate::integration::set_windows_startup(!self.windows_startup).err();
        if let Ok(enabled) = crate::integration::starts_with_windows() {
            self.windows_startup = enabled;
        }
        cx.notify();
    }

    pub(crate) fn toggle_tray_icon(&mut self, window: &Window, cx: &mut Context<Self>) {
        let visible = !self.settings.show_tray_icon;
        self.integration_message = crate::tray::set_visible(window, visible).err();
        if self.integration_message.is_none() {
            self.settings.show_tray_icon = visible;
            self.settings.save();
        }
        cx.notify();
    }

    pub fn toggle_folder_default(&mut self, cx: &mut Context<Self>) {
        self.integration_message = crate::integration::set_folder_default(!self.folder_default).err();
        self.folder_default = crate::integration::folder_default();
        cx.notify();
    }

    pub fn open_quick_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.recording_shortcut = None;
        self.close_context_menu(cx);
        let owner = cx.weak_entity();
        let main = window.window_handle();
        let monitor = platform::SearchMonitor::current(window, cx);
        cx.defer(move |cx| {
            let Some(entity) = owner.upgrade() else { return; };
            let existing = entity.read(cx).quick_window.clone();
            if existing.is_some_and(|popup| popup.activate(monitor, cx)) {
                return;
            }
            match crate::quick_search::QuickSearchWindow::open(&entity, main, monitor, cx) {
                Ok(popup) => entity.update(cx, |app, _| app.quick_window = Some(popup)),
                Err(error) => entity.update(cx, |app, cx| app.toast(
                    crate::localized!(format, "No se pudo abrir la búsqueda: {error}", "Could not open search: {error}"), ToastTone::Error, cx)),
            }
        });
    }

    pub(crate) fn close_quick_search(&mut self, cx: &mut Context<Self>) {
        if let Some(popup) = self.quick_window.take() {
            cx.defer(move |cx| popup.close(cx));
        }
    }

    pub(crate) fn refresh_quick_search(&self, cx: &mut Context<Self>) {
        if let Some(popup) = self.quick_window.clone() {
            cx.defer(move |cx| popup.refresh(cx));
        }
    }

    pub(crate) fn open_quick_location(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if path.is_dir() {
            self.dialog = None;
            crate::tray::show(window);
        }
        self.open_location(path, window, cx);
    }

    pub(crate) fn add_startup_folders(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false, directories: true, multiple: true,
            prompt: Some(tr("Elegir carpetas de inicio", "Choose startup folders").into()),
        });
        cx.spawn(async move |this, cx| {
            match receiver.await {
                Ok(Ok(Some(paths))) => {
                    let _ = this.update(cx, |this, cx| {
                        let folders = &mut this.settings.startup_folders;
                        let before = folders.len();
                        for path in paths {
                            if folders.len() >= crate::settings::MAX_STARTUP_FOLDERS { break; }
                            if path.is_absolute() && !folders.iter().any(|folder| same_favorite_path(folder, &path)) {
                                folders.push(path);
                            }
                        }
                        if folders.len() != before {
                            this.settings.save();
                            cx.notify();
                        }
                    });
                }
                Ok(Err(error)) => {
                    let _ = this.update(cx, |this, cx| {
                        this.toast(crate::localized!(format, "No se pudo abrir el selector de carpetas: {error}", "Could not open the folder picker: {error}"), ToastTone::Error, cx);
                    });
                }
                _ => {}
            }
        }).detach();
    }

    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.temporary_window {
            self.toast(tr("Abre Ajustes en la ventana principal de Xool Files.", "Open Settings in the main Xool Files window."), ToastTone::Info, cx);
            return;
        }
        if !matches!(self.dialog, Some(Dialog::Settings)) || self.settings_closing {
            self.settings_animation = self.settings_animation.wrapping_add(1);
            self.settings_closing = false;
            self.settings_theme_open = false;
            self.wheel_scroll = None;
            self.settings_page_scroll = gpui::ScrollHandle::new();
            self.settings_nav_scroll = gpui::ScrollHandle::new();
            self.dialog = Some(Dialog::Settings);
            self.settings_search.update(cx, |state, cx| state.set_value("", window, cx));
            self.shortcut_search.update(cx, |state, cx| state.set_value("", window, cx));
            if self.preferences_page == ABOUT_SETTINGS_PAGE {
                self.refresh_storage_info(cx);
            }
        }
        self.focus_root(window);
        cx.notify();
    }

    pub fn toggle_settings_navigation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_closing { return; }
        self.settings_nav_collapsed = !self.settings_nav_collapsed;
        self.settings_nav_animation = Some((Instant::now(), self.settings_nav_progress));
        if self.settings_nav_collapsed {
            self.focus_root(window);
            if self.wheel_scroll.is_some_and(|motion| motion.target == WheelScrollTarget::SettingsNavigation) {
                self.wheel_scroll = None;
            }
        }
        cx.notify();
    }

    fn tick_settings_navigation(&mut self) -> bool {
        let Some((started, initial)) = self.settings_nav_animation else { return false };
        let target = if self.settings_nav_collapsed { 0. } else { 1. };
        let t = (started.elapsed().as_secs_f32() / 0.20).min(1.);
        let eased = 1. - (1. - t).powi(3);
        self.settings_nav_progress = initial + (target - initial) * eased;
        if t >= 1. { self.settings_nav_animation = None; }
        t < 1.
    }

    pub fn close_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.dialog, Some(Dialog::Settings)) {
            if self.settings_nav_drag.take().is_some() { self.settings.save(); }
            if self.settings_closing {
                return;
            }
            self.settings_closing = true;
            if self.workspace_ui.edit == Some(crate::workspaces::NameEdit::SettingsPreset) {
                self.workspace_ui.edit = None;
            }
            self.creating_sidebar_group = false;
            self.recording_shortcut = None;
            self.settings_animation = self.settings_animation.wrapping_add(1);
            let animation = self.settings_animation;
            let entity = cx.entity().downgrade();
            cx.spawn(async move |_, cx| {
                cx.background_executor().timer(Duration::from_millis(170)).await;
                let _ = entity.update(cx, |this, cx| {
                    if this.settings_animation == animation && matches!(this.dialog, Some(Dialog::Settings)) {
                        this.dialog = None;
                        this.settings_closing = false;
                        cx.notify();
                    }
                });
            }).detach();
        } else if matches!(self.dialog, Some(Dialog::SidebarGroupName(_))) {
            self.dialog = Some(Dialog::Settings);
        } else {
            self.dialog = None;
        }
        self.focus_root(window);
        cx.notify();
    }

    // ------------------------------------------------------------------
    // Menú contextual
    // ------------------------------------------------------------------

    pub fn hover_file_context_menu(
        &mut self, tab_id: u64, path: &Path, hovered: bool,
        window: &mut Window, cx: &mut Context<Self>,
    ) {
        let Some((pane, tab)) = self.tab_position(tab_id) else { return };
        let tab = &self.panes[pane].tabs[tab];
        if fs_model::is_recycle_bin(&tab.path) { return; }
        let target = ShellTarget::Selection(if tab.selection.contains(path) {
            tab.selected_paths()
        } else {
            vec![path.to_path_buf()]
        });
        self.hover_context_menu(target, hovered, window, cx);
    }

    pub fn hover_location_context_menu(
        &mut self, path: &Path, hovered: bool, window: &Window, cx: &mut Context<Self>,
    ) {
        self.hover_context_menu(ShellTarget::SidebarLocation(path.to_path_buf()), hovered, window, cx);
    }

    fn hover_context_menu(
        &mut self, target: ShellTarget, hovered: bool, window: &Window, cx: &mut Context<Self>,
    ) {
        if !hovered {
            if self.menu_hover.as_ref().is_some_and(|(pending, _)| *pending == target) {
                self.menu_hover = None;
            }
            if self.prepared_menu.as_ref().is_some_and(|ready| ready.target == target) {
                self.prepared_menu = None;
            }
            return;
        }
        if !self.settings.shell_menu_actions || self.context_menu.is_some() || self.dialog.is_some()
            || cx.has_active_drag() || !window.is_window_active()
        {
            return;
        }
        if self.menu_hover.as_ref().is_some_and(|(pending, _)| *pending == target)
            || self.prepared_menu.as_ref().is_some_and(|ready| {
                ready.target == target && ready.started.elapsed() < Duration::from_secs(4)
            })
        {
            return;
        }
        let pending = target.clone();
        let owner = platform::hwnd_value(window);
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(140)).await;
            let _ = this.update(cx, |this, cx| {
                this.menu_hover = None;
                if this.context_menu.is_some() || this.dialog.is_some() || cx.has_active_drag() {
                    return;
                }
                let (shell, rows) = ShellClient::start(owner, pending.clone());
                this.prepared_menu = Some(PreparedContextMenu {
                    target: pending, started: Instant::now(), shell, rows,
                });
            });
        });
        self.menu_hover = Some((target, task));
    }

    pub fn open_pane_context_menu(
        &mut self,
        tab_id: u64,
        target: MenuTarget,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((pane, _)) = self.tab_position(tab_id) else { return };
        if self.active_pane != pane {
            self.activate_pane(pane, window, cx);
        }
        self.open_context_menu(
            target, f32::from(event.position.x), f32::from(event.position.y), window, cx,
        );
    }

    pub fn open_context_menu(
        &mut self,
        target: MenuTarget,
        x: f32,
        y: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let MenuTarget::Item(path) = &target {
            let selected = self.active_tab().selection.contains(path);
            if !selected {
                self.click_entry(path.clone(), gpui::Modifiers::default(), window, cx);
            }
        }
        let system_target = match &target {
            MenuTarget::Item(_) if !self.in_recycle_bin() => {
                Some(ShellTarget::Selection(self.active_tab().selected_paths()))
            }
            MenuTarget::Sidebar(path) => Some(ShellTarget::SidebarLocation(path.clone())),
            MenuTarget::Background if !self.in_recycle_bin() => {
                Some(ShellTarget::Background(self.active_tab().path.clone()))
            }
            _ => None,
        };
        self.menu_hover = None;
        self.dismiss_context_menu();
        self.sort_menu = false;
        self.context_menu_id = self.context_menu_id.wrapping_add(1);
        let mut menu = ContextMenu {
            id: self.context_menu_id, x, y, target,
            pages: Vec::new(), submenu_transition: None, scroll: gpui::ScrollHandle::new(),
            reveal_expanded: std::cell::Cell::new(false), system_rows: Some(Arc::from([])), submenus: HashMap::new(),
            shell: None, loading: None,
        };
        if let Some(target) = system_target.filter(|_| self.settings.shell_menu_actions) {
            let (shell, mut receiver) = self.prepared_menu.take()
                .filter(|ready| ready.target == target && ready.started.elapsed() < Duration::from_secs(4))
                .map(|ready| (ready.shell, ready.rows))
                .unwrap_or_else(|| ShellClient::start(platform::hwnd_value(window), target));
            menu.shell = Some(shell);
            match receiver.try_recv() {
                Ok(Some(mut rows)) => {
                    reuse_menu_icons(&mut rows, &mut self.context_menu_icons, &mut self.image_drops);
                    menu.system_rows = Some(rows);
                }
                Ok(None) => {
                    menu.system_rows = None;
                    menu.loading = Some(load_shell_rows(menu.id, None, receiver, cx));
                }
                Err(_) => { menu.shell = None; }
            }
        }
        self.context_menu = Some(menu);
        self.focus_root(window);
        cx.notify();
    }

    pub fn cancel_pending_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.context_menu.as_ref().is_some_and(|menu| menu.system_rows.is_none()) {
            self.dismiss_context_menu();
            cx.notify();
        }
    }

    fn dismiss_context_menu(&mut self) {
        self.context_menu = None;
    }

    fn toggle_context_submenu(&mut self, page: ContextPage, depth: usize, cx: &mut Context<Self>) {
        let Some(menu) = self.context_menu.as_mut() else { return };
        if depth > menu.pages.len() { return; }
        menu.loading = None;
        if menu.pages.get(depth) == Some(&page) {
            menu.change_submenu(depth, None);
        } else if let ContextPage::Shell(key) = page
            && !menu.submenus.contains_key(&key)
            && let Some(shell) = &menu.shell
        {
            menu.loading = Some(load_shell_rows(menu.id, Some((key, depth)), shell.expand(key), cx));
        } else {
            menu.change_submenu(depth, Some(page));
        }
        cx.notify();
    }

    pub fn close_context_menu(&mut self, cx: &mut Context<Self>) {
        self.dismiss_context_menu();
        self.sort_menu = false;
        self.view_menu = false;
        self.address_history_open = false;
        cx.notify();
    }

    pub fn perform_menu_action(
        &mut self,
        action: MenuAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            MenuAction::ContextSubmenu(page, depth) => {
                self.toggle_context_submenu(page, depth, cx);
                return;
            }
            MenuAction::ShellCommand(offset) | MenuAction::RotateImage(offset) => {
                let rotation = matches!(action, MenuAction::RotateImage(_));
                let paths = if rotation { self.active_tab().selected_paths() } else { Vec::new() };
                let result = self.context_menu.as_ref().and_then(|menu| menu.shell.as_ref())
                    .map(|shell| shell.invoke(offset, rotation));
                self.dismiss_context_menu();
                cx.notify();
                if let Some(result) = result {
                    cx.spawn(async move |this, cx| {
                        let result = receive_shell_reply(result, Duration::from_secs(12), cx).await;
                        let _ = this.update(cx, |this, cx| {
                            match result {
                                Some(Err(error)) => this.toast(error, ToastTone::Error, cx),
                                None => this.toast(tr("Windows no respondió a la acción.", "Windows did not respond to the action."), ToastTone::Error, cx),
                                Some(Ok(())) if rotation => this.refresh_rotated_images(&paths, cx),
                                Some(Ok(())) => {}
                            }
                        });
                    }).detach();
                }
                return;
            }
            _ => {}
        }
        self.dismiss_context_menu();
        self.sort_menu = false;
        cx.notify();
        match action {
            MenuAction::ContextSubmenu(..) | MenuAction::ShellCommand(_) | MenuAction::RotateImage(_) => unreachable!(),
            MenuAction::Open(path) => self.open_path(path, window, cx),
            MenuAction::OpenLocation(path) => self.open_location(path, window, cx),
            MenuAction::OpenWith(path) => platform::open_with(window, &path),
            MenuAction::OpenInNewTab(path) => self.open_in_new_tab(path, window, cx),
            MenuAction::OpenInNewWindow(path) => {
                if !path.is_dir() && !fs_model::is_recycle_bin(&path) {
                    self.toast(tr("La carpeta ya no está disponible.", "The folder is no longer available."), ToastTone::Info, cx);
                } else if let Err(error) = std::env::current_exe().and_then(|exe| {
                    std::process::Command::new(exe).arg("--new-window").arg(path).spawn()
                }) {
                    self.toast(crate::localized!(format, "No se pudo abrir la ventana: {error}", "Could not open the window: {error}"), ToastTone::Error, cx);
                }
            }
            MenuAction::AddToWorkspace(path) => {
                if !path.is_dir() {
                    self.toast(tr("La carpeta no está disponible.", "The folder is unavailable."), ToastTone::Error, cx);
                } else {
                    let end = self.settings.workspaces.current().rows.len();
                    if self.insert_workspace_shortcuts([path], end, false, cx) {
                        self.toast(tr("Carpeta añadida al espacio de trabajo.", "Folder added to the workspace."), ToastTone::Success, cx);
                    } else {
                        self.toast(tr("La carpeta ya está en este espacio de trabajo.", "The folder is already in this workspace."), ToastTone::Info, cx);
                    }
                }
            }
            MenuAction::EditAddress(path) => self.edit_address(path, window, cx),
            MenuAction::ToggleCompactPaths => {
                self.settings.compact_paths = !self.settings.compact_paths;
                self.settings.save();
            }
            MenuAction::TogglePathBreadcrumbs => {
                self.settings.path_breadcrumbs = !self.settings.path_breadcrumbs;
                self.settings.save();
            }
            MenuAction::OpenInOtherPane(path) => self.open_in_other_pane(path, window, cx),
            MenuAction::Properties(path) => platform::properties(window, &path),
            MenuAction::OpenTerminal(path) => platform::open_terminal(window, &path),
            MenuAction::CopyPath(path) => {
                let target = if self.in_recycle_bin() {
                    self.active_tab()
                        .entry_for(&path)
                        .and_then(|entry| entry.origin.clone())
                        .unwrap_or(path)
                } else {
                    path
                };
                self.copy_path(&target, window, cx);
            }
            MenuAction::CopySelection => self.copy_selection(false, window, cx),
            MenuAction::CutSelection => self.copy_selection(true, window, cx),
            MenuAction::Paste => self.paste(window, cx),
            MenuAction::DeleteSelection => self.delete_selected(window, cx),
            MenuAction::Rename(path) => self.begin_inline_rename(path, window, cx),
            MenuAction::NewFolder => self.open_new_folder_dialog(window, cx),
            MenuAction::Refresh => self.refresh_active(cx),
            MenuAction::SelectAll => self.select_all(cx),
            MenuAction::SetSort(key) => self.set_sort(key,cx),
            MenuAction::ToggleSortDesc => {
                let tab = self.active_tab_mut();
                tab.sort_desc = !tab.sort_desc;
                tab.sort_entries();
                self.refresh_active_filter();
                self.remember_folder_view();
                self.settings.save();
                cx.notify();
            }
            MenuAction::SetView(view) => {
                self.set_zoom(view.zoom_percent(self.settings.icon_scale), cx);
            }
            MenuAction::ToggleHidden => self.toggle_hidden(cx),
            MenuAction::ToggleSplit => self.toggle_split(window, cx),
            MenuAction::TogglePreview => self.toggle_preview(cx),
            MenuAction::AddFavorite(path) => self.add_favorite(path, cx),
            MenuAction::RemoveFavorite(path) => self.remove_favorite(&path, cx),
            MenuAction::SetFavoriteColor(path, color) => self.set_favorite_color(&path, color, cx),
            MenuAction::RevealRecentFile(path) => {
                if let Some(parent) = path.parent().filter(|parent| parent.is_dir()) {
                    self.open_in_new_tab(parent.to_path_buf(), window, cx);
                    self.select_path_later(path);
                } else {
                    self.toast(tr("La carpeta del archivo ya no está disponible.", "The file’s folder is no longer available."), ToastTone::Info, cx);
                }
            }
            MenuAction::ClearRecentFiles => self.clear_recent_files(cx),
            MenuAction::RestoreRecycle => self.restore_selected(cx),
            MenuAction::EmptyRecycleBin => self.confirm_empty_recycle_bin(window, cx),
            MenuAction::Settings => self.open_settings(window, cx),
            MenuAction::CloseTab => self.close_active_tab(window, cx),
            MenuAction::MoveTab(id, direction) => {
                if let Some((pane, index)) = self.tab_position(id)
                    && let Some(target) = index.checked_add_signed(direction).and_then(|next| self.panes[pane].tabs.get(next))
                {
                    self.reorder_tab(id, target.id, window, cx);
                }
            }
            MenuAction::CloseTabFromMenu(id) | MenuAction::CloseOtherTabs(id) | MenuAction::CloseTabsToRight(id) => {
                if let Some((pane, index)) = self.tab_position(id) {
                    self.close_tabs(pane, |candidate| match action {
                        MenuAction::CloseOtherTabs(_) => candidate != index,
                        MenuAction::CloseTabsToRight(_) => candidate > index,
                        _ => candidate == index,
                    }, window, cx);
                }
            }
        }
    }

    fn refresh_rotated_images(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let mut affected_tabs = Vec::new();
        for tab in self.panes.iter_mut().flat_map(|pane| &mut pane.tabs) {
            let mut changed = false;
            for entry in &mut tab.entries {
                if entry.kind == FileKind::Image && paths.contains(&entry.path) {
                    self.image_drops.extend(entry.thumb.take());
                    entry.icon = None;
                    changed = true;
                }
            }
            if changed {
                tab.thumbnail_task = None;
                tab.thumbnail_range = 0..0;
                affected_tabs.push(tab.id);
            }
        }
        if self.preview_path.as_ref().is_some_and(|path| paths.contains(path)) {
            self.preview_path = None;
            self.refresh_preview(cx);
        }
        for id in affected_tabs { self.start_scan_tab(id, cx); }
        cx.notify();
    }

    pub fn is_favorite(&self, path: &Path) -> bool {
        self.settings.favorites.iter().any(|favorite| same_favorite_path(&favorite.path, path))
    }

    pub fn add_favorite(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.is_favorite(&path) {
            self.toast(tr("Ya está en favoritos.", "Already in Favorites."), ToastTone::Info, cx);
            return;
        }
        let name = folder_name(&path);
        let palette = &crate::settings::favorite_colors();
        let color = palette[self.settings.favorites.len() % palette.len()].0;
        self.settings.favorites.push(crate::settings::Favorite { name, path, color });
        self.settings.favorites_collapsed = false;
        self.settings.save();
        self.toast(tr("Añadido a favoritos.", "Added to Favorites."), ToastTone::Success, cx);
        cx.notify();
    }

    fn set_favorite_color(&mut self, path: &Path, color: usize, cx: &mut Context<Self>) {
        if let Some(favorite) = self.settings.favorites.iter_mut().find(|item| same_favorite_path(&item.path, path)) {
            favorite.color = color;
            self.settings.save();
            cx.notify();
        }
    }

    fn favorite_drag_target(&self, source: &Path, to: usize) -> Option<(usize, usize)> {
        if !self.settings.reorder_sidebar_favorites || to >= self.settings.favorites.len() { return None; }
        let from = self.settings.favorites.iter().position(|item| same_favorite_path(&item.path, source))?;
        (from != to).then_some((from, to))
    }

    fn insertion_slot(index: usize, target: Option<(usize, usize)>) -> usize {
        let Some((from, to)) = target else { return index };
        if index == from { to }
        else if from < index && index <= to { index - 1 }
        else if to <= index && index < from { index + 1 }
        else { index }
    }

    pub(crate) fn sidebar_favorite_offset(&self, index: usize) -> f32 {
        let path = &self.settings.favorites[index].path;
        let slot = Self::insertion_slot(index, self.sidebar_favorite_target);
        let progress = self.sidebar_favorite_offsets.get(path).map_or(0.0, |(started, offset)| {
            offset * (1.0 - started.elapsed().as_secs_f32() / 0.16).max(0.0).powi(3)
        });
        (slot as f32 - index as f32) * self.settings.sidebar_row_height() + progress
    }

    fn preview_sidebar_favorite(&mut self, target: Option<(usize, usize)>) -> bool {
        if self.sidebar_favorite_target == target { return false; }
        let now = Instant::now();
        let height = self.settings.sidebar_row_height();
        let offsets: Vec<_> = self.settings.favorites.iter().enumerate().filter_map(|(index, favorite)| {
            let before = Self::insertion_slot(index, self.sidebar_favorite_target);
            let after = Self::insertion_slot(index, target);
            (before != after).then(|| (favorite.path.clone(),
                (now, self.sidebar_favorite_offset(index) - (after as f32 - index as f32) * height)))
        }).collect();
        self.sidebar_favorite_target = target;
        self.sidebar_favorite_offsets.extend(offsets);
        true
    }

    pub(crate) fn hover_sidebar_favorite(&mut self, source: &Path, index: usize, cx: &mut Context<Self>) {
        if self.preview_sidebar_favorite(self.favorite_drag_target(source, index)) {
            cx.notify();
        }
    }

    pub(crate) fn move_sidebar_favorite(&mut self, source: &Path, index: usize, cx: &mut Context<Self>) {
        let target = self.favorite_drag_target(source, index);
        self.preview_sidebar_favorite(target);
        if let Some((from, to)) = target {
            let favorites = &mut self.settings.favorites;
            let favorite = favorites.remove(from);
            favorites.insert(to, favorite);
            self.sidebar_favorite_target = None;
            self.settings.save();
        }
        cx.notify();
    }

    pub(crate) fn menu_tool_offset(&self, key: &str, index: usize) -> f32 {
        let slot = Self::insertion_slot(index, self.menu_tool_target);
        let offset = self.menu_tool_offsets.get(key).map_or(0., |(started, offset)| {
            offset * (1. - started.elapsed().as_secs_f32() / 0.16).max(0.).powi(3)
        });
        (slot as f32 - index as f32) * crate::ui::MENU_TOOL_ROW_HEIGHT + offset
    }

    fn preview_menu_tool_order(&mut self, order: &[String], target: Option<(usize, usize)>) -> bool {
        if self.menu_tool_target == target { return false; }
        let now = Instant::now();
        for (index, key) in order.iter().enumerate() {
            let before = Self::insertion_slot(index, self.menu_tool_target);
            let after = Self::insertion_slot(index, target);
            if before != after {
                let remaining = self.menu_tool_offset(key, index)
                    - (after as f32 - index as f32) * crate::ui::MENU_TOOL_ROW_HEIGHT;
                self.menu_tool_offsets.insert(key.clone(), (now, remaining));
            }
        }
        self.menu_tool_target = target;
        true
    }

    pub(crate) fn hover_menu_tool(&mut self, source: &str, target: usize, cx: &mut Context<Self>) {
        let order = crate::ui::context_tool_order(&self.settings);
        if let Some(from) = order.iter().position(|key| key == source)
            && self.preview_menu_tool_order(&order, Some((from, target)))
        {
            cx.notify();
        }
    }

    pub(crate) fn reorder_menu_tool(&mut self, source: &str, target: usize, cx: &mut Context<Self>) {
        let mut order = crate::ui::context_tool_order(&self.settings);
        let Some(from) = order.iter().position(|key| key == source) else { return; };
        if target >= order.len() { return; }
        self.preview_menu_tool_order(&order, Some((from, target)));
        self.menu_tool_target = None;
        if from != target {
            let key = order.remove(from);
            order.insert(target, key);
            self.settings.menu_tools_order = order;
            self.settings.save();
        }
        cx.notify();
    }

    pub fn remove_favorite(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.settings.favorites.retain(|favorite| !same_favorite_path(&favorite.path, path));
        self.settings.save();
        self.toast(tr("Favorito eliminado.", "Removed from Favorites."), ToastTone::Info, cx);
        cx.notify();
    }

    pub fn favorite_current(&mut self, cx: &mut Context<Self>) {
        let path = self.active_tab().path.clone();
        self.add_favorite(path, cx);
    }

    // ------------------------------------------------------------------
    // Ajustes y utilidades
    // ------------------------------------------------------------------

    pub fn toggle_hidden(&mut self, cx: &mut Context<Self>) {
        self.settings.show_hidden = !self.settings.show_hidden;
        self.settings.save();
        self.refresh_all(cx);
        self.toast(
            if self.settings.show_hidden {
                tr("Archivos ocultos visibles.", "Hidden files are visible.")
            } else {
                tr("Archivos ocultos ocultos.", "Hidden files are hidden.")
            },
            ToastTone::Info,
            cx,
        );
    }

    pub fn toggle_preview(&mut self, cx: &mut Context<Self>) {
        if self.settings.show_preview && self.text_edit.is_some() {
            self.reveal_pending_text_edit(cx);
            return;
        }
        self.settings.show_preview = !self.settings.show_preview;
        self.settings.save();
        if self.settings.show_preview {
            // Fuerza a recrear el contenido multimedia para que el vídeo
            // vuelva a reproducirse al abrir el inspector.
            self.preview_path = None;
            self.refresh_preview(cx);
        } else {
            self.preview_token = self.preview_token.wrapping_add(1);
            self.preview_loading = false;
            self.stop_media();
            self.discard_preview();
        }
        cx.notify();
    }

    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.settings.sidebar_visible = !self.settings.sidebar_visible;
        self.sidebar_animation = Some((Instant::now(), self.sidebar_progress));
        self.settings.save();
        cx.notify();
    }

    pub fn toggle_sidebar_side(&mut self, cx: &mut Context<Self>) {
        self.settings.sidebar_right = !self.settings.sidebar_right;
        self.settings.save();
        cx.notify();
    }

    pub fn remember_folder_view(&mut self) {
        let (mode, scale, compact) = (self.settings.view, self.settings.icon_scale, self.settings.compact_density);
        let tab = self.active_tab_mut();
        (tab.view, tab.icon_scale, tab.compact) = (mode, scale, compact);
        let tab=self.active_tab();
        let view=crate::settings::FolderView{path:tab.path.clone(),view:self.settings.view,scale:self.settings.icon_scale,compact:self.settings.compact_density,sort:tab.sort,descending:tab.sort_desc};
        self.settings.remember_view(view);
    }
    fn apply_saved_folder_view(&mut self) {
        let path=self.active_tab().path.clone();
        if let Some(view)=self.settings.folder_view(&path).cloned() {
            let ignore_accents = self.settings.search_ignore_accents;
            let tab = self.active_tab_mut();
            (tab.view, tab.icon_scale, tab.compact) = (view.view, view.scale, view.compact);
            if tab.sort != view.sort || tab.sort_desc != view.descending {
                tab.sort = view.sort;
                tab.sort_desc = view.descending;
                tab.sort_entries();
                tab.refresh_filter(ignore_accents);
            }
        }
        self.restore_folder_view();
    }

    fn restore_folder_view(&mut self) {
        let tab = self.active_tab();
        let (view, scale, compact) = (tab.view, tab.icon_scale, tab.compact);
        (self.settings.view, self.settings.icon_scale, self.settings.compact_density) =
            (view, scale, compact);
        self.zoom_animation = None;
        self.icon_scale_display = scale;
    }

    fn sync_search_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane();
        pane.tab_scroll.scroll_to_item(pane.active);
        let query = self.active_tab().search_query.clone();
        if self.search.read(cx).value().as_ref() != query {
            self.search.update(cx, |state, cx| state.set_value(query, window, cx));
        }
    }

    pub fn toggle_view(&mut self, cx: &mut Context<Self>) {
        let zoom = if self.settings.view == ViewMode::Grid { 15 } else { 25 };
        self.set_zoom(zoom, cx);
    }

    pub fn toggle_view_menu(&mut self, cx: &mut Context<Self>) {
        self.view_menu = !self.view_menu;
        self.dismiss_context_menu();
        self.sort_menu = false;
        self.address_history_open = false;
        cx.notify();
    }

    pub fn set_view_preset(&mut self, zoom: u32, cx: &mut Context<Self>) {
        self.view_menu = false;
        self.set_zoom(zoom, cx);
        cx.notify();
    }

    // ------------------------------------------------------------------
    // Historial de direcciones
    // ------------------------------------------------------------------

    fn move_address_history_selection(&mut self, direction: isize, cx: &mut Context<Self>) -> bool {
        let paths = &self.settings.recent_paths;
        if paths.is_empty() { return false; }
        let index = self.address_history_selection
            .or_else(|| paths.iter().position(|path| path == &self.active_tab().path))
            .map_or_else(
                || if direction < 0 { paths.len() - 1 } else { 0 },
                |index| index.saturating_add_signed(direction).min(paths.len() - 1),
            );
        self.address_history_selection = Some(index);
        self.address_history_scroll.scroll_to_item(index);
        cx.notify();
        true
    }

    pub fn toggle_address_history(&mut self, cx: &mut Context<Self>) {
        self.address_history_selection = None;
        self.address_history_scroll = gpui::ScrollHandle::new();
        self.address_history_open = !self.address_history_open;
        self.dismiss_context_menu();
        self.sort_menu = false;
        cx.notify();
    }

    pub fn close_address_history(&mut self, cx: &mut Context<Self>) {
        if self.address_history_open || self.address_editing {
            self.address_history_open = false;
            self.address_editing = false;
            cx.notify();
        }
    }

    pub fn navigate_recent(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.address_history_open = false;
        self.navigate(path, window, cx);
    }

    fn clear_recent_files(&mut self, cx: &mut Context<Self>) {
        self.recent_files_generation = self.recent_files_generation.wrapping_add(1);
        self.settings.recent_file_paths.clear();
        self.recent_icons.clear();
        self.settings.save();
        cx.notify();
    }

    pub fn clear_recent(&mut self, cx: &mut Context<Self>) {
        self.address_history_selection = None;
        self.address_history_scroll = gpui::ScrollHandle::new();
        self.settings.recent_paths.clear();
        self.settings.save();
        cx.notify();
    }

    // ------------------------------------------------------------------
    // Redimensionado de la barra lateral
    // ------------------------------------------------------------------

    pub fn begin_sidebar_resize(&mut self, cx: &mut Context<Self>) {
        self.sidebar_resizing = true;
        cx.notify();
    }

    pub fn toggle_sidebar_drive_node(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.sidebar_drive_nodes.remove(&path).is_none() {
            self.sidebar_drive_nodes.insert(path.clone(), None);
            let show_hidden = self.settings.show_hidden;
            cx.spawn(async move |this, cx| {
                let folder = path.clone();
                let mut children = cx.background_executor().spawn(async move {
                    std::fs::read_dir(folder).into_iter().flatten().flatten().filter_map(|entry| {
                        let metadata = entry.metadata().ok()?;
                        if !metadata.is_dir() || (!show_hidden && fs_model::is_hidden(&metadata, &entry.file_name().to_string_lossy())) {
                            return None;
                        }
                        Some(entry.path())
                    }).take(100).collect::<Vec<_>>()
                }).await;
                children.sort_by_cached_key(|path| folder_name(path).to_lowercase());
                let _ = this.update(cx, |this, cx| {
                    if let Some(slot) = this.sidebar_drive_nodes.get_mut(&path) {
                        *slot = Some(children);
                        cx.notify();
                    }
                });
            }).detach();
        }
        cx.notify();
    }

    pub fn drag_sidebar(&mut self, event: &MouseMoveEvent, window: &Window, cx: &mut Context<Self>) {
        if !self.sidebar_resizing {
            return;
        }
        let viewport_w = f32::from(window.viewport_size().width);
        let raw = f32::from(event.position.x);
        let width = if self.settings.sidebar_right {
            viewport_w - raw
        } else {
            raw
        };
        let workspace = if self.settings.sidebar_right == self.settings.workspaces.right {
            self.workspace_extent(viewport_w)
        } else {
            0.
        };
        let width = (width - workspace).clamp(184.0, 420.0);
        if (width - self.settings.sidebar_width).abs() > 0.5 {
            self.settings.sidebar_width = width;
            cx.notify();
        }
    }

    pub fn end_sidebar_resize(&mut self, cx: &mut Context<Self>) {
        if !self.sidebar_resizing {
            return;
        }
        self.sidebar_resizing = false;
        self.settings.save();
        cx.notify();
    }

    // ------------------------------------------------------------------
    // Redimensionado de la vista previa
    // ------------------------------------------------------------------

    pub fn begin_preview_resize(&mut self, cx: &mut Context<Self>) {
        self.preview_resizing = true;
        cx.notify();
    }

    pub fn drag_preview(&mut self, event: &MouseMoveEvent, window: &Window, cx: &mut Context<Self>) {
        if !self.preview_resizing {
            return;
        }
        let viewport_w = f32::from(window.viewport_size().width);
        let sidebar = self.sidebar_extent(viewport_w);
        let workspace = self.workspace_extent(viewport_w);
        let left_inset = if self.settings.sidebar_right { 0. } else { sidebar }
            + if self.settings.workspaces.right { 0. } else { workspace };
        let right_inset = sidebar + workspace - left_inset;
        let x = f32::from(event.position.x);
        let width = if self.settings.preview_left {
            x - left_inset
        } else {
            viewport_w - x - right_inset
        }.clamp(260.0, 720.0);
        if (width - self.settings.preview_width).abs() > 0.5 {
            self.settings.preview_width = width;
            cx.notify();
        }
    }

    pub fn end_preview_resize(&mut self, cx: &mut Context<Self>) {
        if !self.preview_resizing {
            return;
        }
        self.preview_resizing = false;
        self.settings.save();
        cx.notify();
    }

    // ------------------------------------------------------------------
    // Redimensionado de la división entre paneles
    // ------------------------------------------------------------------

    /// La barra lateral se oculta sola en ventanas estrechas para que el
    /// contenido siga siendo usable. El ajuste del usuario se conserva y
    /// vuelve a aplicarse al ensanchar la ventana.
    pub fn sidebar_extent(&self, viewport_w: f32) -> f32 {
        if viewport_w - self.workspace_extent(viewport_w) < 620.0 { return 0.0; }
        (self.settings.sidebar_display_width() + 4.0) * self.sidebar_progress
    }

    pub fn sidebar_group_progress(&self, group: crate::settings::SidebarGroup) -> f32 {
        let target = if self.settings.sidebar_group_collapsed(group) { 0.0 } else { 1.0 };
        self.sidebar_group_animations.get(&group).map_or(target, |(started, from)| {
            let t = (started.elapsed().as_secs_f32() / 0.16).min(1.0);
            from + (target - from) * (1.0 - (1.0 - t).powi(3))
        })
    }

    pub fn toggle_sidebar_group(&mut self, group: crate::settings::SidebarGroup, cx: &mut Context<Self>) {
        use crate::settings::SidebarGroup;
        let from = self.sidebar_group_progress(group);
        let collapsed = match group {
            SidebarGroup::Favorites => &mut self.settings.favorites_collapsed,
            SidebarGroup::Quick => &mut self.settings.quick_collapsed,
            SidebarGroup::Drives => &mut self.settings.drives_collapsed,
            SidebarGroup::Recent => &mut self.settings.recent_collapsed,
            SidebarGroup::Custom(id) => {
                let Some(custom) = self.settings.custom_sidebar_groups.iter_mut().find(|group| group.id == id) else { return };
                &mut custom.collapsed
            }
        };
        *collapsed = !*collapsed;
        self.sidebar_group_animations.insert(group, (Instant::now(), from));
        self.settings.save();
        cx.notify();
    }

    fn tick_sidebar(&mut self) -> bool {
        let Some((started, initial)) = self.sidebar_animation else { return false };
        let target = if self.settings.sidebar_visible { 1.0 } else { 0.0 };
        let t = (started.elapsed().as_secs_f32() / 0.20).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - t).powi(3);
        self.sidebar_progress = initial + (target - initial) * eased;
        if t >= 1.0 { self.sidebar_animation = None; }
        t < 1.0
    }

    /// Lo mismo para la vista previa: en una ventana de ~330 px no cabe junto
    /// al explorador, así que se aparta sin desactivar la preferencia.
    pub fn preview_visible_at(&self, viewport_w: f32) -> bool {
        self.settings.show_preview && self.preview_fits_at(viewport_w)
    }

    fn preview_fits_at(&self, viewport_w: f32) -> bool {
        let workspace = self.workspace_extent(viewport_w);
        let available = viewport_w - workspace;
        available >= 760.0 && (workspace == 0.
            || available - self.sidebar_extent(viewport_w)
                >= self.settings.preview_width.clamp(260.0, 720.0) + 164.)
    }

    /// Origen y ancho disponibles para los paneles (sin barra lateral ni
    /// vista previa). Se usa tanto para dibujar como para arrastrar el divisor.
    pub fn files_area(&self, window: &Window) -> (f32, f32) {
        let viewport_w = f32::from(window.viewport_size().width);
        let sidebar_w = self.sidebar_extent(viewport_w);
        let workspace_w = self.workspace_extent(viewport_w);
        let preview_w = if self.preview_visible_at(viewport_w) {
            self.settings.preview_width.clamp(260.0, 720.0) + 4.0
        } else {
            0.0
        };
        let left = if !self.settings.sidebar_right {
            sidebar_w
        } else {
            0.0
        };
        let left = left + if self.settings.preview_left { preview_w } else { 0.0 };
        let left = left + if self.settings.workspaces.right { 0.0 } else { workspace_w };
        let width = (viewport_w - sidebar_w - preview_w - workspace_w).max(0.0);
        (left, width)
    }

    pub fn load_folder_counts(&mut self, tab_id: u64, range: std::ops::Range<usize>, cx: &mut Context<Self>) {
        if !self.settings.detail_column_visible[DetailColumn::Folders.index()] { return; }
        let Some(tab) = self.tab_by_id_mut(tab_id) else { return; };
        if !matches!(tab.view, ViewMode::Tree | ViewMode::Details) || tab.loading { return; }
        let paths: Vec<_> = tab.visible.get(range.start..range.end.min(tab.visible.len())).unwrap_or_default()
            .iter().filter_map(|&index| tab.entries.get(index))
            .filter(|entry| entry.is_dir && !tab.folder_counts.contains_key(&entry.path))
            .map(|entry| entry.path.clone()).collect();
        if paths.is_empty() { return; }
        for path in &paths { tab.folder_counts.insert(path.clone(), None); }
        let token = tab.scan_token;
        cx.spawn(async move |view, cx| {
            let counts = cx.background_executor().spawn(async move {
                paths.into_iter().map(|path| {
                    let count = std::fs::read_dir(&path).ok().map(|entries| entries.flatten()
                        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir())).count());
                    (path, count)
                }).collect::<Vec<_>>()
            }).await;
            let _ = view.update(cx, |this, cx| {
                if let Some(tab) = this.tab_by_id_mut(tab_id) && tab.scan_token == token {
                    tab.folder_counts.extend(counts);
                    cx.notify();
                }
            });
        }).detach();
    }

    pub fn detail_column_order(&self) -> Vec<DetailColumn> {
        let mut order = self.settings.detail_column_order.clone();
        if let Some((column, slot)) = self.column_drag_target
            && let Some(from) = order.iter().position(|item| *item == column)
        {
            order.remove(from);
            order.insert(slot.min(order.len()), column);
        }
        order
    }

    pub fn column_offset(&self, column: DetailColumn) -> f32 {
        self.column_offsets.get(&column).map_or(0., |(start, offset)| {
            offset * (1. - start.elapsed().as_secs_f32() / 0.16).max(0.).powi(3)
        })
    }

    pub fn preview_column_order(&mut self, target: Option<(DetailColumn, usize)>) {
        if self.column_drag_target == target { return; }
        let previous = self.detail_column_order();
        self.column_drag_target = target;
        for (index, column) in self.detail_column_order().into_iter().enumerate() {
            if let Some(old) = previous.iter().position(|item| *item == column) && old != index {
                let offset = (old as f32 - index as f32) * 30. + self.column_offset(column);
                self.column_offsets.insert(column, (Instant::now(), offset));
            }
        }
    }

    pub fn commit_column_order(&mut self, column: DetailColumn, slot: usize, cx: &mut Context<Self>) {
        self.preview_column_order(Some((column, slot)));
        self.settings.detail_column_order = self.detail_column_order();
        self.column_drag_target = None;
        self.settings.save();
        cx.notify();
    }

    pub fn begin_column_resize(
        &mut self, pane: usize, index: usize, pane_width: f32, widths: [f32; DETAIL_COLUMNS], x: f32, cx: &mut Context<Self>,
    ) {
        let Some(pane) = self.panes.get(pane) else { return };
        let tab_id = pane.active_tab().id;
        let occupied: f32 = widths.iter().enumerate()
            .filter(|&(i, &width)| i != index && width > 0.0)
            .map(|(_, width)| width + DETAIL_COLUMN_GAP).sum();
        self.column_resize = Some(ColumnResize {
            tab_id, index, x, width: widths[index],
            max_width: (pane_width - DETAIL_NAME_RESERVE - DETAIL_COLUMN_GAP - occupied).clamp(DetailColumn::MIN_WIDTHS[index], 1000.0),
        });
        cx.notify();
    }

    pub fn drag_column(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if let Some(drag) = &self.column_resize {
            let width = (drag.width + drag.x - f32::from(event.position.x)).clamp(DetailColumn::MIN_WIDTHS[drag.index], drag.max_width);
            let (id, index) = (drag.tab_id, drag.index);
            if let Some(tab) = self.tab_by_id_mut(id) { tab.detail_widths[index] = width; }
            cx.notify();
        }
    }

    pub fn end_column_resize(&mut self, cx: &mut Context<Self>) {
        if let Some(drag) = self.column_resize.take() {
            if let Some(tab) = self.tab_by_id(drag.tab_id) {
                self.settings.detail_widths = tab.detail_widths;
            }
            self.settings.save();
            cx.notify();
        }
    }

    pub fn resizing_branch(&self, branch: &[bool]) -> bool {
        self.split_resize.as_ref().is_some_and(|resize| resize.branch == branch)
    }

    pub fn begin_split_resize(
        &mut self, branch: Vec<bool>, axis: Axis, start: f32, extent: f32, ratio: f32,
        cx: &mut Context<Self>,
    ) {
        self.split_resize = Some(SplitResize { branch, axis, start, extent, ratio });
        cx.notify();
    }

    pub fn drag_split(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(resize) = &self.split_resize else { return };
        let pointer = resize.axis.extent(f32::from(event.position.x), f32::from(event.position.y));
        let available = (resize.extent - DIVIDER).max(1.0);
        let next = (resize.ratio + (pointer - resize.start) / available).clamp(0.2, 0.8);
        if let Some(PaneLayout::Split { ratio, .. }) = self.pane_layout.branch_mut(&resize.branch)
            && (next - *ratio).abs() > 0.001
        {
            *ratio = next;
            cx.notify();
        }
    }

    pub fn end_split_resize(&mut self, cx: &mut Context<Self>) {
        if self.split_resize.take().is_some() {
            self.persist();
            cx.notify();
        }
    }

    // ------------------------------------------------------------------
    // Almacenamiento y caché
    // ------------------------------------------------------------------

    fn start_drive_monitor(&mut self, cx: &mut Context<Self>) {
        self.drive_monitor_task = Some(cx.spawn(async move |this, cx| {
            let executor = cx.background_executor().clone();
            let mut previous_mask = 0;
            loop {
                let mask = platform::logical_drive_mask();
                if mask != 0 && mask != previous_mask {
                    let added_mask = mask & !previous_mask;
                    let new_drives = executor.spawn(async move { platform::drives(added_mask) }).await;
                    if platform::logical_drive_mask() == mask {
                        if this.update(cx, |this, cx| {
                            this.drive_usage_task = None;
                            this.drives.retain(|drive| drive.is_in_mask(mask));
                            this.sidebar_drive_nodes.retain(|path, _| {
                                this.drives.iter().any(|drive| &drive.path == path)
                            });
                            this.drives.extend(new_drives);
                            this.drives.sort_unstable_by(|a, b| a.letter.cmp(&b.letter));
                            cx.notify();
                        }).is_err() {
                            return;
                        }
                        previous_mask = mask;
                    }
                }
                executor.timer(Duration::from_secs(1)).await;
            }
        }));
    }

    fn refresh_drive_usage(&mut self, cx: &mut Context<Self>) {
        let paths: Vec<_> = self.drives.iter().map(|drive| drive.path.clone()).collect();
        if paths.is_empty() { return; }

        self.drive_usage_task = Some(cx.spawn(async move |this, cx| {
            let usage = cx.background_executor().spawn(async move {
                paths.into_iter().filter_map(|path| {
                    platform::disk_space(&path).map(|space| (path, space))
                }).collect::<HashMap<_, _>>()
            }).await;
            let _ = this.update(cx, |this, cx| {
                let mut changed = false;
                for drive in &mut this.drives {
                    if let Some(&(total, free)) = usage.get(&drive.path)
                        && (drive.total, drive.free) != (total, free)
                    {
                        (drive.total, drive.free) = (total, free);
                        changed = true;
                    }
                }
                if changed { cx.notify(); }
            });
        }));
    }

    pub fn refresh_storage_info(&mut self, cx: &mut Context<Self>) {
        let entity = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let info = cx.background_executor().spawn(async { Self::read_storage_info() }).await;
            let _ = entity.update(cx, |this, cx| {
                this.storage_info = Some(info);
                cx.notify();
            });
        }).detach();
    }

    fn read_storage_info() -> StorageInfo {
        let cache_path = crate::settings::cache_dir();
        let (cache, cache_files) = thumbs::cache_stats();
        let app = std::env::current_exe()
            .ok()
            .and_then(|path| std::fs::metadata(path).ok())
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let settings = crate::settings::settings_path()
            .and_then(|path| std::fs::metadata(path).ok())
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let probe = cache_path
            .as_deref()
            .and_then(|path| path.ancestors().find(|candidate| candidate.exists()))
            .unwrap_or_else(|| Path::new("C:\\"));
        let (total, free) = platform::disk_space(probe).unwrap_or((0, 0));
        StorageInfo {
            total,
            free,
            app,
            settings,
            cache,
            cache_files,
            cache_path,
        }
    }

    pub fn clear_thumbnail_cache(&mut self, cx: &mut Context<Self>) {
        match thumbs::clear_cache() {
            Ok(()) => {
                self.refresh_storage_info(cx);
                self.toast(tr("Caché de miniaturas borrada.", "Thumbnail cache cleared."), ToastTone::Success, cx);
            }
            Err(error) => self.toast(error, ToastTone::Error, cx),
        }
        cx.notify();
    }

    /// Botón lateral «atrás» del ratón.
    pub fn on_mouse_back(&mut self, _: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.mouse_travel(-1, window, cx);
    }

    /// Botón lateral «adelante» del ratón.
    pub fn on_mouse_forward(
        &mut self,
        _: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.mouse_travel(1, window, cx);
    }

    fn mouse_travel(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() || self.address_editing || self.recording_shortcut.is_some() {
            return;
        }
        self.travel(delta, window, cx);
        cx.stop_propagation();
    }

    pub(crate) fn open_settings_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(folder) = crate::settings::settings_path()
            .and_then(|path| path.parent().map(Path::to_path_buf))
        else {
            self.toast(tr("La carpeta de configuración no está disponible.", "The settings folder is unavailable."), ToastTone::Error, cx);
            return;
        };
        if let Err(error) = std::fs::create_dir_all(&folder) {
            self.toast(crate::localized!(format, "No se pudo abrir la carpeta de configuración: {error}", "Could not open the settings folder: {error}"), ToastTone::Error, cx);
            return;
        }
        self.close_dialog(window, cx);
        self.open_in_new_tab(folder, window, cx);
    }

    pub fn export_preferences(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let directory = self.quick.iter().find(|place| place.label == tr("Documentos", "Documents"))
            .map_or_else(|| self.active_tab().path.clone(), |place| place.path.clone());
        let receiver = cx.prompt_for_new_path(&directory, Some(tr("Xool Files - configuración.json", "Xool Files - settings.json")));
        cx.spawn_in(window, async move |this, cx| {
            let result = match receiver.await {
                Ok(Ok(Some(path))) => Ok(path),
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(error)) => Err(error.to_string()),
            };
            let _ = this.update_in(cx, |this, _, cx| {
                match result.and_then(|path| this.settings.export_to(&path)) {
                    Ok(()) => this.toast(tr("Configuración exportada.", "Settings exported."), ToastTone::Success, cx),
                    Err(error) => this.toast(crate::localized!(format, "No se pudo exportar la configuración: {error}", "Could not export settings: {error}"), ToastTone::Error, cx),
                }
            });
        }).detach();
    }

    pub fn import_preferences(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true, directories: false, multiple: false,
            prompt: Some(tr("Importar configuración de Xool Files", "Import Xool Files settings").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = match receiver.await {
                Ok(Ok(Some(paths))) => {
                    let Some(path) = paths.first() else { return };
                    Settings::read_import(path)
                }
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(error)) => Err(error.to_string()),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                let result = result.and_then(|mut imported| {
                    imported.window_placement = this.settings.window_placement;
                    let backup = imported.replace_with_backup()?;
                    this.apply_preferences(imported, window, cx);
                    Ok(backup)
                });
                match result {
                    Ok(backup) => this.preferences_replaced(tr("Configuración importada.", "Settings imported."), backup, cx),
                    Err(error) => this.toast(crate::localized!(format, "No se pudo importar la configuración: {error}", "Could not import settings: {error}"), ToastTone::Error, cx),
                }
            });
        }).detach();
    }

    pub fn reset_preferences(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let defaults = Settings::default();
        match defaults.replace_with_backup() {
            Ok(backup) => {
                self.apply_preferences(defaults, window, cx);
                self.preferences_replaced(tr("Preferencias restauradas.", "Preferences restored."), backup, cx);
            }
            Err(error) => self.toast(crate::localized!(format, "No se pudo restaurar la configuración: {error}", "Could not restore settings: {error}"), ToastTone::Error, cx),
        }
        cx.notify();
    }

    fn preferences_replaced(&mut self, message: &str, backup: Option<PathBuf>, cx: &mut Context<Self>) {
        let message = match backup.as_deref().and_then(Path::file_name) {
            Some(name) => crate::localized!(format, "{message} Copia guardada en {}.", "{message} Backup saved to {}.", name.to_string_lossy()),
            None => message.to_owned(),
        };
        self.toast(message, ToastTone::Success, cx);
    }

    pub(crate) fn set_language(&mut self, language: crate::i18n::Language, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.language = language;
        self.settings.save();
        self.refresh_language(window, cx);
    }

    fn refresh_language(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        crate::i18n::set(self.settings.language);
        for (item, (label, _)) in self.quick.iter_mut().zip(platform::known_folders()) {
            item.label = label;
        }
        if let Some(item) = self.quick.iter_mut().find(|item| fs_model::is_recycle_bin(&item.path)) {
            item.label = tr("Papelera", "Recycle Bin").into();
        }
        for (input, placeholder) in [
            (&self.address, tr("Escribe una ruta y pulsa Enter…", "Type a path and press Enter…")),
            (&self.search, tr("Buscar en esta carpeta…", "Search this folder…")),
            (&self.workspace_ui.search, tr("Buscar carpetas…", "Find folders…")),
            (&self.settings_search, tr("Buscar en Ajustes", "Search Settings")),
            (&self.shortcut_search, tr("Buscar acción o combinación", "Search for an action or shortcut")),
            (&self.dialog_input, tr("Escribe un nombre…", "Enter a name…")),
            (&self.rename_input, tr("Nombre del elemento…", "Item name…")),
        ] {
            input.update(cx, |state, cx| state.set_placeholder(placeholder, window, cx));
        }
        cx.notify();
    }

    fn apply_preferences(&mut self, settings: Settings, window: &mut Window, cx: &mut Context<Self>) {
        let previous_keys = self.settings.quick_search_shortcut.clone();
        self.settings = settings;
        self.workspace_ui.appearance_workspace = None;
        self.reset_workspace_panel(window, cx);
        self.refresh_language(window, cx);
        self.clear_background_image();
        self.refresh_background_image(cx);
        self.integration_message = crate::tray::set_visible(window, self.settings.show_tray_icon).err();
        self.search_index.set_live(self.settings.search_live_index);
        self.sidebar_progress = if self.settings.sidebar_visible { 1.0 } else { 0.0 };
        self.sidebar_animation = None;
        self.sidebar_drive_nodes.clear();
        self.sidebar_group_animations.clear();
        self.icon_scale_display = self.settings.icon_scale;
        self.zoom_animation = None;
        for pane in &mut self.panes {
            for tab in &mut pane.tabs {
                tab.sort = self.settings.sort;
                tab.sort_desc = self.settings.sort_descending;
                tab.view = self.settings.view;
                tab.icon_scale = self.settings.icon_scale;
                tab.compact = self.settings.compact_density;
                tab.detail_widths = self.settings.detail_widths;
            }
        }
        self.recording_shortcut = None;
        self.quick_search_message = platform::register_quick_search(window, self.settings.quick_search_shortcut.as_deref()).err();
        if self.quick_search_message.is_some() {
            self.settings.quick_search_shortcut = previous_keys;
        }
        self.set_theme(self.settings.theme, window, cx);
        self.rebind_shortcuts(cx);
        self.stop_media();
        self.discard_preview();
        self.preview_path = None;
        self.refresh_preview(cx);
        self.load_recent_icons(cx);
        self.persist();
        self.refresh_all(cx);
        self.refresh_storage_info(cx);
    }

    pub fn set_theme(&mut self, theme_mode: ThemeMode, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.theme = theme_mode;
        self.settings.save();
        let mode = if theme::is_dark(&self.settings) {
            gpui_component::ThemeMode::Dark
        } else {
            gpui_component::ThemeMode::Light
        };
        gpui_component::Theme::change(mode, Some(window), cx);
        theme::apply_component_theme(&self.settings, cx);
        cx.notify();
    }

    pub(crate) fn toggle_translucent_background(&mut self, cx: &mut Context<Self>) {
        self.settings.translucent_background = !self.settings.translucent_background;
        self.refresh_background_image(cx);
        self.settings.save();
        cx.notify();
    }

    pub(crate) fn use_desktop_background(&mut self, cx: &mut Context<Self>) {
        self.settings.background_image_path = None;
        self.clear_background_image();
        self.settings.save();
        cx.notify();
    }

    pub(crate) fn choose_background_image(&mut self, cx: &mut Context<Self>) {
        let result = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(tr("Elegir imagen de fondo", "Choose background image").into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = result.await && let Some(path) = paths.into_iter().next() {
                let _ = this.update(cx, |this, cx| {
                    if this.settings.translucent_background {
                        this.load_background_image(path, true, cx);
                    }
                });
            }
        }).detach();
    }

    fn clear_background_image(&mut self) {
        self.background_image_task = None;
        self.pending_background_path = None;
        self.image_drops.extend(self.background_image.take());
    }

    pub(crate) fn refresh_background_image(&mut self, cx: &mut Context<Self>) {
        if !self.settings.translucent_background {
            self.clear_background_image();
        } else if let Some(path) = self.pending_background_path.clone() {
            self.load_background_image(path, true, cx);
        } else if let Some(path) = self.settings.background_image_path.clone() {
            self.load_background_image(path, false, cx);
        } else {
            self.clear_background_image();
        }
    }

    fn load_background_image(&mut self, path: PathBuf, save_selection: bool, cx: &mut Context<Self>) {
        let blur = self.settings.background_blur;
        let input = path.clone();
        if save_selection { self.pending_background_path = Some(path.clone()); }
        self.background_image_task = Some(cx.spawn(async move |this, cx| {
            let image = cx.background_executor().spawn(async move {
                crate::thumbs::background_image(&input, blur)
            }).await;
            let _ = this.update(cx, |this, cx| {
                this.pending_background_path = None;
                if let Some(image) = image {
                    this.image_drops.extend(this.background_image.replace(image));
                    if save_selection {
                        this.settings.background_image_path = Some(path);
                        this.settings.save();
                    }
                } else if save_selection {
                    this.toast(tr("No se pudo abrir esa imagen.", "Could not open this image."), ToastTone::Error, cx);
                }
                cx.notify();
            });
        }));
    }

    pub fn set_accent(&mut self, accent: Accent, cx: &mut Context<Self>) {
        self.settings.accent = accent;
        self.settings.save();
        theme::apply_component_theme(&self.settings, cx);
        cx.notify();
    }

    pub fn step_zoom(&mut self, increase: bool, cx: &mut Context<Self>) {
        let next = self.settings.view.stepped_zoom(self.settings.icon_scale, increase);
        self.set_zoom(next, cx);
    }

    pub fn set_zoom(&mut self, zoom: u32, cx: &mut Context<Self>) {
        let (view, scale) = ViewMode::from_zoom(zoom);
        let previous = self.settings.view;
        if view == previous && (scale - self.settings.icon_scale).abs() < 0.0001 {
            return;
        }
        self.settings.view = view;
        self.settings.icon_scale = scale;
        if view != previous {
            let ignore_accents = self.settings.search_ignore_accents;
            let tab = self.active_tab_mut();
            if view != ViewMode::Tree { tab.clear_tree(); }
            tab.scroll = gpui::UniformListScrollHandle::new();
            tab.refresh_filter(ignore_accents);
        }
        self.remember_folder_view();
        self.settings.save();
        if view == ViewMode::Grid && previous == ViewMode::Grid {
            self.zoom_animation = Some((Instant::now(), self.icon_scale_display));
        } else {
            self.zoom_animation = None;
            self.icon_scale_display = scale;
        }
        self.refresh_preview(cx);
        cx.notify();
    }

    pub fn scroll_wheel_smoothly(
        &mut self, event: &gpui::ScrollWheelEvent, line_height: Pixels, settings_nav_axis: gpui::Axis,
    ) -> bool {
        use gpui::{Axis as ScrollAxis, ScrollDelta};
        use WheelScrollTarget::*;

        if self.settings.wheel_scroll_ms == 0 { return false; }

        let (target, handle, axis, line_step) = if matches!(self.dialog, Some(Dialog::Settings)) {
            if self.settings_closing || self.settings_theme_open { return false; }
            if self.settings_page_scroll.bounds().contains(&event.position) {
                (SettingsPage(self.preferences_page), self.settings_page_scroll.clone(), ScrollAxis::Vertical, line_height)
            } else if !self.settings_nav_collapsed && self.settings_nav_progress == 1.
                && self.settings_nav_scroll.bounds().contains(&event.position) {
                (SettingsNavigation, self.settings_nav_scroll.clone(), settings_nav_axis, line_height)
            } else {
                return false;
            }
        } else {
            if self.dialog.is_some() || self.context_menu.is_some()
                || self.view_menu || self.sort_menu || self.address_history_open { return false; }
            let Some((id, handle)) = self.panes.iter().map(|pane| {
                let tab = pane.active_tab();
                (tab.id, tab.scroll.0.borrow().base_handle.clone())
            }).find(|(_, scroll)| scroll.bounds().contains(&event.position)) else { return false; };
            (Files(id), handle, ScrollAxis::Vertical, gpui::px(28.))
        };

        let ScrollDelta::Lines(lines) = event.delta else {
            if self.wheel_scroll.is_some_and(|motion| motion.target == target) {
                self.wheel_scroll = None;
            }
            return false;
        };
        let mut lines_in_axis = lines.along(axis);
        if lines_in_axis == 0. && !matches!(target, Files(_)) {
            lines_in_axis = lines.along(axis.invert());
        }

        let maximum = handle.max_offset().along(axis);
        if maximum <= gpui::px(0.) || lines_in_axis == 0. { return false; }
        let current = handle.offset().along(axis);
        let previous = self.wheel_scroll.filter(|motion| {
            motion.target == target && motion.axis == axis
                && f32::from(motion.to - current) * lines_in_axis > 0.
        }).map_or(current, |motion| motion.to);
        let to = (previous + line_step * lines_in_axis).clamp(-maximum, gpui::px(0.));
        self.wheel_scroll = Some(WheelScrollMotion { target, axis, started: Instant::now(), from: current, to });
        true
    }

    fn tick_wheel_scroll(&mut self, settings_nav_axis: gpui::Axis) -> bool {
        use WheelScrollTarget::*;

        let Some(motion) = self.wheel_scroll.take() else { return false; };
        let millis = self.settings.wheel_scroll_ms;
        if millis == 0 { return false; }

        let handle = match motion.target {
            Files(id) if self.dialog.is_none() => {
                let Some(tab) = self.panes.iter().map(Pane::active_tab).find(|tab| tab.id == id) else { return false; };
                let state = tab.scroll.0.borrow();
                if state.deferred_scroll_to_item.is_some() { return false; }
                state.base_handle.clone()
            }
            target if matches!(self.dialog, Some(Dialog::Settings)) && !self.settings_closing && !self.settings_theme_open => {
                match target {
                    SettingsPage(page) if page == self.preferences_page => self.settings_page_scroll.clone(),
                    SettingsNavigation if motion.axis == settings_nav_axis => self.settings_nav_scroll.clone(),
                    _ => return false,
                }
            }
            _ => return false,
        };
        let maximum = handle.max_offset().along(motion.axis);
        let progress = (motion.started.elapsed().as_secs_f32() / (f32::from(millis) / 1000.)).min(1.);
        let eased = 1. - (1. - progress).powi(3);
        let offset = (motion.from + (motion.to - motion.from) * eased).clamp(-maximum, gpui::px(0.));
        handle.set_offset(handle.offset().apply_along(motion.axis, |_| offset));
        if progress < 1. { self.wheel_scroll = Some(motion); }
        progress < 1.
    }

    fn tick_icon_scale(&mut self) -> bool {
        let Some((started, start)) = self.zoom_animation else { return false };
        let progress = (started.elapsed().as_secs_f32() / 0.23).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - progress).powi(3);
        self.icon_scale_display = start + (self.settings.icon_scale - start) * eased;
        if progress >= 1.0 {
            self.zoom_animation = None;
        }
        progress < 1.0
    }

    pub(crate) fn action_sound(&self, action: SoundAction) -> Option<&ActionSound> {
        match action {
            SoundAction::SelectFile => self.settings.select_file_sound.as_ref(),
            SoundAction::SwitchTab => self.settings.tab_sound.as_ref(),
        }
    }

    fn play_action_sound(&self, action: SoundAction) {
        if self.settings.action_sounds_enabled && let Some(sound) = self.action_sound(action) {
            sound.play();
        }
    }

    pub(crate) fn preview_action_sound(&mut self, sound: &ActionSound, cx: &mut Context<Self>) {
        if !sound.play() {
            self.toast(tr("No se pudo reproducir el sonido.", "Could not play the sound."), ToastTone::Error, cx);
        }
    }

    pub(crate) fn set_action_sound(&mut self, action: SoundAction, sound: Option<ActionSound>, cx: &mut Context<Self>) {
        match action {
            SoundAction::SelectFile => self.settings.select_file_sound = sound,
            SoundAction::SwitchTab => self.settings.tab_sound = sound,
        }
        self.settings.save();
        cx.notify();
    }

    pub fn toast(&mut self, text: impl Into<String>, tone: ToastTone, cx: &mut Context<Self>) {
        self.toast_token = self.toast_token.wrapping_add(1);
        let token = self.toast_token;
        self.toast = Some(Toast {
            text: text.into(),
            tone,
        });
        let this = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(3))
                .await;
            this.update(cx, |this, cx| {
                if this.toast_token == token {
                    this.toast = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn sidebar_group_order(&self) -> Vec<crate::settings::SidebarGroup> {
        let mut order = self.settings.sidebar_groups.clone();
        if let Some((group, slot)) = self.sidebar_group_target
            && let Some(source) = order.iter().position(|item| *item == group)
        {
            order.remove(source);
            order.insert(slot.min(order.len()), group);
        }
        order
    }

    pub fn sidebar_group_offset(&self, group: crate::settings::SidebarGroup) -> f32 {
        self.sidebar_group_offsets.get(&group).map_or(0.0, |(started, offset)| {
            let remaining = (1.0 - started.elapsed().as_secs_f32() / 0.16).max(0.0);
            offset * remaining.powi(3)
        })
    }

    pub fn preview_sidebar_group(&mut self, target: Option<(crate::settings::SidebarGroup, usize)>) {
        if self.sidebar_group_target == target { return; }
        let before = self.sidebar_group_order();
        self.sidebar_group_target = target;
        let after = self.sidebar_group_order();
        for (position, group) in after.into_iter().enumerate() {
            if let Some(old) = before.iter().position(|item| *item == group) && old != position {
                let offset = (old as f32 - position as f32) * 36.0 + self.sidebar_group_offset(group);
                self.sidebar_group_offsets.insert(group, (Instant::now(), offset));
            }
        }
    }

    pub fn commit_sidebar_group(&mut self, group: crate::settings::SidebarGroup, slot: usize, cx: &mut Context<Self>) {
        self.preview_sidebar_group(Some((group, slot)));
        self.settings.sidebar_groups = self.sidebar_group_order();
        self.sidebar_group_target = None;
        self.settings.save();
        cx.notify();
    }

    pub fn finish_drag(&mut self) {
        self.preview_tab_reorder(None);
        self.preview_sidebar_group(None);
        self.preview_sidebar_favorite(None);
        if self.menu_tool_target.is_some() {
            self.preview_menu_tool_order(&crate::ui::context_tool_order(&self.settings), None);
        }
        self.preview_column_order(None);
        self.native_drag_task = None;
        self.folder_drag = None;
        self.file_drag_tab = None;
        self.folder_drop_bounds = None;
        self.tab_drop_zone = None;
        self.tab_drop_pane = None;
    }

    fn cancel_active_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_drag();
        cx.stop_active_drag(window);
        cx.notify();
    }

    pub fn hover_drag_folder(
        &mut self, tab_id: Option<u64>, path: &Path, bounds: Bounds<Pixels>, inside: bool,
        window: &mut Window, cx: &mut Context<Self>,
    ) {
        let pending = self.folder_drag.as_ref()
            .is_some_and(|(tab, candidate, _)| *tab == tab_id && candidate == path);
        if !inside {
            if pending { self.folder_drag = None; }
            return;
        }
        let current = self.active_tab();
        if pending || (tab_id.is_none_or(|id| id == current.id) && current.path == path) {
            return;
        }

        let path = path.to_path_buf();
        let target = path.clone();
        let task = cx.spawn_in(window, async move |view, cx| {
            cx.background_executor().timer(Duration::from_millis(650)).await;
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.folder_drag = None;
                    if !cx.has_active_drag() || !bounds.contains(&window.mouse_position()) {
                        return;
                    }
                    if let Some(id) = tab_id {
                        let Some((pane, index)) = this.tab_position(id) else { return };
                        if this.panes[pane].active != index { return; }
                        if this.active_pane != pane {
                            this.switch_tab(pane, index, window, cx);
                        }
                    }
                    if this.active_tab().path != target {
                        this.navigate(target, window, cx);
                    }
                });
            });
        });
        self.folder_drag = Some((tab_id, path, task));
    }

    pub fn hover_file_drag_tab(
        &mut self,
        tab_id: u64,
        bounds: Bounds<Pixels>,
        inside: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pending = self.file_drag_tab.as_ref().is_some_and(|(id, _)| *id == tab_id);
        if !inside {
            if pending { self.file_drag_tab = None; }
            return;
        }
        if pending || self.active_tab().id == tab_id { return; }

        let task = cx.spawn_in(window, async move |view, cx| {
            cx.background_executor().timer(Duration::from_millis(500)).await;
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.file_drag_tab = None;
                    if cx.has_active_drag() && bounds.contains(&window.mouse_position())
                        && let Some((pane, index)) = this.tab_position(tab_id)
                        && this.active_tab().id != tab_id
                    {
                        this.switch_tab(pane, index, window, cx);
                    }
                });
            });
        });
        self.file_drag_tab = Some((tab_id, task));
    }

    pub fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.detail_label_menu.take().is_some() {
            cx.notify();
            return;
        }
        if self.marquee.take().is_some() || std::mem::take(&mut self.recently_closed_open) {
            cx.notify();
            return;
        }
        if self.columns_menu.take().is_some() && !cx.has_active_drag() {
            cx.notify();
            return;
        }
        if cx.has_active_drag() {
            self.cancel_active_drag(window, cx);
            return;
        }
        self.tab_drop_zone = None;
        self.tab_drop_pane = None;
        if self.rename_path.is_some() {
            self.cancel_inline_rename(window, cx);
            self.focus_root(window);
            return;
        }
        if self.context_menu.is_some() || self.sort_menu || self.address_history_open || self.view_menu {
            if self.address_history_open { self.address_editing = false; }
            self.dismiss_context_menu();
            self.sort_menu = false;
            self.view_menu = false;
            self.address_history_open = false;
            self.focus_root(window);
            cx.notify();
            return;
        }
        if self.creating_sidebar_group && matches!(self.dialog, Some(Dialog::Settings)) {
            self.cancel_sidebar_group_creation(window, cx);
            return;
        }
        if self.dialog.is_some() {
            self.close_dialog(window, cx);
            return;
        }
        if self.preview_image_tool.is_some() {
            self.close_image_tool(cx);
            return;
        }
        if self.address_editing {
            self.address_editing = false;
            self.focus_root(window);
            cx.notify();
            return;
        }
        if let Some((sequence, _)) = &self.cut_clipboard {
            match platform::cancel_clipboard_cut(window, *sequence) {
                Ok(()) => { self.cut_clipboard = None; cx.notify(); }
                Err(error) => self.toast(error, ToastTone::Error, cx),
            }
            return;
        }
        self.clear_selection(cx);
        self.focus_root(window);
    }

    // ------------------------------------------------------------------
    // Atajos
    // ------------------------------------------------------------------

    /// Captura la siguiente combinación cuando Ajustes está grabando un atajo.
    /// Devuelve true si consumió la tecla para que no ejecute su acción normal.
    fn capture_shortcut(
        &mut self,
        event: &KeystrokeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(id) = self.recording_shortcut else {
            return false;
        };
        if event.keystroke.key.eq_ignore_ascii_case("escape") {
            self.recording_shortcut = None;
            self.toast(tr("Cambio de atajo cancelado.", "Shortcut change canceled."), ToastTone::Info, cx);
            self.focus_root(window);
            cx.notify();
            return true;
        }
        if id == shortcuts::QUICK_SEARCH_RECORDING {
            if let Some(keys) = shortcuts::keystroke_to_string(&event.keystroke) {
                self.set_quick_key(Some(&keys), window, cx);
            }
            return true;
        }
        // Supr o Retroceso mientras se graba dejan la acción sin atajo.
        let bare_key = !event.keystroke.modifiers.control
            && !event.keystroke.modifiers.alt
            && !event.keystroke.modifiers.shift
            && !event.keystroke.modifiers.platform;
        if bare_key
            && matches!(
                event.keystroke.key.to_lowercase().as_str(),
                "delete" | "supr" | "backspace"
            )
        {
            self.settings.unbind_shortcut(id);
            self.settings.save();
            self.recording_shortcut = None;
            self.rebind_shortcuts(cx);
            self.toast(tr("Atajo eliminado. La acción queda sin atajo.", "Shortcut removed. The action is unassigned."), ToastTone::Info, cx);
            self.focus_root(window);
            cx.notify();
            return true;
        }
        let Some(keys) = shortcuts::keystroke_to_string(&event.keystroke) else {
            return true;
        };
        self.assign_shortcut(id, keys, window, cx);
        true
    }

    pub fn assign_shortcut(
        &mut self,
        id: &'static str,
        keys: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(message) = shortcuts::validate(&keys) {
            self.toast(message, ToastTone::Error, cx);
            return;
        }
        let normalized = shortcuts::normalize(&keys);
        for def in shortcuts::SHORTCUTS {
            if def.id == id {
                continue;
            }
            if shortcuts::normalize(&self.settings.shortcut(def.id)) == normalized {
                self.toast(
                    crate::localized!(format, "Ese atajo ya lo usa «{}».", "That shortcut is already used by “{}”.", def.label()),
                    ToastTone::Error,
                    cx,
                );
                return;
            }
        }
        self.settings.set_shortcut(id, keys);
        self.settings.save();
        self.recording_shortcut = None;
        self.rebind_shortcuts(cx);
        self.toast(tr("Atajo actualizado.", "Shortcut updated."), ToastTone::Success, cx);
        self.focus_root(window);
        cx.notify();
    }

    pub fn reset_shortcut(&mut self, id: &str, cx: &mut Context<Self>) {
        self.settings.reset_shortcut(id);
        self.settings.save();
        self.rebind_shortcuts(cx);
        if let Some(def) = shortcuts::find(id) {
            self.toast(
                crate::localized!(format, "«{}» vuelve a su atajo predeterminado.", "“{}” was reset to its default shortcut.", def.label()),
                ToastTone::Info,
                cx,
            );
        }
        cx.notify();
    }

    pub fn reset_all_shortcuts(&mut self, cx: &mut Context<Self>) {
        self.settings.shortcuts.clear();
        self.settings.save();
        self.rebind_shortcuts(cx);
        self.toast(tr("Todos los atajos vuelven a su valor predeterminado.", "All shortcuts were reset to their defaults."), ToastTone::Info, cx);
        cx.notify();
    }

    /// Deja una acción sin atajo. Se guarda un centinela para que no vuelva
    /// a tomar el valor predeterminado al reiniciar.
    pub fn clear_shortcut(&mut self, id: &str, cx: &mut Context<Self>) {
        self.settings.unbind_shortcut(id);
        self.settings.save();
        self.rebind_shortcuts(cx);
        if let Some(def) = shortcuts::find(id) {
            self.toast(
                crate::localized!(format, "«{}» queda sin atajo.", "“{}” is now unassigned.", def.label()),
                ToastTone::Info,
                cx,
            );
        }
        cx.notify();
    }

    /// Aplica los atajos actuales sin reiniciar. Las combinaciones antiguas
    /// que ya no se usan se anulan con `NoAction` para que no queden activas.
    pub fn rebind_shortcuts(&mut self, cx: &mut Context<Self>) {
        let mut effective = HashSet::new();
        for def in shortcuts::SHORTCUTS {
            let keys = self.settings.shortcut(def.id);
            if !shortcuts::is_unbound(&keys) {
                effective.insert(shortcuts::normalize(&keys));
            }
        }
        let mut bindings = Vec::new();
        for keys in self.applied_shortcuts.values() {
            if shortcuts::is_unbound(keys) {
                continue;
            }
            if !effective.contains(&shortcuts::normalize(keys)) {
                bindings.push(KeyBinding::new(keys, NoAction, Some("XoolFiles")));
            }
        }
        for def in shortcuts::SHORTCUTS {
            let keys = self.settings.shortcut(def.id);
            if let Some(binding) = shortcut_binding(def.id, &keys) {
                bindings.push(binding);
            }
            self.applied_shortcuts.insert(def.id.to_string(), keys);
        }
        cx.bind_keys(bindings);
    }

    pub(crate) fn on_new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        self.new_tab(window, cx);
    }
    pub(crate) fn on_close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        self.close_active_tab(window, cx);
    }
    pub(crate) fn on_next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_tab(1, window, cx);
    }
    pub(crate) fn on_prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_tab(-1, window, cx);
    }
    pub(crate) fn on_back(&mut self, _: &Back, window: &mut Window, cx: &mut Context<Self>) {
        self.travel(-1, window, cx);
    }
    pub(crate) fn on_forward(&mut self, _: &Forward, window: &mut Window, cx: &mut Context<Self>) {
        self.travel(1, window, cx);
    }
    pub(crate) fn on_up(&mut self, _: &Up, window: &mut Window, cx: &mut Context<Self>) {
        self.go_up(window, cx);
    }
    pub(crate) fn on_refresh(&mut self, _: &Refresh, _window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_active(cx);
    }
    pub(crate) fn on_focus_address(&mut self, _: &FocusAddress, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_address_edit(window, cx);
    }
    pub(crate) fn on_focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_search(window, cx);
    }
    pub(crate) fn on_toggle_recursive_search(
        &mut self, _: &ToggleRecursiveSearch, window: &mut Window, cx: &mut Context<Self>,
    ) {
        if self.dialog.is_none() {
            self.toggle_recursive_search(window, cx);
            cx.stop_propagation();
        }
    }

    pub(crate) fn on_toggle_hidden(&mut self, _: &ToggleHidden, _window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_hidden(cx);
    }
    pub(crate) fn on_toggle_split(&mut self, _: &ToggleSplit, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_split(window, cx);
    }
    pub(crate) fn on_toggle_preview(&mut self, _: &TogglePreview, _window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_preview(cx);
    }
    pub(crate) fn on_toggle_sidebar(&mut self, _: &ToggleSidebar, _window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_sidebar(cx);
    }
    pub(crate) fn on_new_folder(&mut self, _: &NewFolder, window: &mut Window, cx: &mut Context<Self>) {
        self.open_new_folder_dialog(window, cx);
    }
    pub(crate) fn on_rename(&mut self, _: &RenameSelected, window: &mut Window, cx: &mut Context<Self>) {
        self.rename_selected(window, cx);
    }
    pub(crate) fn on_delete(&mut self, _: &DeleteSelected, window: &mut Window, cx: &mut Context<Self>) {
        self.delete_selected(window, cx);
    }
    pub(crate) fn on_copy(&mut self, _: &CopySelected, window: &mut Window, cx: &mut Context<Self>) {
        self.copy_selection(false, window, cx);
    }
    pub(crate) fn on_cut(&mut self, _: &CutSelected, window: &mut Window, cx: &mut Context<Self>) {
        self.copy_selection(true, window, cx);
    }
    pub(crate) fn on_paste(&mut self, _: &PasteFiles, window: &mut Window, cx: &mut Context<Self>) {
        self.paste(window, cx);
    }
    pub(crate) fn on_open(&mut self, _: &OpenSelected, window: &mut Window, cx: &mut Context<Self>) {
        self.open_selected(window, cx);
    }
    pub(crate) fn on_select_all(&mut self, _: &SelectAllFiles, _window: &mut Window, cx: &mut Context<Self>) {
        self.select_all(cx);
    }
    pub(crate) fn on_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(window, cx);
    }
    pub(crate) fn on_toggle_view(&mut self, _: &ToggleViewMode, _window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_view(cx);
    }
    pub(crate) fn on_favorite(&mut self, _: &FavoriteCurrent, _window: &mut Window, cx: &mut Context<Self>) {
        self.favorite_current(cx);
    }
    pub(crate) fn on_zoom_in(&mut self, _: &ZoomIn, _window: &mut Window, cx: &mut Context<Self>) {
        self.step_zoom(true, cx);
    }
    pub(crate) fn on_zoom_out(&mut self, _: &ZoomOut, _window: &mut Window, cx: &mut Context<Self>) {
        self.step_zoom(false, cx);
    }
    pub(crate) fn on_zoom_reset(&mut self, _: &ZoomReset, _window: &mut Window, cx: &mut Context<Self>) {
        self.set_zoom(25, cx);
    }
    pub(crate) fn on_escape(&mut self, _: &EscapeOverlay, window: &mut Window, cx: &mut Context<Self>) {
        self.escape(window, cx);
    }
    pub(crate) fn on_sort_name(&mut self, _: &SortByName, _window: &mut Window, cx: &mut Context<Self>) {
        self.set_sort(SortKey::Name, cx);
    }
    pub(crate) fn on_sort_size(&mut self, _: &SortBySize, _window: &mut Window, cx: &mut Context<Self>) {
        self.set_sort(SortKey::Size, cx);
    }
    pub(crate) fn on_sort_kind(&mut self, _: &SortByKind, _window: &mut Window, cx: &mut Context<Self>) {
        self.set_sort(SortKey::Kind, cx);
    }
    pub(crate) fn on_sort_modified(&mut self, _: &SortByModified, _window: &mut Window, cx: &mut Context<Self>) {
        self.set_sort(SortKey::Modified, cx);
    }

    fn set_sort(&mut self, key: SortKey, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        if tab.sort == key {
            tab.sort_desc = !tab.sort_desc;
        } else {
            tab.sort = key;
            tab.sort_desc = false;
        }
        let tab = self.active_tab_mut();
        tab.sort_entries();
        let (sort, sort_desc) = (tab.sort, tab.sort_desc);
        self.settings.sort = sort;
        self.settings.sort_descending = sort_desc;
        self.refresh_active_filter();
        self.remember_folder_view();
        self.settings.save();
        cx.notify();
    }

    pub(crate) fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            if self.workspace_ui.edit.is_some() && self.workspace_ui.name.read(cx).focus_handle(cx).is_focused(window) {
                self.cancel_workspace_name(window, cx);
                cx.stop_propagation();
                return;
            }
            self.escape(window, cx);
            cx.stop_propagation();
            return;
        }
        if self.dialog.is_some() || self.address_editing || !self.focus_handle.is_focused(window) {
            return;
        }
        let shift = event.keystroke.modifiers.shift;
        let control = event.keystroke.modifiers.control;
        if event.keystroke.modifiers.alt || event.keystroke.modifiers.platform {
            return;
        }
        let columns = self.view_columns(window);
        match event.keystroke.key.as_str() {
            "z" if control && !shift && self.settings.undo_file_operations => self.undo_file_operation(cx),
            "down" => self.move_cursor(columns as isize, shift, control, window, cx),
            "up" => self.move_cursor(-(columns as isize), shift, control, window, cx),
            "right" => self.move_cursor(1, shift, control, window, cx),
            "left" => self.move_cursor(-1, shift, control, window, cx),
            "home" => self.cursor_to_edge(false, shift, window, cx),
            "end" => self.cursor_to_edge(true, shift, window, cx),
            "pageup" => {
                self.move_cursor(-((columns * 4) as isize), shift, control, window, cx)
            }
            "pagedown" => {
                self.move_cursor((columns * 4) as isize, shift, control, window, cx)
            }
            "space" => {
                self.toggle_cursor_selection(cx);
            }
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Render for XoolFiles {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let acrylic = self.settings.translucent_background && self.background_image.is_none();
        if self.background_uses_acrylic != acrylic {
            self.background_uses_acrylic = acrylic;
            window.set_background_appearance(if acrylic {
                gpui::WindowBackgroundAppearance::Blurred
            } else {
                gpui::WindowBackgroundAppearance::Opaque
            });
        }
        if self.panes.iter().any(|pane| pane.active_tab().needs_scan) {
            let pending: Vec<_> = self.panes.iter().map(Pane::active_tab)
                .filter(|tab| tab.needs_scan).map(|tab| tab.id).collect();
            for id in pending { self.start_scan_tab(id, cx); }
        }
        self.tick_workspace_visibility(window, cx);
        let fits = self.preview_fits_at(f32::from(window.viewport_size().width));
        if self.preview_fits != fits {
            self.preview_fits = fits;
            self.refresh_preview(cx);
        }
        let zooming = self.tick_icon_scale();
        let sidebar_moving = self.tick_sidebar();
        let settings_nav_moving = self.tick_settings_navigation();
        let now = Instant::now();
        let tab_gradient_moving = self.tab_gradient_reveal.tick(self.settings.active_tab_gradient, now);
        let tab_close_moving = self.tab_close_reveal.tick(self.settings.show_tab_close, now);
        if tab_gradient_moving || tab_close_moving {
            window.request_animation_frame();
        }
        self.sidebar_group_animations.retain(|_, (started, _)| started.elapsed() < Duration::from_millis(160));
        self.sidebar_group_offsets.retain(|_, (started, _)| started.elapsed() < Duration::from_millis(160));
        self.menu_tool_offsets.retain(|_, (started, _)| started.elapsed() < Duration::from_millis(160));
        self.sidebar_favorite_offsets.retain(|_, (started, _)| started.elapsed() < Duration::from_millis(160));
        self.tab_reorder_offsets.retain(|_, (started, _)| started.elapsed() < Duration::from_millis(160));
        self.column_offsets.retain(|_, (start, _)| start.elapsed() < Duration::from_millis(160));
        let scrolling = self.tick_wheel_scroll(crate::ui::settings_navigation_axis(f32::from(window.viewport_size().width)));
        if self.tick_media() || zooming || sidebar_moving || settings_nav_moving || scrolling || !self.column_offsets.is_empty() || !self.sidebar_group_animations.is_empty() || !self.sidebar_group_offsets.is_empty() || !self.sidebar_favorite_offsets.is_empty() || !self.menu_tool_offsets.is_empty() || !self.tab_reorder_offsets.is_empty() {
            window.request_animation_frame();
        }
        for image in self.image_drops.drain(..) {
            let _ = window.drop_image(image);
        }
        self.marquee_layouts.clear();
        self.workspace_ui.tick(&self.settings.workspaces.current().rows, window, cx.has_active_drag());
        self.sync_workspace_icons(cx);
        crate::ui::render(self, window, cx)
    }
}

// ----------------------------------------------------------------------
// Utilidades de presentación
// ----------------------------------------------------------------------

fn preview_editor_text(original: &str) -> String {
    original.strip_prefix('\u{feff}').unwrap_or(original).replace("\r\n", "\n")
}

fn recycle_item_from_entry(entry: &Entry) -> Option<fs_model::RecycleItem> {
    match (&entry.origin, &entry.recycle_meta) {
        (Some(origin), Some(meta)) => Some(fs_model::RecycleItem {
            recycled: entry.path.clone(),
            meta: meta.clone(),
            origin: origin.clone(),
        }),
        _ => None,
    }
}

fn shortcut_binding(id: &str, keys: &str) -> Option<KeyBinding> {
    if shortcuts::is_unbound(keys) {
        return None;
    }
    let context = Some("XoolFiles");
    Some(match id {
        "new_tab" => KeyBinding::new(keys, NewTab, context),
        "close_tab" => KeyBinding::new(keys, CloseTab, context),
        "next_tab" => KeyBinding::new(keys, NextTab, context),
        "prev_tab" => KeyBinding::new(keys, PrevTab, context),
        "back" => KeyBinding::new(keys, Back, context),
        "forward" => KeyBinding::new(keys, Forward, context),
        "up" => KeyBinding::new(keys, Up, context),
        "refresh" => KeyBinding::new(keys, Refresh, context),
        "focus_address" => KeyBinding::new(keys, FocusAddress, context),
        "focus_search" => KeyBinding::new(keys, FocusSearch, context),
        "recursive_search" => KeyBinding::new(keys, ToggleRecursiveSearch, context),
        "toggle_hidden" => KeyBinding::new(keys, ToggleHidden, context),
        "toggle_split" => KeyBinding::new(keys, ToggleSplit, context),
        "toggle_preview" => KeyBinding::new(keys, TogglePreview, context),
        "toggle_sidebar" => KeyBinding::new(keys, ToggleSidebar, context),
        "new_folder" => KeyBinding::new(keys, NewFolder, context),
        "rename" => KeyBinding::new(keys, RenameSelected, context),
        "delete" => KeyBinding::new(keys, DeleteSelected, context),
        "copy" => KeyBinding::new(keys, CopySelected, context),
        "cut" => KeyBinding::new(keys, CutSelected, context),
        "paste" => KeyBinding::new(keys, PasteFiles, context),
        "open" => KeyBinding::new(keys, OpenSelected, context),
        "select_all" => KeyBinding::new(keys, SelectAllFiles, context),
        "settings" => KeyBinding::new(keys, OpenSettings, context),
        "toggle_view" => KeyBinding::new(keys, ToggleViewMode, context),
        "favorite" => KeyBinding::new(keys, FavoriteCurrent, context),
        "zoom_in" => KeyBinding::new(keys, ZoomIn, context),
        "zoom_out" => KeyBinding::new(keys, ZoomOut, context),
        "zoom_reset" => KeyBinding::new(keys, ZoomReset, context),
        "sort_name" => KeyBinding::new(keys, SortByName, context),
        "sort_size" => KeyBinding::new(keys, SortBySize, context),
        "sort_kind" => KeyBinding::new(keys, SortByKind, context),
        "sort_modified" => KeyBinding::new(keys, SortByModified, context),
        "escape" => KeyBinding::new(keys, EscapeOverlay, context),
        _ => return None,
    })
}

fn audio_seek_at(bounds: Bounds<Pixels>, x: Pixels) -> f32 {
    (f32::from(x - bounds.origin.x) / f32::from(bounds.size.width)).clamp(0., 1.)
}

fn record_recent(recent: &mut Vec<PathBuf>, path: &Path) {
    recent.retain(|existing| existing != path);
    recent.insert(0, path.to_path_buf());
    recent.truncate(50);
}

async fn receive_shell_reply<T>(
    receiver: oneshot::Receiver<T>, timeout: Duration, cx: &gpui::AsyncApp,
) -> Option<T> {
    let timer = cx.background_executor().timer(timeout);
    futures::pin_mut!(timer);
    match select(receiver, timer).await {
        Either::Left((result, _)) => result.ok(),
        Either::Right(_) => None,
    }
}

fn load_shell_rows(
    id: u64, submenu: Option<(u32, usize)>, receiver: oneshot::Receiver<ShellRows>,
    cx: &mut Context<XoolFiles>,
) -> gpui::Task<()> {
    cx.spawn(async move |this, cx| {
        let result = receive_shell_reply(receiver, Duration::from_secs(3), cx).await;
        let _ = this.update(cx, |app, cx| {
            let Some(menu) = app.context_menu.as_mut().filter(|menu| menu.id == id) else { return };
            menu.loading = None;
            if result.is_none() && submenu.is_none() {
                menu.shell = None;
            }
            let mut rows = result.unwrap_or_else(|| Arc::from([]));
            reuse_menu_icons(&mut rows, &mut app.context_menu_icons, &mut app.image_drops);
            if let Some((key, depth)) = submenu {
                menu.submenus.insert(key, rows);
                menu.change_submenu(depth, Some(ContextPage::Shell(key)));
            } else {
                menu.system_rows = Some(rows);
            }
            cx.notify();
        });
    })
}

fn reuse_menu_icons(
    rows: &mut ShellRows, cache: &mut VecDeque<Arc<RenderImage>>, drops: &mut Vec<Arc<RenderImage>>,
) {
    const MAX_MENU_ICONS: usize = 80;
    for row in Arc::make_mut(rows) {
        let ShellRow::Item { icon: Some(icon), .. } = row else { continue };
        if let Some(position) = cache.iter().position(|saved| {
            Arc::ptr_eq(saved, icon) || (saved.size(0) == icon.size(0) && saved.as_bytes(0) == icon.as_bytes(0))
        })
            && let Some(saved) = cache.remove(position)
        {
            *icon = saved;
        } else if cache.len() == MAX_MENU_ICONS
            && let Some(oldest) = cache.pop_front()
        {
            drops.push(oldest);
        }
        cache.push_back(icon.clone());
    }
}

fn transfer_directories(paths: &[PathBuf], destination: &Path, moving: bool) -> Vec<PathBuf> {
    let mut directories = vec![destination.to_path_buf()];
    if moving { directories.extend(paths.iter().filter_map(|path| path.parent().map(Path::to_path_buf))); }
    directories
}

fn restored_tabs(paths: &[PathBuf], fallback: &Path) -> Vec<(Option<usize>, PathBuf)> {
    let mut tabs: Vec<_> = paths.iter().enumerate()
        .filter(|(_, path)| path.is_dir() || fs_model::is_recycle_bin(path))
        .map(|(index, path)| (Some(index), path.clone()))
        .collect();
    if tabs.is_empty() {
        tabs.push((None, fallback.to_path_buf()));
    }
    tabs
}

fn same_favorite_path(left: &Path, right: &Path) -> bool {
    left == right || left.as_os_str().eq_ignore_ascii_case(right.as_os_str())
}

pub fn folder_name(path: &Path) -> String {
    if fs_model::is_recycle_bin(path) {
        return tr("Papelera", "Recycle Bin").to_string();
    }
    // La carpeta del perfil se muestra como "Inicio", igual que en Windows.
    if let Some(home) = std::env::var_os("USERPROFILE") {
        let home = Path::new(&home);
        if path == home
            || path
                .to_string_lossy()
                .eq_ignore_ascii_case(&home.to_string_lossy())
        {
            return tr("Inicio", "Home").to_string();
        }
    }
    if let Some(name) = path.file_name() {
        return name.to_string_lossy().into_owned();
    }
    let text = fs_model::normalize_path(path);
    text.trim_end_matches('\\').to_string()
}

fn address_text(path: &Path) -> String {
    if fs_model::is_recycle_bin(path) {
        tr("Papelera", "Recycle Bin").to_string()
    } else {
        fs_model::normalize_path(path)
    }
}

pub fn breadcrumb_parts(path: &Path) -> Vec<(String, PathBuf)> {
    if fs_model::is_recycle_bin(path) {
        return vec![(tr("Papelera", "Recycle Bin").to_string(), path.to_path_buf())];
    }
    let mut parts = Vec::new();
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match component {
            Component::Prefix(prefix) => {
                let label = prefix
                    .as_os_str()
                    .to_string_lossy()
                    .trim_end_matches('\\')
                    .to_string();
                if !label.is_empty() {
                    parts.push((label, current.clone()));
                }
            }
            Component::RootDir => {
                if let Some((_, root)) = parts.last_mut() { *root = current.clone(); }
            }
            Component::Normal(name) => {
                parts.push((name.to_string_lossy().into_owned(), current.clone()));
            }
            _ => {}
        }
    }
    if let Some(home) = std::env::var_os("USERPROFILE") {
        let home = Path::new(&home);
        if path == home
            || path
                .to_string_lossy()
                .eq_ignore_ascii_case(&home.to_string_lossy())
        {
            if let Some(last) = parts.last_mut() {
                last.0 = tr("Inicio", "Home").to_string();
            }
        }
    }
    parts
}

fn expand_path(value: &str, base: &Path) -> PathBuf {
    let mut value = value.trim().to_string();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        value = value[1..value.len() - 1].to_string();
    }
    if value.is_empty() {
        return base.to_path_buf();
    }
    if value == "~" || value.starts_with("~/") || value.starts_with("~\\") {
        let home = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\"));
        if value == "~" {
            return home;
        }
        return home.join(&value[2..]);
    }
    let mut expanded = value.clone();
    if let Some(rest) = value.strip_prefix('%')
        && let Some(end) = rest.find('%')
    {
        let variable = &rest[..end];
        if let Ok(replacement) = std::env::var(variable) {
            expanded = format!("{}{}", replacement, &rest[end + 1..]);
        }
    }
    let path = PathBuf::from(expanded);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|'])
}

fn quick_glyph(index: usize) -> &'static str {
    match index {
        0 => "\u{E80F}",
        1 => "\u{E8FC}",
        2 => "\u{E896}",
        3 => "\u{E8A5}",
        4 => "\u{E91B}",
        5 => "\u{E8D6}",
        6 => "\u{E714}",
        _ => "\u{E8B7}",
    }
}

fn quick_color(index: usize) -> u32 {
    match index {
        0 => 0xffb454,
        1 => 0x6ea8fe,
        2 => 0x5bd6c8,
        3 => 0x9db4ff,
        4 => 0xf28db2,
        5 => 0xb18cff,
        6 => 0xff7a66,
        _ => 0x9aa7b8,
    }
}









