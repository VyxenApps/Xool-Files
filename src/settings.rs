use crate::i18n::tr;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::shortcuts;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StartupMode { NewTab, LastSession, Folders }

pub(crate) const MAX_STARTUP_FOLDERS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    Dark,
    Light,
    System,
    MidnightSlate,
    AyuMirage,
    Cherry,
}

impl Default for ThemeMode {
    fn default() -> Self {
        Self::Dark
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Accent {
    Amber,
    #[serde(alias = "coral")]
    Red,
    #[serde(alias = "lime", alias = "cyan")]
    Green,
    Yellow,
    Purple,
    Gray,
    Blue,
    Rose,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TabGradient {
    pub intensity: usize,
    pub border: usize,
}

impl Default for TabGradient {
    fn default() -> Self {
        Self {
            intensity: 0,
            border: 1,
        }
    }
}

impl Default for Accent {
    fn default() -> Self {
        Self::Amber
    }
}

impl Accent {
    pub fn rgb(self) -> u32 {
        match self {
            Self::Amber => 0xf2a65a,
            Self::Red => 0xf16c70,
            Self::Green => 0x7fc68e,
            Self::Yellow => 0xe8c35a,
            Self::Purple => 0xac91f5,
            Self::Gray => 0xa1aaba,
            Self::Blue => 0x6ea8fe,
            Self::Rose => 0xf28db2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewMode {
    Grid,
    List,
    Tree,
    Details,
}

impl ViewMode {
    pub fn zoom_percent(self, scale: f32) -> u32 {
        match self {
            Self::Grid => (25.0 + (scale.clamp(1.0, 2.0) - 1.0) * 75.0).round() as u32,
            Self::List => 15,
            Self::Tree => 10,
            Self::Details => 0,
        }
    }

    pub fn from_zoom(percent: u32) -> (Self, f32) {
        match percent {
            0 => (Self::Details, 1.0),
            1..=10 => (Self::Tree, 1.0),
            11..=24 => (Self::List, 1.0),
            _ => (Self::Grid, 1.0 + (percent.min(100) - 25) as f32 / 75.0),
        }
    }

    pub fn stepped_zoom(self, scale: f32, increase: bool) -> u32 {
        match (self, increase) {
            (Self::Details, false) => 0,
            (Self::Details, true) | (Self::List, false) => 10,
            (Self::Tree, false) => 0,
            (Self::Tree, true) => 15,
            (Self::List, true) => 25,
            (Self::Grid, true) => (self.zoom_percent(scale) + 5).min(100),
            (Self::Grid, false) => match self.zoom_percent(scale) {
                25 => 15,
                percent => percent.saturating_sub(5).max(25),
            },
        }
    }
}

impl Default for ViewMode {
    fn default() -> Self {
        Self::Grid
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Favorite {
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub color: usize,
}

// Keep stored color IDs stable when changing the order of the palette.
pub fn favorite_colors() -> [(usize, &'static str, u32); 8] {
    [
    (4, tr("Azul", "Blue"), 0x6ea8fe),
    (1, tr("Rojo", "Red"), 0xff7a66),
    (5, tr("Rosado", "Pink"), 0xf28db2),
    (6, tr("Amarillo", "Yellow"), 0xecc45b),
    (0, tr("Naranja", "Orange"), 0xffb454),
    (2, tr("Verde", "Green"), 0xb8e986),
    (7, tr("Morado", "Purple"), 0xa991ee),
    (8, tr("Gris", "Gray"), 0xa0aab9),
    ]
}

pub fn favorite_color(id: usize) -> u32 {
    if id == 3 { return 0x5bd6c8; } // Turquesa de configuraciones anteriores.
    let palette = favorite_colors();
    palette.into_iter().find(|&(color, _, _)| color == id)
        .map_or(palette[0].2, |(_, _, rgb)| rgb)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidebarGroup { Favorites, Quick, Drives, Recent, Custom(u64) }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CustomSidebarGroup {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub folders: Vec<PathBuf>,
    #[serde(default)]
    pub collapsed: bool,
}

impl SidebarGroup {
    pub const ALL: [Self; 4] = [Self::Favorites, Self::Quick, Self::Drives, Self::Recent];

    pub fn heading_id(self) -> gpui::ElementId {
        match self {
            Self::Favorites => "sidebar-favorites".into(), Self::Quick => "sidebar-quick".into(),
            Self::Drives => "sidebar-drives".into(), Self::Recent => "sidebar-recent".into(),
            Self::Custom(id) => ("sidebar-custom", id).into(),
        }
    }

    pub fn heading(self) -> &'static str {
        match self {
            Self::Favorites => tr("FAVORITOS", "FAVORITES"), Self::Quick => tr("ACCESO RÁPIDO", "QUICK ACCESS"),
            Self::Drives => tr("UNIDADES", "DRIVES"), Self::Recent => tr("ARCHIVOS RECIENTES", "RECENT FILES"),
            Self::Custom(_) => tr("GRUPO PERSONALIZADO", "CUSTOM GROUP"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Favorites => tr("Favoritos", "Favorites"),
            Self::Quick => tr("Acceso rápido", "Quick access"),
            Self::Drives => tr("Unidades", "Drives"),
            Self::Recent => tr("Archivos recientes", "Recent files"),
            Self::Custom(_) => tr("Grupo personalizado", "Custom group"),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct HoverFields {
    pub name: bool,
    pub kind: bool,
    pub dimensions: bool,
    pub created: bool,
    pub size: bool,
    pub modified: bool,
    pub accessed: bool,
}

impl HoverFields {
    pub fn any(self) -> bool {
        self.name || self.kind || self.dimensions || self.created || self.size || self.modified || self.accessed
    }
}

impl Default for HoverFields {
    fn default() -> Self {
        Self { name: true, kind: true, dimensions: true, created: true, size: true, modified: false, accessed: false }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioVisualization {
    #[default]
    Bars,
    Centered,
    Wave,
}

impl AudioVisualization {
    pub const ALL: [Self; 3] = [Self::Bars, Self::Centered, Self::Wave];

    pub fn label(self) -> &'static str {
        match self {
            Self::Bars => tr("Barras", "Bars"),
            Self::Centered => tr("Barras centradas", "Centered bars"),
            Self::Wave => tr("Onda", "Wave"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextMenuStyle {
    Minimal,
    #[default]
    #[serde(alias = "full")]
    Standard,
}

impl ContextMenuStyle {
    pub const ALL: [Self; 2] = [Self::Minimal, Self::Standard];

    pub fn label(self) -> &'static str {
        match self {
            Self::Minimal => tr("Minimalista", "Minimal"),
            Self::Standard => tr("Predeterminado", "Default"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetailColumn {
    Modified, Kind, Size, Created, Accessed, Folders, Status, Priority,
}

pub(crate) const DETAIL_COLUMNS: usize = 8;

impl DetailColumn {
    pub const ALL: [Self; DETAIL_COLUMNS] = [Self::Modified, Self::Kind, Self::Size,
        Self::Created, Self::Accessed, Self::Folders, Self::Status, Self::Priority];
    pub const MIN_WIDTHS: [f32; DETAIL_COLUMNS] = [90.; DETAIL_COLUMNS];
    pub const DEFAULT_WIDTHS: [f32; DETAIL_COLUMNS] = [138., 142., 90., 150., 150., 130., 106., 92.];

    pub fn label(self) -> &'static str {
        match self {
            Self::Modified => tr("Modificado", "Modified"),
            Self::Kind => tr("Tipo", "Type"),
            Self::Size => tr("Tamaño", "Size"),
            Self::Created => tr("Creación", "Created"),
            Self::Accessed => tr("Último acceso", "Last accessed"),
            Self::Folders => tr("Total de carpetas", "Total folders"),
            Self::Status => tr("Estado", "Status"),
            Self::Priority => tr("Prioridad", "Priority"),
        }
    }

    pub fn index(self) -> usize { self as usize }

    pub fn sort_key(self) -> Option<crate::fs_model::SortKey> {
        match self {
            Self::Modified => Some(crate::fs_model::SortKey::Modified),
            Self::Kind => Some(crate::fs_model::SortKey::Kind),
            Self::Size => Some(crate::fs_model::SortKey::Size),
            _ => None,
        }
    }
}

const DEFAULT_DETAIL_VISIBILITY: [bool; DETAIL_COLUMNS] = [true, true, true, false, false, false, true, true];

fn load_detail_visibility<'de, D: serde::Deserializer<'de>>(d: D) -> Result<[bool; DETAIL_COLUMNS], D::Error> {
    let mut visible = DEFAULT_DETAIL_VISIBILITY;
    for (slot, value) in visible.iter_mut().zip(Vec::<bool>::deserialize(d)?) {
        *slot = value;
    }
    Ok(visible)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DetailLabel {
    Idea, InProgress, Scheduled, Published, Canceled,
    Low, Medium, High, Urgent,
}

impl DetailLabel {
    pub fn choices(column: DetailColumn) -> Option<&'static [Self]> {
        match column {
            DetailColumn::Status => Some(&[Self::Idea, Self::InProgress, Self::Scheduled, Self::Published, Self::Canceled]),
            DetailColumn::Priority => Some(&[Self::Low, Self::Medium, Self::High, Self::Urgent]),
            _ => None,
        }
    }

    pub fn appearance(self) -> (&'static str, u32) {
        const ORANGE: u32 = 0x935125;
        const RED: u32 = 0x974b49;
        const GREEN: u32 = 0x367256;
        const PURPLE: u32 = 0x76538a;
        match self {
            Self::Idea => (tr("Idea", "Idea"), 0x895f1d),
            Self::InProgress => (tr("En progreso", "In progress"), ORANGE),
            Self::Scheduled => (tr("Programado", "Scheduled"), RED),
            Self::Published => (tr("Publicado", "Published"), GREEN),
            Self::Canceled => (tr("Cancelado", "Canceled"), PURPLE),
            Self::Low => (tr("Baja", "Low"), PURPLE),
            Self::Medium => (tr("Media", "Medium"), GREEN),
            Self::High => (tr("Alta", "High"), ORANGE),
            Self::Urgent => (tr("Urgente", "Urgent"), RED),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct FileLabels {
    pub status: Option<DetailLabel>,
    pub priority: Option<DetailLabel>,
}

impl FileLabels {
    pub fn get(self, column: DetailColumn) -> Option<DetailLabel> {
        let value = match column {
            DetailColumn::Status => self.status,
            DetailColumn::Priority => self.priority,
            _ => None,
        };
        value.filter(|label| DetailLabel::choices(column).is_some_and(|choices| choices.contains(label)))
    }
}

pub(crate) fn load_detail_widths<'de, D: serde::Deserializer<'de>>(d: D) -> Result<[f32; DETAIL_COLUMNS], D::Error> {
    let values = Vec::<f32>::deserialize(d)?;
    let mut widths = DetailColumn::DEFAULT_WIDTHS;
    for (index, (out, width)) in widths.iter_mut().zip(values).enumerate() {
        if width.is_finite() { *out = width.clamp(DetailColumn::MIN_WIDTHS[index], 1000.); }
    }
    Ok(widths)
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) struct WindowPlacement {
    pub normal_rect: [i32; 4],
    pub maximized: bool,
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct LegacyCompactMode {
    #[serde(default)]
    flush_tabs: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub(crate) workspaces: crate::workspaces::Preferences,
    pub(crate) language: crate::i18n::Language,
    #[serde(skip)]
    pub(crate) temporary_window: bool,
    pub(crate) compact_paths: bool,
    pub(crate) path_breadcrumbs: bool,
    pub(crate) window_placement: Option<WindowPlacement>,
    pub wheel_tabs: bool,
    pub active_tab_gradient: bool,
    pub tab_gradient: TabGradient,
    pub hide_tab_icons: bool,
    pub show_tab_close: bool,
    pub tab_close_left: bool,
    pub tab_close_always: bool,
    pub use_system_tab_icons: bool,
    pub background_on_close: bool,
    pub show_tray_icon: bool,
    pub closed_tab_limit: u8,
    pub closed_tabs: Vec<PathBuf>,
    pub sidebar_groups: Vec<SidebarGroup>,
    pub(crate) custom_sidebar_groups: Vec<CustomSidebarGroup>,
    pub(crate) next_sidebar_group_id: u64,
    pub sidebar_icons_only: bool,
    pub sidebar_row_size: u8,
    pub drive_name_style: u8,
    pub hide_drive_usage: bool,
    pub sidebar_drive_tree: bool,
    pub sidebar_recent_limit: usize,
    pub preview_autoplay: bool,
    pub video_autoplay: bool,
    pub audio_visualization: AudioVisualization,
    pub(crate) action_sounds_enabled: bool,
    #[serde(deserialize_with = "crate::sounds::deserialize_assignment")]
    pub(crate) select_file_sound: Option<crate::sounds::ActionSound>,
    #[serde(alias = "open_folder_sound", deserialize_with = "crate::sounds::deserialize_assignment")]
    pub(crate) tab_sound: Option<crate::sounds::ActionSound>,
    pub remember_folder_views: bool,
    pub folder_views: Vec<FolderView>,
    pub theme: ThemeMode,
    pub translucent_background: bool,
    pub background_image_path: Option<PathBuf>,
    pub background_tint: u8,
    pub background_blur: u8,
    pub accent: Accent,
    pub view: ViewMode,
    pub icon_scale: f32,
    #[serde(default)]
    normalized_zoom: bool,
    /// Último criterio de orden usado. Se aplica a las pestañas nuevas y se
    /// conserva al cerrar y volver a abrir Xool Files.
    pub sort: crate::fs_model::SortKey,
    pub sort_descending: bool,
    pub show_hidden: bool,
    pub hover_details: bool,
    pub hover_fields: HoverFields,
    #[serde(alias = "quick_search_hotkey", deserialize_with = "shortcuts::deserialize_quick_search_shortcut")]
    pub quick_search_shortcut: Option<String>,
    pub search_ignore_accents: bool,
    pub search_live_index: bool,
    pub show_preview: bool,
    pub split: bool,
    /// Proporción del panel izquierdo cuando la vista está dividida.
    pub split_ratio: f32,
    /// Si la división es vertical (paneles arriba/abajo) en lugar de
    /// horizontal (izquierda/derecha).
    pub split_vertical: bool,
    pub confirm_delete: bool,
    pub single_click_files: bool,
    pub confirm_open_count: usize,
    pub confirm_open_multiple: bool,
    pub undo_file_operations: bool,
    pub edit_text_preview: bool,
    pub menu_tools_order: Vec<String>,
    pub hidden_menu_tools: Vec<String>,
    pub restore_session: bool,
    pub(crate) startup_mode: Option<StartupMode>,
    pub(crate) startup_folders: Vec<PathBuf>,
    pub shell_menu_actions: bool,
    pub context_menu_style: ContextMenuStyle,
    pub command_bar: bool,
    pub preview_left: bool,
    pub focus_middle_tab: bool,
    pub split_below: bool,
    #[serde(alias = "new_tab_address")]
    pub new_tab_home: bool,
    pub settings_nav_width: f32,
    pub wheel_scroll_ms: u16,
    pub grid_name_lines: u8,
    pub show_extensions: bool,
    pub flush_tabs_when_maximized: bool,
    #[serde(rename = "compact_mode", skip_serializing)]
    legacy_compact_mode: Option<LegacyCompactMode>,
    pub compact_density: bool,
    pub compact_row_height: u8,
    pub compact_grid_tight: bool,
    pub compact_grid_show_size: bool,
    #[serde(deserialize_with = "load_detail_widths")]
    pub detail_widths: [f32; DETAIL_COLUMNS],
    pub detail_column_order: Vec<DetailColumn>,
    #[serde(deserialize_with = "load_detail_visibility")]
    pub detail_column_visible: [bool; DETAIL_COLUMNS],
    pub(crate) file_labels: BTreeMap<PathBuf, FileLabels>,
    pub sidebar_visible: bool,
    pub sidebar_width: f32,
    pub sidebar_right: bool,
    pub status_bar_above: bool,
    pub preview_width: f32,
    pub audio_volume: f32,
    pub favorites: Vec<Favorite>,
    pub favorites_collapsed: bool,
    pub reorder_sidebar_favorites: bool,
    pub quick_collapsed: bool,
    pub recent_collapsed: bool,
    pub drives_collapsed: bool,
    pub last_left: Vec<PathBuf>,
    pub last_right: Vec<PathBuf>,
    pub last_split: bool,
    pub pane_session: Option<crate::pane_layout::PaneSession>,
    pub active_left: usize,
    pub active_right: usize,
    pub recent_paths: Vec<PathBuf>,
    pub(crate) recent_file_paths: Vec<PathBuf>,
    /// Solo guarda los atajos que el usuario cambia. El resto se toma del
    /// catálogo de `shortcuts.rs`, así los valores predeterminados pueden
    /// evolucionar sin dejar combinaciones antiguas pegadas.
    pub shortcuts: BTreeMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            workspaces: Default::default(),
            language: crate::i18n::Language::default(),
            window_placement: None,
            preview_autoplay: true,
            video_autoplay: true,
            audio_visualization: AudioVisualization::default(),
            action_sounds_enabled: false,
            select_file_sound: Some(crate::sounds::ActionSound::Plop),
            tab_sound: Some(crate::sounds::ActionSound::Pulso),
            remember_folder_views: true,
            folder_views: Vec::new(),
            theme: ThemeMode::Dark,
            translucent_background: false,
            background_image_path: None,
            background_tint: 60,
            background_blur: 32,
            accent: Accent::Amber,
            view: ViewMode::Grid,
            icon_scale: 1.0,
            normalized_zoom: true,
            sort: crate::fs_model::SortKey::Name,
            sort_descending: false,
            show_hidden: false,
            hover_details: false,
            hover_fields: HoverFields::default(),
            quick_search_shortcut: None,
            search_ignore_accents: false,
            search_live_index: false,
            show_preview: true,
            split: false,
            split_ratio: 0.5,
            split_vertical: false,
            confirm_delete: true,
            single_click_files: false,
            confirm_open_count: 5,
            confirm_open_multiple: true,
            undo_file_operations: true,
            edit_text_preview: false,
            menu_tools_order: Vec::new(),
            hidden_menu_tools: Vec::new(),
            restore_session: true,
            startup_mode: None,
            startup_folders: Vec::new(),
            temporary_window: false,
            compact_paths: false,
            path_breadcrumbs: true,
            shell_menu_actions: true,
            context_menu_style: ContextMenuStyle::default(),
            command_bar: false,
            preview_left: false,
            focus_middle_tab: true,
            split_below: false,
            new_tab_home: false,
            settings_nav_width: 210.0,
            wheel_scroll_ms: 0,
            grid_name_lines: 2,
            show_extensions: true,
            flush_tabs_when_maximized: false,
            legacy_compact_mode: None,
            compact_density: false,
            compact_row_height: 25,
            compact_grid_tight: false,
            compact_grid_show_size: true,
            detail_widths: DetailColumn::DEFAULT_WIDTHS,
            detail_column_order: DetailColumn::ALL.to_vec(),
            detail_column_visible: DEFAULT_DETAIL_VISIBILITY,
            file_labels: BTreeMap::new(),
            sidebar_visible: true,
            wheel_tabs: false,
            active_tab_gradient: false,
            tab_gradient: TabGradient::default(),
            hide_tab_icons: false,
            show_tab_close: true,
            tab_close_left: false,
            tab_close_always: false,
            use_system_tab_icons: false,
            background_on_close: false,
            show_tray_icon: false,
            closed_tab_limit: 5,
            closed_tabs: Vec::new(),
            sidebar_groups: vec![SidebarGroup::Favorites, SidebarGroup::Quick, SidebarGroup::Drives],
            custom_sidebar_groups: Vec::new(),
            next_sidebar_group_id: 1,
            sidebar_icons_only: false,
            sidebar_row_size: 1,
            drive_name_style: 0,
            hide_drive_usage: false,
            sidebar_drive_tree: false,
            sidebar_recent_limit: 10,
            sidebar_width: 238.0,
            sidebar_right: false,
            status_bar_above: false,
            preview_width: 360.0,
            audio_volume: 0.8,
            favorites: Vec::new(),
            favorites_collapsed: false,
            reorder_sidebar_favorites: false,
            quick_collapsed: false,
            recent_collapsed: false,
            drives_collapsed: false,
            last_left: Vec::new(),
            last_right: Vec::new(),
            last_split: false,
            pane_session: None,
            active_left: 0,
            active_right: 0,
            recent_paths: Vec::new(),
            recent_file_paths: Vec::new(),
            shortcuts: BTreeMap::new(),
        }
    }
}

impl Settings {
    pub(crate) const BACKGROUND_TINT_LEVELS: [u8; 4] = [45, 60, 75, 90];
    pub(crate) const BACKGROUND_BLUR_LEVELS: [u8; 4] = [0, 16, 32, 56];

    pub(crate) fn set_file_label(&mut self, path: &Path, column: DetailColumn, value: Option<DetailLabel>) {
        let Some(choices) = DetailLabel::choices(column) else { return; };
        if value.is_some_and(|label| !choices.contains(&label)) { return; }
        let labels = self.file_labels.entry(path.to_path_buf()).or_default();
        if column == DetailColumn::Status { labels.status = value; } else { labels.priority = value; }
        if labels.status.is_none() && labels.priority.is_none() { self.file_labels.remove(path); }
    }

    pub(crate) fn move_file_label(&mut self, from: &Path, to: &Path, column: DetailColumn) -> bool {
        if crate::workspaces::same_path(from, to) { return false; }
        let Some(label) = self.file_labels.get(from).and_then(|labels| labels.get(column)) else { return false; };
        let destination = self.file_labels.get(to).and_then(|labels| labels.get(column));
        self.set_file_label(from, column, destination);
        self.set_file_label(to, column, Some(label));
        true
    }

    pub(crate) fn custom_sidebar_group(&self, id: u64) -> Option<&CustomSidebarGroup> {
        self.custom_sidebar_groups.iter().find(|group| group.id == id)
    }

    pub(crate) fn sidebar_group_name(&self, group: SidebarGroup) -> &str {
        if let SidebarGroup::Custom(id) = group
            && let Some(custom) = self.custom_sidebar_group(id) {
            return &custom.name;
        }
        group.label()
    }

    pub fn sidebar_group_collapsed(&self, group: SidebarGroup) -> bool {
        match group {
            SidebarGroup::Favorites => self.favorites_collapsed,
            SidebarGroup::Quick => self.quick_collapsed,
            SidebarGroup::Drives => self.drives_collapsed,
            SidebarGroup::Recent => self.recent_collapsed,
            SidebarGroup::Custom(id) => self.custom_sidebar_group(id).is_some_and(|group| group.collapsed),
        }
    }

    pub fn sidebar_display_width(&self) -> f32 {
        if self.sidebar_icons_only { 50.0 } else { self.sidebar_width.clamp(184.0, 420.0) }
    }

    pub fn sidebar_row_height(&self) -> f32 {
        match self.sidebar_row_size { 0 => 25.0, 2 => 37.0, _ => 31.0 }
    }

    pub fn load() -> Self {
        let Some(path) = settings_path() else {
            return Self::default();
        };
        let mut settings: Self = fs::read(path).ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        settings.validate();
        settings
    }

    pub fn read_import(path: &std::path::Path) -> Result<Self, String> {
        let text = fs::read_to_string(path).map_err(|error| error.to_string())?;
        Self::parse_import(text.trim_start_matches('\u{feff}'))
    }

    fn parse_import(text: &str) -> Result<Self, String> {
        let json: serde_json::Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
        let keys = serde_json::to_value(Self::default()).map_err(|error| error.to_string())?;
        let recognized = json.as_object().is_some_and(|fields| {
            fields.keys().any(|key| keys.get(key).is_some())
        });
        if !recognized {
            return Err(tr("El archivo no contiene una configuración de Xool Files.", "The file does not contain Xool Files settings.").into());
        }
        let mut settings: Self = serde_json::from_value(json).map_err(|error| error.to_string())?;
        settings.validate();
        Ok(settings)
    }

    pub fn export_to(&self, path: &std::path::Path) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        atomic_write(path, &json).map_err(|error| error.to_string())
    }

    pub fn replace_with_backup(&self) -> Result<Option<PathBuf>, String> {
        let path = settings_path().ok_or_else(|| tr("La ruta de configuración no está disponible.", "The settings path is unavailable.").to_string())?;
        let parent = path.parent().ok_or_else(|| tr("La carpeta de configuración no está disponible.", "The settings folder is unavailable.").to_string())?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let backup = match fs::read(&path) {
            Ok(previous) => {
                let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S-%f");
                let backup = parent.join(format!("settings.backup-{stamp}.json"));
                atomic_write(&backup, &previous).map_err(|error| error.to_string())?;
                Some(backup)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
        };
        self.export_to(&path)?;
        Ok(backup)
    }

    pub fn save(&self) {
        if self.temporary_window { return; }
        let Some(path) = settings_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = atomic_write(&path, text.as_bytes());
        }
    }

    /// Combinación efectiva de una acción: la personalizada, la predeterminada
    /// o una cadena vacía si el usuario dejó la acción sin atajo.
    pub fn shortcut(&self, id: &str) -> String {
        match self.shortcuts.get(id) {
            Some(keys) if shortcuts::is_unbound(keys) => String::new(),
            Some(keys) => keys.clone(),
            None => shortcuts::default_for(id).map(str::to_string).unwrap_or_default(),
        }
    }

    pub fn set_shortcut(&mut self, id: &str, keys: String) {
        self.shortcuts.insert(id.to_string(), keys);
    }

    /// Deja la acción explícitamente sin atajo.
    pub fn unbind_shortcut(&mut self, id: &str) {
        self.shortcuts
            .insert(id.to_string(), shortcuts::UNBOUND.to_string());
    }

    pub fn reset_shortcut(&mut self, id: &str) {
        self.shortcuts.remove(id);
    }
}

/// Carpeta de caché de miniaturas. En pruebas usa el mismo directorio de
/// datos que la configuración para poder limpiarla sin tocar al usuario.
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(root) = data_dir_override() {
        return Some(root.join("cache"));
    }
    std::env::var_os("LOCALAPPDATA")
        .map(|base| PathBuf::from(base).join("XoolFiles").join("cache"))
}

pub fn settings_path() -> Option<PathBuf> {
    if let Some(root) = data_dir_override() {
        return Some(root.join("settings.json"));
    }
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join(".config")))?;
    Some(base.join("XoolFiles").join("settings.json"))
}

pub(crate) fn data_dir_override() -> Option<PathBuf> {
    std::env::var_os("XOOLFILES_DATA_DIR")
        .map(PathBuf::from)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FolderView {
    pub path: PathBuf,
    pub view: ViewMode,
    pub scale: f32,
    pub compact: bool,
    pub sort: crate::fs_model::SortKey,
    pub descending: bool,
}
impl Settings {
    pub(crate) fn startup_mode(&self) -> StartupMode {
        self.startup_mode.unwrap_or(if self.restore_session { StartupMode::LastSession } else { StartupMode::NewTab })
    }

    pub fn validate(&mut self) {
        let gradient = &mut self.tab_gradient;
        gradient.intensity = gradient.intensity.min(2);
        gradient.border = gradient.border.min(2);
        self.workspaces.validate();
        if self.background_tint == 30 { self.background_tint = 45; }
        if !Self::BACKGROUND_TINT_LEVELS.contains(&self.background_tint) { self.background_tint = 60; }
        if !Self::BACKGROUND_BLUR_LEVELS.contains(&self.background_blur) { self.background_blur = 32; }
        self.background_image_path = self.background_image_path.take().filter(|path| path.is_absolute());
        if let Some(legacy) = self.legacy_compact_mode.take() {
            self.flush_tabs_when_maximized = legacy.flush_tabs;
        }
        self.startup_folders.retain(|path| path.is_absolute());
        self.startup_folders.truncate(MAX_STARTUP_FOLDERS);
        if ![5, 10, 15].contains(&self.closed_tab_limit) { self.closed_tab_limit = 5; }
        self.closed_tabs.truncate(15);
        fn bounded(value: f32, fallback: f32, min: f32, max: f32) -> f32 {
            if value.is_finite() {
                value.clamp(min, max)
            } else {
                fallback
            }
        }
        if !self.normalized_zoom {
            if self.view == ViewMode::List && !self.compact_density {
                self.view = ViewMode::Details;
            }
            for view in &mut self.folder_views {
                if view.view == ViewMode::List && !view.compact {
                    view.view = ViewMode::Details;
                }
            }
            self.normalized_zoom = true;
        }
        self.icon_scale = bounded(self.icon_scale, 1., 1.0, 2.0);
        let mut seen_columns = HashSet::new();
        self.detail_column_order.retain(|column| DetailColumn::ALL.contains(column) && seen_columns.insert(*column));
        if !seen_columns.contains(&DetailColumn::Priority) {
            self.detail_column_visible[DetailColumn::Priority.index()] = true;
        }
        self.detail_column_order.extend(DetailColumn::ALL.into_iter().filter(|column| seen_columns.insert(*column)));
        self.sidebar_width = bounded(self.sidebar_width, 238., 180., 420.);
        self.preview_width = bounded(self.preview_width, 360., 240., 720.);
        self.split_ratio = bounded(self.split_ratio, 0.5, 0.2, 0.8);
        self.audio_volume = bounded(self.audio_volume, 0.8, 0., 1.);
        self.folder_views.retain(|v| v.scale.is_finite());
        for v in &mut self.folder_views {
            v.scale = v.scale.clamp(1.0, 2.0);
        }
        if self.folder_views.len() > 500 {
            self.folder_views.drain(..self.folder_views.len() - 500);
        }
        self.last_left.truncate(32);
        self.last_right.truncate(32);
        self.recent_paths.truncate(50);
        self.recent_file_paths.retain(|path| path.is_absolute());
        self.recent_file_paths.truncate(50);
        if ![5, 10, 20, 50].contains(&self.sidebar_recent_limit) { self.sidebar_recent_limit = 10; }
        let mut custom_ids = HashSet::new();
        self.custom_sidebar_groups.retain_mut(|group| {
            group.name = group.name.trim().to_owned();
            group.folders.retain(|path| path.is_absolute());
            !group.name.is_empty() && group.id < u64::MAX && custom_ids.insert(group.id)
        });
        let mut groups = HashSet::new();
        self.sidebar_groups.retain(|group| {
            groups.insert(*group) && match group {
                SidebarGroup::Custom(id) => custom_ids.contains(id),
                _ => true,
            }
        });
        let minimum_id = custom_ids.into_iter().max().unwrap_or_default() + 1;
        self.next_sidebar_group_id = self.next_sidebar_group_id.max(minimum_id);

        // Descarta atajos desconocidos, inválidos o repetidos. Los valores
        // predeterminados del catálogo siempre están disponibles y el centinela
        // de «sin atajo» se conserva tal cual.
        for keys in self.shortcuts.values_mut() {
            if keys.trim().is_empty() {
                *keys = shortcuts::UNBOUND.to_string();
            }
        }
        self.shortcuts.retain(|id, keys| {
            shortcuts::find(id).is_some()
                && (shortcuts::is_unbound(keys) || shortcuts::validate(keys).is_ok())
        });
        let mut used = HashSet::new();
        for def in shortcuts::SHORTCUTS {
            if let Some(keys) = self.shortcuts.get(def.id) {
                if shortcuts::is_unbound(keys) {
                    continue;
                }
                if used.insert(shortcuts::normalize(keys)) {
                    continue;
                }
                self.shortcuts.remove(def.id);
            }
            used.insert(shortcuts::normalize(def.default));
        }
    }
    pub fn folder_view(&self, path: &std::path::Path) -> Option<&FolderView> {
        if !self.remember_folder_views {
            return None;
        }
        let key = path.to_string_lossy().to_lowercase();
        self.folder_views
            .iter()
            .find(|v| v.path.to_string_lossy().to_lowercase() == key)
    }
    pub fn remember_view(&mut self, view: FolderView) {
        if !self.remember_folder_views {
            return;
        }
        let key = view.path.to_string_lossy().to_lowercase();
        self.folder_views
            .retain(|v| v.path.to_string_lossy().to_lowercase() != key);
        self.folder_views.push(view);
        if self.folder_views.len() > 500 {
            self.folder_views.remove(0);
        }
    }
}
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        },
        core::PCWSTR,
    };
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let (temp, mut file) = loop {
        let counter = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let temp = path.with_file_name(format!(".xool-files-{}-{counter}.tmp", std::process::id()));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => break (temp, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    let staged = std::io::Write::write_all(&mut file, bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = staged {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    let from: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let result = unsafe {
        MoveFileExW(
            PCWSTR(from.as_ptr()),
            PCWSTR(to.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(std::io::Error::other);
    if result.is_err() { let _ = std::fs::remove_file(temp); }
    result
}
#[cfg(test)]
mod tests {

    #[test]
    fn moving_labels_only_changes_the_targeted_column() {
        use super::{DetailColumn, DetailLabel};
        let mut settings = Settings::default();
        let from = std::path::Path::new("first.txt");
        let to = std::path::Path::new("second.txt");
        settings.set_file_label(from, DetailColumn::Status, Some(DetailLabel::Idea));
        settings.set_file_label(to, DetailColumn::Status, Some(DetailLabel::Published));
        settings.set_file_label(to, DetailColumn::Priority, Some(DetailLabel::High));
        assert!(settings.move_file_label(from, to, DetailColumn::Status));
        assert_eq!(settings.file_labels[from].status, Some(DetailLabel::Published));
        assert_eq!(settings.file_labels[to].status, Some(DetailLabel::Idea));
        assert_eq!(settings.file_labels[to].priority, Some(DetailLabel::High));
        let empty = std::path::Path::new("third.txt");
        assert!(settings.move_file_label(from, empty, DetailColumn::Status));
        assert!(!settings.file_labels.contains_key(from));
        assert!(!settings.move_file_label(empty, empty, DetailColumn::Status));
    }

    #[test]
    fn removed_context_menu_style_migrates_without_losing_preferences() {
        let settings: Settings = serde_json::from_str(
            r#"{"context_menu_style":"full","shell_menu_actions":false}"#,
        ).unwrap();
        assert_eq!(settings.context_menu_style, super::ContextMenuStyle::Standard);
        assert!(!settings.shell_menu_actions);
        assert_eq!(serde_json::to_value(settings).unwrap()["context_menu_style"], "standard");
    }

    #[test]
    fn zoom_switches_layouts_at_the_requested_sizes() {
        let cases = [
            (0, ViewMode::Details, 1.0, 0, 10),
            (10, ViewMode::Tree, 1.0, 0, 15),
            (15, ViewMode::List, 1.0, 10, 25),
            (25, ViewMode::Grid, 1.0, 15, 30),
            (100, ViewMode::Grid, 2.0, 95, 100),
        ];
        for (percent, mode, scale, previous, next) in cases {
            assert_eq!(ViewMode::from_zoom(percent), (mode, scale));
            assert_eq!(mode.zoom_percent(scale), percent);
            assert_eq!(mode.stepped_zoom(scale, false), previous);
            assert_eq!(mode.stepped_zoom(scale, true), next);
        }
    }

    #[test]
    fn existing_uncompressed_list_restores_as_details() {
        let mut settings: Settings = serde_json::from_str(r#"{"view":"list","icon_scale":0.75}"#).unwrap();
        settings.validate();
        assert_eq!(settings.view, ViewMode::Details);
        assert_eq!(settings.icon_scale, 1.0);

        let mut settings = Settings::default();
        settings.view = ViewMode::List;
        let mut round_trip: Settings = serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        round_trip.validate();
        assert_eq!(round_trip.view, ViewMode::List);
    }

    use super::*;
    #[test]
    fn atomic_settings_replace_preserves_valid_json() {
        let base = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("settings-test-{}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        let path = base.join("settings.json");
        atomic_write(&path, b"{}").unwrap();
        let mut settings = Settings::default();
        settings.restore_session = false;
        atomic_write(&path, &serde_json::to_vec(&settings).unwrap()).unwrap();
        let loaded: Settings = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(!loaded.restore_session);
        assert!(
            !path
                .with_extension(format!("{}.tmp", std::process::id()))
                .exists()
        );
        fs::remove_file(path).unwrap();
        fs::remove_dir(base).unwrap();
    }
    #[test]
    fn migrate_tab_spacing_from_removed_compact_mode() {
        let mut settings: Settings = serde_json::from_str(
            r#"{"compact_mode":{"enabled":true,"flush_tabs":true},"compact_row_height":22}"#,
        ).unwrap();
        settings.validate();

        let saved = serde_json::to_value(settings).unwrap();
        assert_eq!(saved["flush_tabs_when_maximized"], true);
        assert_eq!(saved["compact_row_height"], 22);
        assert!(saved.get("compact_mode").is_none());
    }

    #[test]
    fn preference_import_checks_format_and_keeps_saved_values() {
        assert!(Settings::parse_import(r#"{"unrelated":true}"#).is_err());
        assert!(Settings::parse_import(r#"{"compact_row_height":"22"}"#).is_err());
        let legacy = Settings::parse_import(r#"{"compact_row_height":22}"#).unwrap();
        assert!(!legacy.active_tab_gradient);
        assert_eq!(legacy.tab_gradient, TabGradient::default());
        let current = Settings {
            compact_row_height: 22,
            flush_tabs_when_maximized: true,
            active_tab_gradient: true,
            tab_gradient: TabGradient {
                intensity: 2, border: 0,
            },
            ..Settings::default()
        };
        let imported = Settings::parse_import(&serde_json::to_string(&current).unwrap()).unwrap();
        assert_eq!(imported.compact_row_height, 22);
        assert!(imported.flush_tabs_when_maximized);
        assert!(imported.active_tab_gradient);
        assert_eq!(imported.tab_gradient, current.tab_gradient);
        let previous = Settings::parse_import(r#"{"active_tab_gradient":true,"tab_gradient":{"intensity":1,"border":2,"direction":1,"color_mode":2,"style":2,"colors":[0,16777215]}}"#).unwrap();
        assert!(previous.active_tab_gradient);
        assert_eq!(previous.tab_gradient, TabGradient { intensity: 1, border: 2 });
    }

    #[test]
    fn old_settings_load_and_validate() {
        let mut s: Settings =
            serde_json::from_str(r#"{"icon_scale":900,"sidebar_width":-1,"audio_volume":8}"#)
                .unwrap();
        s.validate();
        assert_eq!(s.icon_scale, 2.0);
        assert_eq!(s.sidebar_width, 180.);
        assert_eq!(s.audio_volume, 1.);
        assert!(s.remember_folder_views);
        assert!(!s.status_bar_above);
        s.status_bar_above = true;
        let json = serde_json::to_string(&s).unwrap();
        assert!(serde_json::from_str::<Settings>(&json).unwrap().status_bar_above);
    }
    #[test]
    fn unbound_shortcut_survives_validation() {
        let mut s = Settings::default();
        s.unbind_shortcut("new_tab");
        assert_eq!(s.shortcut("new_tab"), "");
        s.validate();
        assert_eq!(s.shortcut("new_tab"), "");
        assert!(s.shortcuts.contains_key("new_tab"));
        s.reset_shortcut("new_tab");
        assert_eq!(s.shortcut("new_tab"), "ctrl-t");
    }

    #[test]
    fn folder_preferences_replace_case_insensitively() {
        let mut s = Settings::default();
        for path in ["C:\\Photos", "c:\\photos"] {
            s.remember_view(FolderView {
                path: path.into(),
                view: ViewMode::Grid,
                scale: 1.2,
                compact: false,
                sort: crate::fs_model::SortKey::Size,
                descending: true,
            });
        }
        assert_eq!(s.folder_views.len(), 1);
        assert!(
            s.folder_view(std::path::Path::new("C:\\PHOTOS"))
                .unwrap()
                .descending
        );
        s.remember_folder_views = false;
        assert!(s.folder_view(std::path::Path::new("C:\\Photos")).is_none());
    }
}
