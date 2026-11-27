#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod drag_object;
mod fs_model;
mod image_edits;
mod media;
mod marquee;
mod motion;
mod integration;
mod instance;
mod pane_layout;
mod platform;
mod quick_search;
mod search_index;
mod settings;
mod shell_menu;
mod shortcuts;
mod sounds;
mod theme;
mod tray;
mod thumbs;
mod ui;
mod workspaces;

use gpui::{
    App, Application, Bounds, KeyBinding, WindowBounds, WindowOptions, prelude::*, px, size,
};

use app::{
    Back, CloseTab, CopySelected, CutSelected, DeleteSelected, EscapeOverlay, FavoriteCurrent,
    FocusAddress, FocusSearch, Forward, NewFolder, NewTab, NextTab, OpenSelected, OpenSettings,
    PasteFiles, PrevTab, Refresh, RenameSelected, SelectAllFiles, SortByKind, SortByModified,
    SortByName, SortBySize, ToggleHidden, ToggleRecursiveSearch, TogglePreview, ToggleSidebar, ToggleSplit,
    ToggleViewMode, Up, ZoomIn, ZoomOut, ZoomReset,
};

fn shortcut_bindings(settings: &settings::Settings) -> Vec<KeyBinding> {
    let context = Some("XoolFiles");
    let mut used = std::collections::HashSet::new();
    let mut bindings = Vec::new();
    for def in shortcuts::SHORTCUTS {
        let keys = settings.shortcut(def.id);
        if shortcuts::is_unbound(&keys) {
            continue;
        }
        if !used.insert(shortcuts::normalize(&keys)) {
            continue;
        }
        let keys = keys.as_str();
        let binding = match def.id {
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
            _ => continue,
        };
        bindings.push(binding);
    }
    bindings
}

struct XoolAssets;

const ICON_ASSETS: &[(&str, &[u8])] = &[
    ("icons/image-convert.svg", include_bytes!("../assets/image-convert.svg")),
    ("icons/image-resize.svg", include_bytes!("../assets/image-resize.svg")),
    ("icons/image-mirror-horizontal.svg", include_bytes!("../assets/image-mirror-horizontal.svg")),
    ("icons/image-mirror-vertical.svg", include_bytes!("../assets/image-mirror-vertical.svg")),
    ("icons/xool-files.png", include_bytes!("../assets/xool-files-preview.png")),
    ("icons/nav-back.svg", include_bytes!("../assets/nav-back.svg")),
    ("icons/tab-close.svg", include_bytes!("../assets/tab-close.svg")),
    ("icons/nav-forward.svg", include_bytes!("../assets/nav-forward.svg")),
    ("icons/nav-up.svg", include_bytes!("../assets/nav-up.svg")),
    ("icons/nav-refresh.svg", include_bytes!("../assets/nav-refresh.svg")),
    ("icons/toolbar-view-details.svg", include_bytes!("../assets/toolbar-view-details.svg")),
    ("icons/toolbar-view-grid.svg", include_bytes!("../assets/toolbar-view-grid.svg")),
    ("icons/toolbar-settings.svg", include_bytes!("../assets/toolbar-settings.svg")),
    ("icons/settings-gear.svg", include_bytes!("../assets/settings-gear.svg")),
    ("icons/settings-sidebar.svg", include_bytes!("../assets/settings-sidebar.svg")),
    ("icons/sidebar-heading-favorites.svg", include_bytes!("../assets/sidebar-heading-favorites.svg")),
    ("icons/sidebar-heading-quick.svg", include_bytes!("../assets/sidebar-heading-quick.svg")),
    ("icons/sidebar-heading-drives.svg", include_bytes!("../assets/sidebar-heading-drives.svg")),
    ("icons/sidebar-heading-recent.svg", include_bytes!("../assets/sidebar-heading-recent.svg")),
    ("icons/settings-workspaces.svg", include_bytes!("../assets/settings-workspaces.svg")),
    ("icons/titlebar-sidebar.svg", include_bytes!("../assets/titlebar-sidebar.svg")),
    ("icons/titlebar-sidebar-hidden.svg", include_bytes!("../assets/titlebar-sidebar-hidden.svg")),
    ("icons/settings-context-menu.svg", include_bytes!("../assets/settings-context-menu.svg")),
    ("icons/settings-integration.svg", include_bytes!("../assets/settings-integration.svg")),
];

impl gpui::AssetSource for XoolAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        Ok(ICON_ASSETS.iter().find(|(name, _)| *name == path)
            .map(|(_, bytes)| std::borrow::Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<gpui::SharedString>> {
        Ok(ICON_ASSETS.iter().map(|(name, _)| *name).filter(|name| name.starts_with(path))
            .map(Into::into).collect())
    }
}

mod i18n;

fn main() {
    std::panic::set_hook(Box::new(|info| {
        if let Some(path) = settings::settings_path() {
            let log = path.with_file_name("last-error.log");
            if let Some(parent) = log.parent() { let _ = std::fs::create_dir_all(parent); }
            let _ = std::fs::write(log, format!("{}\n{}\n{}", chrono::Local::now(), info,
                std::backtrace::Backtrace::force_capture()));
        }
    }));
    let mut args = std::env::args_os().skip(1);
    let option = args.next();
    let temporary = option.as_deref() == Some(std::ffi::OsStr::new("--new-window"));
    let folder = args.next();
    let startup_folder = if temporary || option.as_deref() == Some(std::ffi::OsStr::new("--open-folder")) {
        folder.and_then(|path| {
            if temporary && fs_model::is_recycle_bin(std::path::Path::new(&path)) {
                Some(path.into())
            } else {
                std::path::absolute(path).ok().filter(|path| path.is_dir())
            }
        })
    } else { None };
    let mut settings = settings::Settings::load();
    i18n::set(settings.language);
    let _instance = if temporary {
        if startup_folder.is_none() { return; }
        None
    } else {
        match instance::Primary::start(startup_folder.as_deref()) {
            Ok(Some(primary)) => Some(primary),
            Ok(None) => return,
            Err(_) => {
                instance::report_start_error();
                return;
            }
        }
    };
    Application::new().with_assets(XoolAssets).run(move |cx: &mut App| {
        gpui_component::init(cx);

        settings.temporary_window = temporary;
        let component_mode = if theme::is_dark(&settings) {
            gpui_component::ThemeMode::Dark
        } else {
            gpui_component::ThemeMode::Light
        };
        gpui_component::Theme::change(component_mode, None, cx);
        theme::apply_component_theme(&settings, cx);

        cx.bind_keys(shortcut_bindings(&settings));

        open_explorer_window(settings, startup_folder, cx).expect(crate::i18n::tr("no se pudo abrir la ventana de Xool Files", "could not open the Xool Files window"));
        cx.activate(true);
    });
}

fn open_explorer_window(settings: settings::Settings, folder: Option<std::path::PathBuf>, cx: &mut App) -> gpui::Result<()> {
    let bounds = Bounds::centered(None, size(px(1280.0), px(820.0)), cx);
    let temporary = settings.temporary_window;
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(320.0), px(320.0))),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("Xool Files".into()),
                appears_transparent: true,
                traffic_light_position: None,
            }),
            ..Default::default()
        },
        move |window, cx| {
            platform::enable_native_snap(window);
            if !temporary && let Some(placement) = settings.window_placement {
                platform::restore_window_placement(window, placement);
            }
            platform::set_window_icon(window);
            let (initial_folder, launch_folder) = if temporary { (folder, None) } else { (None, folder) };
            let view = cx.new(|cx| app::XoolFiles::new(settings, initial_folder, window, cx));
            view.update(cx, |this, cx| {
                if !temporary { this.initialize_integration(window, cx); }
                this.start_initial_scans(cx);
                if let Some(path) = launch_folder { this.navigate(path, window, cx); }
                this.focus_root(window);
            });
            if !temporary {
                instance::publish(window).expect(crate::i18n::tr("no se pudo registrar la ventana principal de Xool Files", "could not register the main Xool Files window"));
            }
            let weak = view.downgrade();
            window.on_window_should_close(cx, move |window, cx| {
                weak.update(cx, |this, cx| this.request_window_close(window, cx)).unwrap_or(true)
            });
            cx.new(|cx| gpui_component::Root::new(view, window, cx).with_transparent_background())
        },
    )?;
    Ok(())
}
