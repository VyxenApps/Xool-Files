use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, atomic::{AtomicU64, Ordering}},
    time::{Duration, Instant},
};

use gpui::{AppContext, Bounds, Context, Entity, Focusable, MouseMoveEvent, Pixels, RenderImage, Task, Window};
use gpui_component::input::{InputEvent, InputState};
use serde::{Deserialize, Serialize};

use crate::{app::XoolFiles, i18n::tr, motion::VisibilityMotion, search_index::SearchTerms, settings::Accent};

pub(crate) const PIN_PAGE_SIZES: [usize; 4] = [2, 4, 6, 8];
const DEFAULT_PIN_PAGE_SIZE: usize = 6;
const MIN_PANEL_WIDTH: f32 = 152.;
const ICON_PANEL_WIDTH: f32 = 54.;
const PANEL_SNAP_WIDTH: f32 = MIN_PANEL_WIDTH / 2.;
const MAX_PANEL_WIDTH: f32 = 420.;
pub(crate) const ROW_HEIGHT: f32 = 30.;
pub(crate) const ROW_GAP: f32 = 4.;
pub(crate) const ROW_STRIDE: f32 = ROW_HEIGHT + ROW_GAP;
const SLIDE_TIME: Duration = Duration::from_millis(165);

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Workspace {
    pub name: String,
    pub color: Option<Accent>,
    pub icon: Option<WorkspaceIcon>,
    pub pins: Vec<Option<PathBuf>>,
    pub file_pins: HashSet<PathBuf>,
    pub rows: Vec<Row>,
    pub panel_width: Option<f32>,
    pub access_styles: HashMap<PathBuf, AccessStyle>,
    pub recent_accesses: Vec<PathBuf>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct AccessStyle {
    pub name: Option<String>,
    pub icon: Option<WorkspaceIcon>,
    pub color: Option<Accent>,
}

impl Workspace {
    pub fn access_name(&self, path: &Path) -> String {
        self.access_styles.get(path).and_then(|style| style.name.clone())
            .unwrap_or_else(|| crate::app::folder_name(path))
    }

    pub fn row_order(&self, order: AccessOrder, parent: Option<u64>) -> Vec<usize> {
        let mut indices: Vec<_> = self.rows.iter().enumerate()
            .filter_map(|(index, row)| (row.parent == parent).then_some(index)).collect();
        if order == AccessOrder::Manual { return indices; }
        let mut start = 0;
        for end in indices.iter().enumerate()
            .filter_map(|(position, &index)| (!self.rows[index].item.is_shortcut()).then_some(position))
            .chain(std::iter::once(indices.len())).collect::<Vec<_>>()
        {
            let group = &mut indices[start..end];
            match order {
                AccessOrder::Alphabetical => group.sort_by_cached_key(|&i| {
                    match &self.rows[i].item {
                        Item::Web(url) => Some(web_shortcut_title(url).to_lowercase()),
                        item => item.path().map(|path| self.access_name(path).to_lowercase()),
                    }
                }),
                AccessOrder::Recent => group.sort_by_cached_key(|&i| self.rows[i].item.path()
                    .and_then(|path| self.recent_accesses.iter().position(|recent| same_path(recent, path)))
                    .unwrap_or(usize::MAX)),
                AccessOrder::Manual => unreachable!(),
            }
            start = end + 1;
        }
        indices
    }

    fn visible_positions(&self, order: AccessOrder, stride: f32) -> HashMap<u64, f32> {
        let mut positions = HashMap::new();
        let mut top = 0.;
        for index in self.row_order(order, None) {
            let row = &self.rows[index];
            positions.insert(row.id, top);
            top += stride;
            if matches!(row.item, Item::Group { collapsed: false, .. }) {
                for child in self.row_order(order, Some(row.id)) {
                    positions.insert(self.rows[child].id, top);
                    top += stride;
                }
            }
        }
        positions
    }

    fn rows_in_display_order(&self, order: AccessOrder) -> Vec<Row> {
        self.row_order(order, None).into_iter().flat_map(|index| {
            std::iter::once(index).chain(self.row_order(order, Some(self.rows[index].id)))
        }).map(|index| self.rows[index].clone()).collect()
    }

    fn projected_row_offsets(&self, id: u64, destination: RowDestination, order: AccessOrder, stride: f32) -> HashMap<u64, f32> {
        let parent = self.rows.iter().find(|row| row.id == id).map(|row| row.parent);
        let target_parent = self.destination(destination).map(|(_, parent)| parent);
        if parent.is_none() || target_parent != parent { return HashMap::new(); }
        let mut preview = Workspace { rows: self.rows_in_display_order(order), ..Workspace::default() };
        if !preview.move_row(id, destination) { return HashMap::new(); }
        let before = self.visible_positions(order, stride);
        preview.visible_positions(AccessOrder::Manual, stride).into_iter()
            .filter_map(|(id, top)| {
                let offset = top - before.get(&id)?;
                (offset != 0.).then_some((id, offset))
            }).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkspaceIcon {
    Folder,
    Briefcase,
    Home,
    Star,
    Images,
    Music,
    Code,
    Books,
}

impl WorkspaceIcon {
    pub fn options() -> [(Self, &'static str); 8] {
        [
            (Self::Folder, tr("Carpeta", "Folder")),
            (Self::Briefcase, tr("Maletín", "Briefcase")),
            (Self::Home, tr("Casa", "Home")),
            (Self::Star, tr("Estrella", "Star")),
            (Self::Images, tr("Imágenes", "Images")),
            (Self::Music, tr("Música", "Music")),
            (Self::Code, tr("Código", "Code")),
            (Self::Books, tr("Biblioteca", "Library")),
        ]
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Self::Folder => "\u{E8B7}",
            Self::Briefcase => "\u{E821}",
            Self::Home => "\u{E80F}",
            Self::Star => "\u{E734}",
            Self::Images => "\u{EB9F}",
            Self::Music => "\u{E8D6}",
            Self::Code => "\u{E943}",
            Self::Books => "\u{E8F1}",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Row {
    pub id: u64,
    pub item: Item,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum Item {
    Folder(PathBuf),
    File(PathBuf),
    Web(String),
    Divider(String),
    Group { name: String, collapsed: bool, #[serde(default)] style: AccessStyle },
}

impl Item {
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Folder(path) | Self::File(path) => Some(path),
            Self::Web(_) | Self::Divider(_) | Self::Group { .. } => None,
        }
    }

    pub(crate) fn is_shortcut(&self) -> bool { self.path().is_some() || matches!(self, Self::Web(_)) }
}

fn parse_web_shortcut(value: &str) -> Option<url::Url> {
    let url = url::Url::parse(value.trim()).ok()?;
    (matches!(url.scheme(), "https" | "http") && url.host().is_some()).then_some(url)
}

pub(crate) fn web_shortcut_title(value: &str) -> String {
    parse_web_shortcut(value).and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| value.to_owned())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowDestination {
    Before(u64),
    After(u64),
    Group(u64),
    End,
}

impl Workspace {
    pub fn label(&self) -> &str {
        if self.name.is_empty() { tr("Principal", "Main") } else { &self.name }
    }

    pub fn page_count(&self, page_size: usize) -> usize {
        self.pins.len().div_ceil(page_size).max(1)
    }

    fn add_row(&mut self, index: usize, item: Item) -> u64 {
        let ids: HashSet<_> = self.rows.iter().map(|row| row.id).collect();
        let id = (1..).find(|id| !ids.contains(id)).unwrap();
        self.rows.insert(index.min(self.rows.len()), Row { id, item, parent: None });
        id
    }

    fn add_shortcuts(&mut self, paths: impl IntoIterator<Item = (PathBuf, bool)>, index: usize) -> bool {
        self.add_shortcuts_in(paths, index, None)
    }

    fn add_shortcuts_in(&mut self, paths: impl IntoIterator<Item = (PathBuf, bool)>, mut index: usize, parent: Option<u64>) -> bool {
        let mut changed = false;
        for (path, file) in paths {
            if self.rows.iter().any(|row| row.item.path().is_some_and(|existing| same_path(existing, &path))) {
                continue;
            }
            index = index.min(self.rows.len());
            self.add_row(index, if file { Item::File(path) } else { Item::Folder(path) });
            self.rows[index].parent = parent;
            index += 1;
            changed = true;
        }
        changed
    }

    fn destination(&self, destination: RowDestination) -> Option<(usize, Option<u64>)> {
        let anchor = match destination {
            RowDestination::Before(id) | RowDestination::After(id) | RowDestination::Group(id) => id,
            RowDestination::End => return Some((self.rows.len(), None)),
        };
        let index = self.rows.iter().position(|row| row.id == anchor)?;
        let row = &self.rows[index];
        match destination {
            RowDestination::Before(_) => Some((index, row.parent)),
            RowDestination::After(_) => Some((index + 1, row.parent)),
            RowDestination::Group(id) if matches!(row.item, Item::Group { .. }) => Some((
                self.rows.iter().rposition(|row| row.parent == Some(id)).map_or(index + 1, |last| last + 1),
                Some(id),
            )),
            _ => None,
        }
    }

    fn move_row(&mut self, id: u64, destination: RowDestination) -> bool {
        let Some(from) = self.rows.iter().position(|row| row.id == id) else { return false; };
        let Some((to, parent)) = self.destination(destination) else { return false; };
        if parent.is_some() && !self.rows[from].item.is_shortcut() { return false; }
        let to = to - usize::from(from < to);
        if from == to && self.rows[from].parent == parent { return false; }
        let mut row = self.rows.remove(from);
        row.parent = parent;
        self.rows.insert(to, row);
        true
    }

    fn trim_pins(&mut self) {
        self.pins.truncate(self.pins.iter().rposition(Option::is_some).map_or(0, |index| index + 1));
        self.file_pins.retain(|path| self.pins.iter().flatten().any(|pin| pin == path));
    }

    /// Existing pins can swap slots. A new folder uses the requested empty slot
    /// or the first available slot, including the first slot on a new page.
    fn pin(&mut self, path: PathBuf, target: Option<usize>) -> usize {
        let previous = self.pins.iter().position(|pin| pin.as_ref().is_some_and(|pin| same_path(pin, &path)));
        let first_free = self.pins.iter().position(Option::is_none).unwrap_or(self.pins.len());
        let target = if previous.is_some() {
            target.or(previous).unwrap_or(first_free)
        } else {
            target.filter(|&slot| self.pins.get(slot).is_none_or(Option::is_none)).unwrap_or(first_free)
        };
        if self.pins.len() <= target { self.pins.resize(target + 1, None); }
        if let Some(previous) = previous {
            self.pins.swap(previous, target);
        } else {
            self.pins[target] = Some(path);
        }
        self.trim_pins();
        target
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Preferences {
    pub enabled: bool,
    pub show_titlebar_button: bool,
    pub right: bool,
    pub compact: bool,
    pub icon_only_panel: bool,
    pub width: Option<f32>,
    pub remember_width_per_space: bool,
    pub row_density: RowDensity,
    pub access_order: AccessOrder,
    pub highlight_open_folder: bool,
    pub pin_page_size: usize,
    pub show_pin_names: bool,
    pub show_pin_close: bool,
    pub pin_close_left: bool,
    pub pin_close_always: bool,
    pub use_system_icons: bool,
    pub divider_alignment: DividerAlignment,
    pub startup_space: Option<usize>,
    pub selected: usize,
    pub spaces: Vec<Workspace>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            enabled: false, show_titlebar_button: true, right: false, compact: false, icon_only_panel: false, width: None, pin_page_size: DEFAULT_PIN_PAGE_SIZE,
            remember_width_per_space: false, row_density: RowDensity::Normal,
            access_order: AccessOrder::Manual, highlight_open_folder: true,
            show_pin_names: true, show_pin_close: true, pin_close_left: false, pin_close_always: false,
            use_system_icons: false, divider_alignment: DividerAlignment::Left, startup_space: None,
            selected: 0, spaces: vec![Workspace::default()],
        }
    }
}

impl Preferences {
    pub fn validate(&mut self) {
        self.width = self.width.filter(|width| width.is_finite())
            .map(|width| width.clamp(MIN_PANEL_WIDTH, MAX_PANEL_WIDTH));
        if !PIN_PAGE_SIZES.contains(&self.pin_page_size) { self.pin_page_size = DEFAULT_PIN_PAGE_SIZE; }
        if self.spaces.is_empty() { self.spaces.push(Workspace::default()); }
        self.selected = self.selected.min(self.spaces.len() - 1);
        self.startup_space = self.startup_space.filter(|&index| index < self.spaces.len());
        for space in &mut self.spaces {
            space.panel_width = space.panel_width.filter(|width| width.is_finite())
                .map(|width| width.clamp(MIN_PANEL_WIDTH, MAX_PANEL_WIDTH));
            space.recent_accesses.truncate(100);
            let mut ids = HashSet::new();
            space.rows.retain(|row| ids.insert(row.id));
            let groups: HashSet<_> = space.rows.iter()
                .filter(|row| matches!(row.item, Item::Group { .. })).map(|row| row.id).collect();
            for row in &mut space.rows {
                row.parent = row.parent.filter(|parent| row.item.is_shortcut() && groups.contains(parent));
            }
            space.trim_pins();
        }
    }

    pub fn current(&self) -> &Workspace { &self.spaces[self.selected] }
    pub fn panel_width(&self) -> Option<f32> {
        if self.remember_width_per_space { self.current().panel_width.or(self.width) } else { self.width }
    }
    pub fn startup_index(&self) -> usize {
        self.startup_space.filter(|&index| index < self.spaces.len()).unwrap_or(self.selected)
    }
    pub fn current_mut(&mut self) -> &mut Workspace { &mut self.spaces[self.selected] }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RowDensity { Compact, #[default] Normal, Comfortable }

impl RowDensity {
    pub const OPTIONS: [Self; 3] = [Self::Compact, Self::Normal, Self::Comfortable];
    pub fn stride(self) -> f32 {
        match self { Self::Compact => 28., Self::Normal => ROW_STRIDE, Self::Comfortable => 42. }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AccessOrder { #[default] Manual, Alphabetical, Recent }

impl AccessOrder {
    pub const OPTIONS: [Self; 3] = [Self::Manual, Self::Alphabetical, Self::Recent];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DividerAlignment {
    #[default]
    Left,
    Center,
    Right,
}

impl DividerAlignment {
    pub const OPTIONS: [Self; 3] = [Self::Left, Self::Center, Self::Right];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NameEdit {
    Preset,
    SettingsPreset,
    Divider(usize),
    RenameDivider(u64),
    Group(usize),
    Web(usize),
    RenameGroup(u64),
    RenameAccess(AccessTarget),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AccessTarget { Row(u64), Pin(usize) }

impl AccessTarget {
    fn path(self, space: &Workspace) -> Option<&Path> {
        match self {
            Self::Row(id) => space.rows.iter().find(|row| row.id == id)?.item.path(),
            Self::Pin(index) => space.pins.get(index)?.as_deref(),
        }
    }

    pub fn style(self, space: &Workspace) -> Option<&AccessStyle> {
        if let Self::Row(id) = self
            && let Some(Row { item: Item::Group { style, .. }, .. }) =
                space.rows.iter().find(|row| row.id == id)
        {
            return Some(style);
        }
        space.access_styles.get(self.path(space)?)
    }

    pub fn style_mut(self, space: &mut Workspace) -> Option<&mut AccessStyle> {
        let path = match self {
            Self::Row(id) => match &mut space.rows.iter_mut().find(|row| row.id == id)?.item {
                Item::Group { style, .. } => return Some(style),
                item => item.path()?.to_path_buf(),
            },
            Self::Pin(index) => space.pins.get(index)?.clone()?,
        };
        Some(space.access_styles.entry(path).or_default())
    }
}

pub(crate) struct PanelState {
    panel_motion: VisibilityMotion,
    button_motion: VisibilityMotion,
    pub search: Entity<InputState>,
    pub name: Entity<InputState>,
    pub edit: Option<NameEdit>,
    pub edit_version: usize,
    pub results: Vec<PathBuf>,
    pub searching: bool,
    pub search_failed: bool,
    pub page: usize,
    pub page_motion: Option<(usize, f32)>,
    pub pin_motion: Option<(usize, usize)>,
    pub row_arrival: Option<u64>,
    pub row_target: Option<RowDestination>,
    pub dragged_row: Option<u64>,
    pub appearance_workspace: Option<usize>,
    pub resize: Option<(f32, f32)>,
    pub scroll: gpui::ScrollHandle,
    version: usize,
    groups: HashMap<u64, VisibilityMotion>,
    row_offsets: HashMap<u64, (Instant, f32)>,
    projected_offsets: HashMap<u64, f32>,
    drop_bounds: Option<Bounds<Pixels>>,
    search_generation: Arc<AtomicU64>,
    search_task: Option<Task<()>>,
    icons: HashMap<PathBuf, Option<Arc<RenderImage>>>,
    icon_task: Option<Task<()>>,
}

impl PanelState {
    pub fn new(prefs: &Preferences, window: &mut Window, cx: &mut Context<XoolFiles>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx)
            .placeholder(tr("Buscar carpetas…", "Find folders…")));
        let name = cx.new(|cx| InputState::new(window, cx));
        cx.subscribe_in(&search, window, |app, input, event, window, cx| match event {
            InputEvent::Change => {
                let query = input.read(cx).value().to_string();
                app.search_workspace_folders(query, cx);
            }
            InputEvent::PressEnter { .. } => {
                if let Some(path) = app.workspace_ui.results.first().cloned() {
                    app.add_workspace_folder(path, window, cx);
                }
            }
            _ => {}
        }).detach();
        cx.subscribe_in(&name, window, |app, input, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                let name = input.read(cx).value().trim().to_owned();
                app.commit_workspace_name(name, window, cx);
            }
        }).detach();
        Self {
            panel_motion: VisibilityMotion::new(prefs.enabled),
            button_motion: VisibilityMotion::new(prefs.show_titlebar_button),
            search, name, edit: None, edit_version: 0, results: Vec::new(),
            searching: false, search_failed: false, page: 0, page_motion: None,
            pin_motion: None, row_arrival: None, row_target: None, dragged_row: None, appearance_workspace: None, resize: None,
            scroll: gpui::ScrollHandle::new(),
            version: 0, groups: HashMap::new(), row_offsets: HashMap::new(),
            projected_offsets: HashMap::new(), drop_bounds: None,
            search_generation: Arc::new(AtomicU64::new(0)), search_task: None,
            icons: HashMap::new(), icon_task: None,
        }
    }

    pub fn panel_progress(&self) -> f32 { self.panel_motion.progress }
    pub fn button_progress(&self) -> f32 { self.button_motion.progress }

    pub fn tick(&mut self, rows: &[Row], window: &mut Window, dragging: bool) {
        if self.drop_bounds.is_some_and(|bounds| !dragging || !bounds.contains(&window.mouse_position())) {
            self.clear_row_preview();
        }
        let now = Instant::now();
        self.row_offsets.retain(|_, (started, _)| now.duration_since(*started) < SLIDE_TIME);
        if !self.row_offsets.is_empty() { window.request_animation_frame(); }
        for row in rows {
            if let Item::Group { collapsed, .. } = row.item {
                let motion = self.groups.entry(row.id).or_insert_with(|| VisibilityMotion::new(!collapsed));
                if motion.tick(!collapsed, now, SLIDE_TIME) { window.request_animation_frame(); }
            }
        }
    }

    pub fn group_progress(&self, id: u64, collapsed: bool) -> f32 {
        self.groups.get(&id).map_or(if collapsed { 0. } else { 1. }, |motion| motion.progress)
    }

    pub fn row_offset(&self, id: u64) -> f32 {
        self.projected_offsets.get(&id).copied().unwrap_or(0.) + self.row_offsets.get(&id).map_or(0., |(started, offset)| {
            let remaining = 1. - started.elapsed().as_secs_f32() / SLIDE_TIME.as_secs_f32();
            offset * remaining.max(0.).powi(3)
        })
    }

    fn animate_projection(&mut self, next: HashMap<u64, f32>) {
        if self.projected_offsets == next { return; }
        let ids: HashSet<_> = self.projected_offsets.keys().chain(self.row_offsets.keys())
            .chain(next.keys()).copied().collect();
        let now = Instant::now();
        self.row_offsets = ids.into_iter().filter_map(|id| {
            let offset = self.row_offset(id) - next.get(&id).copied().unwrap_or(0.);
            (offset.abs() > 0.5).then_some((id, (now, offset)))
        }).collect();
        self.projected_offsets = next;
    }

    fn clear_row_preview(&mut self) {
        self.row_target = None;
        self.dragged_row = None;
        self.drop_bounds = None;
        if !self.projected_offsets.is_empty() { self.animate_projection(HashMap::new()); }
    }

    fn reset_layout(&mut self) {
        self.edit = None;
        self.resize = None;
        self.row_target = None;
        self.dragged_row = None;
        self.drop_bounds = None;
        self.groups.clear();
        self.row_offsets.clear();
        self.projected_offsets.clear();
        self.page = 0;
        self.page_motion = None;
        self.pin_motion = None;
        self.row_arrival = None;
        self.scroll = gpui::ScrollHandle::new();
        self.icons.clear();
        self.icon_task = None;
    }

    pub fn system_icon(&self, path: &Path) -> Option<Arc<RenderImage>> {
        self.icons.get(path).cloned().flatten()
    }
}

impl XoolFiles {
    pub(crate) fn toggle_workspace_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.workspaces.enabled = !self.settings.workspaces.enabled;
        if !self.settings.workspaces.enabled {
            self.end_workspace_resize(cx);
            let ui = &self.workspace_ui;
            if ui.search.read(cx).focus_handle(cx).is_focused(window)
                || (ui.edit != Some(NameEdit::SettingsPreset) && ui.name.read(cx).focus_handle(cx).is_focused(window))
            {
                self.focus_root(window);
            }
        }
        self.settings.save();
        cx.notify();
    }

    pub(crate) fn tick_workspace_visibility(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prefs = &self.settings.workspaces;
        let ui = &mut self.workspace_ui;
        let previous = ui.panel_progress();
        let now = Instant::now();
        let panel_animating = ui.panel_motion.tick(prefs.enabled, now, SLIDE_TIME);
        let button_animating = ui.button_motion.tick(prefs.show_titlebar_button, now, SLIDE_TIME);
        if panel_animating || button_animating { window.request_animation_frame(); }
        if !prefs.enabled && previous > 0. && ui.panel_progress() == 0. {
            let settings_edit = ui.edit == Some(NameEdit::SettingsPreset);
            self.reset_workspace_panel(window, cx);
            if settings_edit { self.workspace_ui.edit = Some(NameEdit::SettingsPreset); }
        }
    }

    pub(crate) fn sync_workspace_icons(&mut self, cx: &mut Context<Self>) {
        let prefs = &self.settings.workspaces;
        let ui = &mut self.workspace_ui;
        if (!prefs.enabled && ui.panel_progress() == 0.) || !prefs.use_system_icons {
            ui.icons.clear();
            ui.icon_task = None;
            return;
        }

        let space = prefs.current();
        let first = ui.page * prefs.pin_page_size;
        let pins = space.pins.iter().skip(first).take(prefs.pin_page_size).flatten()
            .map(|path| (path, !space.file_pins.contains(path)));
        let rows = space.rows.iter().filter_map(|row| match &row.item {
            Item::Folder(path) => Some((path, true)),
            Item::File(path) => Some((path, false)),
            Item::Web(_) | Item::Divider(_) | Item::Group { .. } => None,
        });
        let paths: HashMap<_, _> = pins.chain(rows).chain(ui.results.iter().map(|path| (path, true))).collect();
        ui.icons.retain(|path, _| paths.contains_key(path));
        if ui.icon_task.is_some() { return; }
        let missing: Vec<_> = paths.into_iter().filter(|(path, _)| !ui.icons.contains_key(*path))
            .take(24).map(|(path, directory)| (path.clone(), directory)).collect();
        if missing.is_empty() { return; }
        ui.icon_task = Some(cx.spawn(async move |this, cx| {
            let icons = cx.background_executor().spawn(async move {
                missing.into_iter().map(|(path, directory)| {
                    let icon = crate::platform::shell_icon(&path, directory, 32);
                    (path, icon)
                }).collect::<Vec<_>>()
            }).await;
            let _ = this.update(cx, |app, cx| {
                app.workspace_ui.icon_task = None;
                app.workspace_ui.icons.extend(icons);
                cx.notify();
            });
        }));
    }

    pub(crate) fn workspace_extent(&self, viewport_width: f32) -> f32 {
        self.workspace_panel_width(viewport_width) * self.workspace_ui.panel_progress()
    }

    pub(crate) fn workspace_panel_width(&self, viewport_width: f32) -> f32 {
        if self.settings.workspaces.icon_only_panel { return ICON_PANEL_WIDTH; }
        let default = (viewport_width * 0.4).clamp(MIN_PANEL_WIDTH, 264.);
        self.settings.workspaces.panel_width().unwrap_or(default)
            .clamp(MIN_PANEL_WIDTH, max_panel_width(viewport_width))
    }

    pub(crate) fn begin_workspace_resize(&mut self, x: f32, window: &Window, cx: &mut Context<Self>) {
        if !self.settings.workspaces.enabled || self.workspace_ui.panel_progress() < 1. { return; }
        let width = self.workspace_panel_width(f32::from(window.viewport_size().width));
        self.workspace_ui.resize = Some((x, width));
        cx.notify();
    }

    pub(crate) fn drag_workspace_resize(&mut self, event: &MouseMoveEvent, window: &Window, cx: &mut Context<Self>) {
        let Some((start_x, start_width)) = self.workspace_ui.resize else { return; };
        let delta = f32::from(event.position.x) - start_x;
        let viewport = f32::from(window.viewport_size().width);
        let requested = start_width + if self.settings.workspaces.right { -delta } else { delta };
        let prefs = &mut self.settings.workspaces;
        let icon_only = snap_to_icons(requested, prefs.icon_only_panel);
        let width = requested.clamp(MIN_PANEL_WIDTH, max_panel_width(viewport));
        if icon_only != prefs.icon_only_panel
            || (!icon_only && prefs.panel_width().is_none_or(|saved| (width - saved).abs() >= 1.))
        {
            prefs.icon_only_panel = icon_only;
            if icon_only && self.workspace_ui.edit != Some(NameEdit::SettingsPreset) {
                self.workspace_ui.edit = None;
            }
            if !icon_only {
                if prefs.remember_width_per_space { prefs.current_mut().panel_width = Some(width); }
                else { prefs.width = Some(width); }
            }
            cx.notify();
        }
    }

    pub(crate) fn expand_workspace_panel(&mut self, cx: &mut Context<Self>) {
        self.settings.workspaces.icon_only_panel = false;
        self.settings.save();
        cx.notify();
    }

    pub(crate) fn end_workspace_resize(&mut self, cx: &mut Context<Self>) {
        if self.workspace_ui.resize.take().is_some() {
            self.settings.save();
            cx.notify();
        }
    }


    pub(crate) fn select_workspace(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.settings.workspaces.spaces.len() { return; }
        self.settings.workspaces.selected = index;
        self.reset_workspace_panel(window, cx);
        self.settings.save();
        cx.notify();
    }

    pub(crate) fn reset_workspace_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace_ui.reset_layout();
        self.clear_workspace_query(window, cx);
    }

    pub(crate) fn clear_workspace_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace_ui.search.update(cx, |input, cx| input.set_value("", window, cx));
        self.search_workspace_folders(String::new(), cx);
    }

    pub(crate) fn edit_workspace_name(&mut self, edit: NameEdit, window: &mut Window, cx: &mut Context<Self>) {
        if !self.settings.workspaces.enabled && edit != NameEdit::SettingsPreset { return; }
        if self.settings.workspaces.icon_only_panel && edit != NameEdit::SettingsPreset {
            self.expand_workspace_panel(cx);
        }
        let text = match edit {
            NameEdit::RenameAccess(target) => target.path(self.settings.workspaces.current())
                .map(|path| self.settings.workspaces.current().access_name(path)).unwrap_or_default(),
            NameEdit::RenameDivider(id) | NameEdit::RenameGroup(id) => self.settings.workspaces.current().rows.iter().find_map(|row| {
                match &row.item {
                    Item::Divider(name) | Item::Group { name, .. } if row.id == id => Some(name.clone()),
                    _ => None,
                }
            }).unwrap_or_default(),
            _ => String::new(),
        };
        let placeholder = match edit {
            NameEdit::RenameAccess(_) => tr("Nombre del acceso", "Shortcut name"),
            NameEdit::Preset | NameEdit::SettingsPreset => tr("Nombre del espacio", "Workspace name"),
            NameEdit::Group(_) | NameEdit::RenameGroup(_) => tr("Nombre de la carpeta", "Folder name"),
            NameEdit::Web(_) => "https://…",
            _ => tr("Nombre de la división", "Divider name"),
        };
        self.workspace_ui.edit = Some(edit);
        self.workspace_ui.edit_version += 1;
        self.workspace_ui.name.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx);
            input.set_value(text, window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    pub(crate) fn cancel_workspace_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace_ui.edit = None;
        self.focus_handle.focus(window);
        cx.notify();
    }

    pub(crate) fn commit_workspace_name(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let web_url = if matches!(self.workspace_ui.edit, Some(NameEdit::Web(_))) {
            let Some(url) = parse_web_shortcut(&name) else {
                self.toast(tr("Introduce una URL que empiece por http:// o https://.", "Enter a URL starting with http:// or https://."),
                    crate::app::ToastTone::Error, cx);
                return;
            };
            Some(url.to_string())
        } else { None };
        if name.trim().is_empty() && !matches!(
            self.workspace_ui.edit, Some(NameEdit::Divider(_) | NameEdit::RenameDivider(_) | NameEdit::RenameAccess(_))
        ) { return; }
        let Some(edit) = self.workspace_ui.edit.take() else { return; };
        match edit {
            NameEdit::RenameAccess(target) => {
                let space = self.settings.workspaces.current_mut();
                if let Some(path) = target.path(space).map(Path::to_path_buf) {
                    let style = space.access_styles.entry(path).or_default();
                    style.name = (!name.is_empty()).then_some(name);
                }
            }
            NameEdit::Preset => {
                self.settings.workspaces.spaces.push(Workspace { name, ..Workspace::default() });
                self.select_workspace(self.settings.workspaces.spaces.len() - 1, window, cx);
                self.focus_handle.focus(window);
                return;
            }
            NameEdit::SettingsPreset => {
                self.settings.workspaces.spaces.push(Workspace { name, ..Workspace::default() });
                self.workspace_ui.appearance_workspace = Some(self.settings.workspaces.spaces.len() - 1);
            }
            NameEdit::Divider(index) => {
                let id = self.settings.workspaces.current_mut().add_row(index, Item::Divider(name));
                self.workspace_ui.row_arrival = Some(id);
            }
            NameEdit::RenameDivider(id) => {
                if let Some(Row { item: Item::Divider(current), .. }) = self.settings.workspaces.current_mut().rows.iter_mut().find(|row| row.id == id) {
                    *current = name;
                }
            }
            NameEdit::Group(index) => {
                let id = self.settings.workspaces.current_mut().add_row(index, Item::Group {
                    name, collapsed: false, style: AccessStyle::default(),
                });
                self.workspace_ui.row_arrival = Some(id);
            }
            NameEdit::Web(index) => {
                if let Some(url) = web_url {
                    let space = self.settings.workspaces.current_mut();
                    if !space.rows.iter().any(|row| matches!(&row.item, Item::Web(existing) if *existing == url)) {
                        self.workspace_ui.row_arrival = Some(space.add_row(index, Item::Web(url)));
                    }
                }
            }
            NameEdit::RenameGroup(id) => {
                if let Some(Row { item: Item::Group { name: current, .. }, .. }) = self.settings.workspaces.current_mut().rows.iter_mut().find(|row| row.id == id) {
                    *current = name;
                }
            }
        }
        self.settings.save();
        self.focus_handle.focus(window);
        cx.notify();
    }

    pub(crate) fn rename_workspace_access(&mut self, target: AccessTarget, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_workspace_query(window, cx);
        self.edit_workspace_name(NameEdit::RenameAccess(target), window, cx);
    }

    fn search_workspace_folders(&mut self, query: String, cx: &mut Context<Self>) {
        let ui = &mut self.workspace_ui;
        let token = ui.search_generation.fetch_add(1, Ordering::Relaxed) + 1;
        ui.search_task = None;
        ui.results.clear();
        ui.searching = false;
        ui.search_failed = false;
        let query = query.trim();
        cx.notify();
        if query.is_empty() || !self.settings.workspaces.enabled { return; }
        let folder = self.active_tab().path.clone();
        let query = query.to_owned();
        let ignore_accents = self.settings.search_ignore_accents;
        let show_hidden = self.settings.show_hidden;
        let generation = self.workspace_ui.search_generation.clone();
        let index = self.search_index.clone();
        self.workspace_ui.searching = true;
        self.workspace_ui.search_task = Some(cx.spawn(async move |this, cx| {
            let executor = cx.background_executor().clone();
            executor.timer(Duration::from_millis(180)).await;
            let local = executor.spawn({
                let query = query.clone();
                async move { local_folders(&folder, &query, ignore_accents, show_hidden) }
            }).await;
            if this.update(cx, |app, cx| {
                app.workspace_ui.results.clone_from(&local);
                cx.notify();
            }).is_err() { return; }
            index.schedule(false);
            while !index.status().ready && index.status().building {
                executor.timer(Duration::from_millis(350)).await;
            }
            let failed = index.status().error.is_some();
            let matches = executor.spawn(async move {
                index.find_folders(&query, ignore_accents, &generation, token)
            }).await;
            let _ = this.update(cx, |app, cx| {
                for path in matches {
                    if !app.workspace_ui.results.contains(&path) { app.workspace_ui.results.push(path); }
                }
                app.workspace_ui.results.truncate(12);
                app.workspace_ui.searching = false;
                app.workspace_ui.search_failed = failed;
                cx.notify();
            });
        }));
    }

    pub(crate) fn add_workspace_folder(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if !path.is_dir() {
            self.toast(tr("La carpeta no está disponible.", "The folder is unavailable."), crate::app::ToastTone::Error, cx);
            return;
        }
        let end = self.settings.workspaces.current().rows.len();
        self.insert_workspace_shortcuts([path.clone()], end, false, cx);
        self.clear_workspace_query(window, cx);
        self.open_workspace_location(path, window, cx);
    }

    pub(crate) fn insert_workspace_shortcuts(&mut self, paths: impl IntoIterator<Item = PathBuf>, index: usize, allow_files: bool, cx: &mut Context<Self>) -> bool {
        let paths = shortcut_paths(paths, allow_files);
        let changed = self.settings.workspaces.current_mut().add_shortcuts(paths, index);
        if changed {
            self.workspace_ui.edit = None;
            self.settings.save();
            cx.notify();
        }
        changed
    }

    pub(crate) fn open_workspace_location(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if path.exists() {
            let recent = &mut self.settings.workspaces.current_mut().recent_accesses;
            if !recent.first().is_some_and(|previous| same_path(previous, &path)) {
                recent.retain(|previous| !same_path(previous, &path));
                recent.insert(0, path.clone());
                recent.truncate(100);
                self.settings.save();
                cx.notify();
            }
        }
        if let Some((pane, tab)) = self.panes.iter().enumerate().find_map(|(pane, model)| {
            model.tabs.iter().position(|tab| same_path(&tab.path, &path)).map(|tab| (pane, tab))
        }) {
            self.switch_tab(pane, tab, window, cx);
        } else if path.is_dir() {
            self.open_in_new_tab(path, window, cx);
        } else if path.is_file() {
            self.open_location(path, window, cx);
        } else {
            self.toast(tr("El acceso no está disponible.", "The shortcut is unavailable."), crate::app::ToastTone::Error, cx);
        }
    }

    pub(crate) fn open_workspace_url(&mut self, value: &str, cx: &mut Context<Self>) {
        if let Some(url) = parse_web_shortcut(value) {
            cx.open_url(url.as_str());
        } else {
            self.toast(tr("El enlace no es válido.", "The link is invalid."), crate::app::ToastTone::Error, cx);
        }
    }

    pub(crate) fn set_workspace_page(&mut self, page: usize, cx: &mut Context<Self>) {
        let ui = &mut self.workspace_ui;
        let page_count = self.settings.workspaces.current().page_count(self.settings.workspaces.pin_page_size);
        let page = page.min(page_count - 1);
        if page != ui.page {
            ui.version += 1;
            ui.page_motion = Some((ui.version, if page > ui.page { 1. } else { -1. }));
            ui.page = page;
            cx.notify();
        }
    }

    pub(crate) fn set_workspace_pin_page_size(&mut self, size: usize, cx: &mut Context<Self>) {
        let previous = self.settings.workspaces.pin_page_size;
        self.settings.workspaces.pin_page_size = size;
        self.set_workspace_page(self.workspace_ui.page * previous / size, cx);
        self.settings.save();
        cx.notify();
    }

    pub(crate) fn pin_workspace_folder(&mut self, path: PathBuf, target: Option<usize>, cx: &mut Context<Self>) {
        if !self.pin_workspace_shortcuts([path], target, false, cx) {
            self.toast(tr("Solo se pueden fijar carpetas disponibles.", "Only available folders can be pinned."), crate::app::ToastTone::Error, cx);
        }
    }

    pub(crate) fn pin_workspace_shortcuts(&mut self, paths: impl IntoIterator<Item = PathBuf>, target: Option<usize>, allow_files: bool, cx: &mut Context<Self>) -> bool {
        let space = self.settings.workspaces.current_mut();
        let position = shortcut_paths(paths, allow_files).enumerate().map(|(index, (path, file))| {
            let position = space.pin(path, if index == 0 { target } else { None });
            let pinned = space.pins[position].as_ref().unwrap();
            if file { space.file_pins.insert(pinned.clone()); }
            else { space.file_pins.remove(pinned); }
            position
        }).last();
        let Some(position) = position else { return false; };
        self.set_workspace_page(position / self.settings.workspaces.pin_page_size, cx);
        let ui = &mut self.workspace_ui;
        ui.version += 1;
        ui.pin_motion = Some((ui.version, position));
        self.settings.save();
        cx.notify();
        true
    }

    pub(crate) fn workspace_tab_path(&self, id: u64) -> Option<PathBuf> {
        self.panes.iter().flat_map(|pane| &pane.tabs).find(|tab| tab.id == id).map(|tab| tab.path.clone())
    }

    pub(crate) fn unpin_workspace_folder(&mut self, position: usize, cx: &mut Context<Self>) {
        let space = self.settings.workspaces.current_mut();
        if let Some(pin) = space.pins.get_mut(position) { *pin = None; }
        space.trim_pins();
        self.set_workspace_page(self.workspace_ui.page, cx);
        self.settings.save();
        cx.notify();
    }

    pub(crate) fn remove_workspace_row(&mut self, id: u64, cx: &mut Context<Self>) {
        let space = self.settings.workspaces.current_mut();
        let Some(index) = space.rows.iter().position(|row| row.id == id) else { return; };
        if matches!(space.rows.remove(index).item, Item::Group { .. }) {
            let index = space.rows[..index].iter().filter(|row| row.parent != Some(id)).count();
            let children: Vec<_> = space.rows.extract_if(.., |row| {
                let child = row.parent == Some(id);
                if child {
                    row.parent = None;
                }
                child
            }).collect();
            space.rows.splice(index..index, children);
        }
        self.workspace_ui.edit = None;
        self.workspace_ui.groups.remove(&id);
        self.workspace_ui.row_offsets.remove(&id);
        if self.workspace_ui.row_arrival == Some(id) { self.workspace_ui.row_arrival = None; }
        self.settings.save();
        cx.notify();
    }

    pub(crate) fn preview_workspace_drop(
        &mut self, id: u64, destination: RowDestination, bounds: Bounds<Pixels>, cx: &mut Context<Self>,
    ) {
        let ui = &mut self.workspace_ui;
        ui.drop_bounds = Some(bounds);
        if ui.row_target == Some(destination) && ui.dragged_row == Some(id) { return; }
        let prefs = &self.settings.workspaces;
        let next = prefs.current().projected_row_offsets(id, destination, prefs.access_order, prefs.row_density.stride());
        ui.row_target = Some(destination);
        ui.dragged_row = Some(id);
        ui.animate_projection(next);
        cx.notify();
    }

    pub(crate) fn move_workspace_row(&mut self, id: u64, destination: RowDestination, cx: &mut Context<Self>) {
        let prefs = &mut self.settings.workspaces;
        let order = prefs.access_order;
        let stride = prefs.row_density.stride();
        let before = prefs.current().visible_positions(order, stride);
        if order == AccessOrder::Manual {
            if !prefs.current_mut().move_row(id, destination) {
                self.workspace_ui.clear_row_preview();
                cx.notify();
                return;
            }
        } else {
            // Freeze the displayed order before applying a manual drag.
            let mut edited = prefs.current().clone();
            edited.rows = edited.rows_in_display_order(order);
            if !edited.move_row(id, destination) {
                self.workspace_ui.clear_row_preview();
                cx.notify();
                return;
            }
            for (index, space) in prefs.spaces.iter_mut().enumerate() {
                if index != prefs.selected { space.rows = space.rows_in_display_order(order); }
            }
            *prefs.current_mut() = edited;
            prefs.access_order = AccessOrder::Manual;
        }
        let after = prefs.current().visible_positions(AccessOrder::Manual, stride);
        let now = Instant::now();
        let ui = &mut self.workspace_ui;
        ui.row_arrival = None;
        let offsets = after.into_iter().filter_map(|(id, top)| {
            let offset = before.get(&id)? + ui.row_offset(id) - top;
            (offset.abs() > 0.5).then_some((id, (now, offset)))
        }).collect();
        ui.projected_offsets.clear();
        ui.clear_row_preview();
        ui.row_offsets = offsets;
        self.settings.save();
        cx.notify();
    }

    pub(crate) fn toggle_workspace_group(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(row) = self.settings.workspaces.current_mut().rows.iter_mut().find(|row| row.id == id) else { return; };
        if let Item::Group { collapsed, .. } = &mut row.item {
            *collapsed = !*collapsed;
            self.settings.save();
            cx.notify();
        }
    }

    pub(crate) fn add_workspace_shortcuts_at(
        &mut self, destination: RowDestination, paths: impl IntoIterator<Item = PathBuf>, allow_files: bool, cx: &mut Context<Self>,
    ) {
        let space = self.settings.workspaces.current_mut();
        let Some((index, parent)) = space.destination(destination) else { return; };
        if space.add_shortcuts_in(shortcut_paths(paths, allow_files), index, parent) {
            self.settings.save();
            cx.notify();
        }
    }
}

fn max_panel_width(viewport_width: f32) -> f32 {
    (viewport_width * 0.55).clamp(MIN_PANEL_WIDTH, MAX_PANEL_WIDTH)
}

fn snap_to_icons(requested: f32, icon_only: bool) -> bool {
    // A small gap between thresholds keeps the panel from switching back and forth at the edge.
    requested <= PANEL_SNAP_WIDTH + if icon_only { 12. } else { 0. }
}


fn local_folders(root: &Path, query: &str, ignore_accents: bool, show_hidden: bool) -> Vec<PathBuf> {
    let path = Path::new(query.trim_matches('"'));
    let direct = if path.is_absolute() { path.to_owned() } else { root.join(path) };
    let mut result = Vec::new();
    if direct.is_dir() { result.push(direct); }
    let mut terms = SearchTerms::new(query, ignore_accents);
    for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let path = entry.path();
        if terms.matches(&path) && path.is_dir() && !result.contains(&path)
            && (show_hidden || entry.metadata().is_ok_and(|metadata| !crate::fs_model::is_hidden(&metadata, &entry.file_name().to_string_lossy())))
        {
            result.push(path);
            if result.len() >= 9 { break; }
        }
    }
    result
}

pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

fn shortcut_paths(paths: impl IntoIterator<Item = PathBuf>, allow_files: bool) -> impl Iterator<Item = (PathBuf, bool)> {
    paths.into_iter().filter_map(move |path| {
        let metadata = path.metadata().ok()?;
        if metadata.is_dir() { Some((path, false)) }
        else if allow_files && metadata.is_file() { Some((path, true)) }
        else { None }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_shortcut_can_move_into_an_empty_closed_group() {
        let mut workspace = Workspace::default();
        let group = workspace.add_row(0, Item::Group {
            name: "Links".into(), collapsed: true, style: AccessStyle::default(),
        });
        let link = workspace.add_row(1, Item::Web("https://example.com".into()));
        assert!(workspace.rows[1].item.is_shortcut());
        assert!(workspace.move_row(link, RowDestination::Group(group)));
        assert!(matches!(workspace.rows[0].item, Item::Group { collapsed: true, .. }));
        assert_eq!(workspace.rows[1].parent, Some(group));
    }

    #[test]
    fn workspace_web_shortcuts_require_a_web_url() {
        assert_eq!(web_shortcut_title("https://example.com/test"), "example.com");
        assert!(parse_web_shortcut("http://example.com").is_some());
        for value in ["", "example.com", "file:///C:/Windows", "javascript:alert(1)", "https://"] {
            assert!(parse_web_shortcut(value).is_none(), "{value}");
        }
    }

    #[test]
    fn workspace_visibility_reverses_without_jumping() {
        let start = Instant::now();
        let midpoint = start + SLIDE_TIME / 2;
        let mut motion = VisibilityMotion::new(false);
        assert!(motion.tick(true, start, SLIDE_TIME));
        assert_eq!(motion.progress, 0.);
        assert!(motion.tick(true, midpoint, SLIDE_TIME));
        let progress = motion.progress;
        assert!(progress > 0. && progress < 1.);
        assert!(motion.tick(false, midpoint, SLIDE_TIME));
        assert_eq!(motion.progress, progress);
        assert!(!motion.tick(false, midpoint + SLIDE_TIME, SLIDE_TIME));
        assert_eq!(motion.progress, 0.);
    }

    #[test]
    fn sorting_keeps_dividers_and_manual_order_and_uses_access_names() {
        let mut space = Workspace::default();
        for item in [
            Item::Folder("Zeta".into()),
            Item::Folder("Beta".into()),
            Item::Divider("Proyecto".into()),
            Item::Folder("Delta".into()),
            Item::Folder("Alfa".into()),
        ] {
            space.add_row(space.rows.len(), item);
        }
        space.access_styles.insert("Zeta".into(), AccessStyle {
            name: Some("Apertura".into()),
            icon: Some(WorkspaceIcon::Star),
            color: Some(Accent::Blue),
        });
        space.recent_accesses.push("Beta".into());
        assert_eq!(space.row_order(AccessOrder::Alphabetical, None), [0, 1, 2, 4, 3]);
        assert_eq!(space.row_order(AccessOrder::Recent, None), [1, 0, 2, 3, 4]);
        assert_eq!(space.row_order(AccessOrder::Manual, None), [0, 1, 2, 3, 4]);
    }

    #[test]
    fn workspace_snaps_at_half_width_and_reopens_when_dragged_out() {
        assert!(!snap_to_icons(77., false));
        assert!(snap_to_icons(76., false));
        assert!(snap_to_icons(84., true));
        assert!(!snap_to_icons(89., true));
    }

    #[test]
    fn workspace_keeps_initials_as_default_and_preserves_custom_icons() {
        assert_eq!(Workspace::default().icon, None);
        let old: Workspace = serde_json::from_str(r#"{"icon":null,"use_initials":false}"#).unwrap();
        assert_eq!(old.icon, None);
        let custom: Workspace = serde_json::from_str(r#"{"icon":"home","use_initials":false}"#).unwrap();
        assert_eq!(custom.icon, Some(WorkspaceIcon::Home));
    }

    #[test]
    fn workspace_drag_preview_displaces_rows_without_changing_saved_order() {
        let mut space = Workspace::default();
        let first = space.add_row(0, Item::Folder("First".into()));
        let group = space.add_row(1, Item::Group { name: "Projects".into(), collapsed: false, style: AccessStyle::default() });
        let last = space.add_row(2, Item::Folder("Last".into()));
        let child = space.add_row(3, Item::Folder("Inside".into()));
        assert!(space.move_row(child, RowDestination::Group(group)));
        let preview = space.projected_row_offsets(first, RowDestination::After(last), AccessOrder::Manual, 34.);
        assert_eq!(preview, HashMap::from([(first, 102.), (group, -34.), (child, -34.), (last, -34.)]));
        assert_eq!(space.rows[0].id, first);
        assert!(space.projected_row_offsets(first, RowDestination::Group(group), AccessOrder::Manual, 34.).is_empty());
    }

    #[test]
    fn workspace_shortcuts_move_into_between_and_out_of_groups() {
        let mut space = Workspace::default();
        let folder = space.add_row(0, Item::Folder("Documents".into()));
        let first = space.add_row(1, Item::Group { name: "Projects".into(), collapsed: true, style: AccessStyle::default() });
        let second = space.add_row(2, Item::Group { name: "Archive".into(), collapsed: false, style: AccessStyle::default() });
        assert!(space.move_row(folder, RowDestination::Group(first)));
        assert_eq!(space.rows.iter().find(|row| row.id == folder).unwrap().parent, Some(first));
        assert!(!space.move_row(second, RowDestination::Before(folder)));
        assert!(space.move_row(folder, RowDestination::Group(second)));
        assert_eq!(space.rows.last().unwrap().parent, Some(second));
        let style = AccessTarget::Row(second).style_mut(&mut space).unwrap();
        style.icon = Some(WorkspaceIcon::Star);
        style.color = Some(Accent::Red);
        let mut space: Workspace = serde_json::from_str(&serde_json::to_string(&space).unwrap()).unwrap();
        let style = AccessTarget::Row(second).style(&space).unwrap();
        assert_eq!(style.icon, Some(WorkspaceIcon::Star));
        assert_eq!(style.color, Some(Accent::Red));
        assert_eq!(space.row_order(AccessOrder::Alphabetical, Some(second)), [2]);
        assert!(space.move_row(folder, RowDestination::Before(first)));
        assert_eq!(space.rows[0].id, folder);
        assert_eq!(space.rows[0].parent, None);
    }

    #[test]
    fn ninth_pin_creates_a_new_page_without_displacing_an_existing_tile() {
        const PAGE_SIZE: usize = 8;
        let mut space = Workspace::default();
        for index in 0..PAGE_SIZE {
            assert_eq!(space.pin(PathBuf::from(format!("folder{index}")), None), index);
        }
        assert_eq!(space.page_count(PAGE_SIZE), 1);

        assert_eq!(space.pin("extra".into(), Some(0)), PAGE_SIZE);
        assert_eq!(space.page_count(PAGE_SIZE), 2);
        assert_eq!(space.page_count(6), 2);
        assert_eq!(space.page_count(4), 3);
        assert_eq!(space.page_count(2), 5);
        assert_eq!(space.pins[0], Some(PathBuf::from("folder0")));

        assert_eq!(space.pin("EXTRA".into(), Some(1)), 1);
        assert_eq!(space.pins[1], Some(PathBuf::from("extra")));
        assert_eq!(space.pins[PAGE_SIZE], Some(PathBuf::from("folder1")));
        space.pins[PAGE_SIZE] = None;
        space.trim_pins();
        assert_eq!(space.page_count(PAGE_SIZE), 1);
    }

    #[test]
    fn workspace_data_round_trips_with_separate_pins_and_dividers() {
        let mut prefs: Preferences = serde_json::from_str("{}").unwrap();
        assert_eq!(prefs.pin_page_size, 6);
        assert_eq!(prefs.width, None);
        assert!(!prefs.icon_only_panel);
        assert!(prefs.show_pin_names);
        assert!(prefs.show_titlebar_button);
        assert!(prefs.show_pin_close);
        assert!(!prefs.pin_close_left);
        assert!(!prefs.pin_close_always);
        assert!(!prefs.use_system_icons);
        assert_eq!(prefs.divider_alignment, DividerAlignment::Left);
        prefs.pin_page_size = 4;
        prefs.width = Some(320.);
        prefs.icon_only_panel = true;
        prefs.current_mut().add_row(0, Item::Divider("Clientes".into()));
        assert!(prefs.current_mut().add_shortcuts([("Descargas".into(), false), ("DESCARGAS".into(), false), ("Notas.txt".into(), true)], 1));
        assert_eq!(prefs.current().rows.len(), 3);
        prefs.current_mut().pin("Documentos".into(), None);
        prefs.current_mut().pin("Notas.txt".into(), None);
        prefs.current_mut().file_pins.insert("Notas.txt".into());
        prefs.spaces.push(Workspace { name: "Personal".into(), ..Workspace::default() });
        prefs.selected = 1;
        assert_eq!(prefs.startup_index(), 1);
        prefs.startup_space = Some(0);
        prefs.show_pin_names = false;
        prefs.show_titlebar_button = false;
        prefs.use_system_icons = true;
        prefs.divider_alignment = DividerAlignment::Center;
        prefs.current_mut().color = Some(Accent::Blue);
        prefs.current_mut().icon = Some(WorkspaceIcon::Home);

        let mut loaded: Preferences = serde_json::from_str(&serde_json::to_string(&prefs).unwrap()).unwrap();
        loaded.validate();
        assert_eq!(loaded.pin_page_size, 4);
        assert_eq!(loaded.width, Some(320.));
        assert!(loaded.icon_only_panel);
        assert_eq!(loaded.startup_index(), 0);
        assert!(!loaded.show_pin_names);
        assert!(!loaded.show_titlebar_button);
        assert!(loaded.use_system_icons);
        assert_eq!(loaded.divider_alignment, DividerAlignment::Center);
        assert_eq!(loaded.current().color, Some(Accent::Blue));
        assert_eq!(loaded.current().icon, Some(WorkspaceIcon::Home));
        assert_eq!(loaded.current().name, "Personal");
        assert!(loaded.current().pins.is_empty());
        assert!(matches!(&loaded.spaces[0].rows[0].item, Item::Divider(name) if name == "Clientes"));
        assert!(matches!(&loaded.spaces[0].rows[1].item, Item::Folder(path) if path == Path::new("Descargas")));
        assert!(matches!(&loaded.spaces[0].rows[2].item, Item::File(path) if path == Path::new("Notas.txt")));
        assert_eq!(loaded.spaces[0].pins[0], Some(PathBuf::from("Documentos")));
        assert!(loaded.spaces[0].file_pins.contains(Path::new("Notas.txt")));

        loaded.width = Some(132.);
        loaded.validate();
        assert_eq!(loaded.width, Some(MIN_PANEL_WIDTH));
    }
}
