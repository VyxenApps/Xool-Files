use gpui_component::{ActiveTheme as _, Side, Sizable as _, menu::{ContextMenuExt, PopupMenu, PopupMenuItem}, popover::Popover};

use super::*;
use crate::settings::Accent;
use crate::workspaces::{AccessOrder, AccessStyle, AccessTarget, DividerAlignment, Item, NameEdit, PIN_PAGE_SIZES, Preferences, ROW_GAP, ROW_HEIGHT, Row, RowDensity, RowDestination, WorkspaceIcon};

#[derive(Clone)]
struct ShortcutDrag(PathBuf);

#[derive(Clone)]
struct RowDrag { id: u64, path: Option<PathBuf>, can_group: bool }

fn access_context_options(
    menu: PopupMenu, owner: gpui::WeakEntity<XoolFiles>, access: AccessTarget,
    default_icon: &'static str,
    window: &mut Window, cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let rename_owner = owner.clone();
    let menu = menu.item(PopupMenuItem::new(tr("Renombrar acceso", "Rename shortcut")).on_click(move |_, window, cx| {
        let _ = rename_owner.update(cx, |app, cx| app.rename_workspace_access(access, window, cx));
    }));
    icon_context_options(menu, owner, access, default_icon, window, cx)
}

fn icon_context_options(
    menu: PopupMenu, owner: gpui::WeakEntity<XoolFiles>, access: AccessTarget,
    default_icon: &'static str, window: &mut Window, cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let icon_owner = owner.clone();
    let (selected_icon, selected_color) = owner.upgrade()
        .and_then(|app| access.style(app.read(cx).settings.workspaces.current()))
        .map(|style| (style.icon, style.color)).unwrap_or_default();
    menu.submenu(tr("Cambiar icono", "Change icon"), window, cx, move |menu, _, _| {
        std::iter::once((None, tr("Predeterminado", "Default")))
            .chain(WorkspaceIcon::options().map(|(icon, name)| (Some(icon), name)))
            .fold(menu.animate_open().check_side(Side::Right), |menu, (icon, name)| {
                let owner = icon_owner.clone();
                let symbol = icon.map(WorkspaceIcon::glyph).unwrap_or(default_icon);
                menu.item(PopupMenuItem::element(move |_, _| {
                    access_choice_preview(name, glyph_inherit(symbol, 14.))
                }).checked(selected_icon == icon).on_click(move |_, _, cx| {
                    let _ = owner.update(cx, |app, cx| {
                        if let Some(style) = access.style_mut(app.settings.workspaces.current_mut()) {
                            style.icon = icon;
                            app.settings.save();
                            cx.notify();
                        }
                    });
                }))
            })
    })
    .submenu(tr("Cambiar color del icono", "Change icon color"), window, cx, move |menu, _, _| {
        std::iter::once((None, tr("Predeterminado", "Default")))
            .chain(theme::accents().map(|(color, name)| (Some(color), name)))
            .fold(menu.animate_open().check_side(Side::Right), |menu, (color, name)| {
                let owner = owner.clone();
                menu.item(PopupMenuItem::element(move |_, cx| {
                    let dot = div().size(px(10.)).rounded_full().border_1()
                        .border_color(cx.theme().muted_foreground);
                    let swatch = match color {
                        Some(color) => dot.bg(rgb(color.rgb())).border_color(rgb(color.rgb())),
                        None => dot,
                    };
                    access_choice_preview(name, swatch)
                }).checked(selected_color == color).on_click(move |_, _, cx| {
                    let _ = owner.update(cx, |app, cx| {
                        if let Some(style) = access.style_mut(app.settings.workspaces.current_mut()) {
                            style.color = color;
                            app.settings.save();
                            cx.notify();
                        }
                    });
                }))
            })
    })
}

fn access_choice_preview(name: &'static str, preview: Div) -> Div {
    div().flex().flex_1().items_center().gap(px(7.)).whitespace_nowrap()
        .child(div().size(px(16.)).flex_shrink_0().flex().items_center().justify_center().child(preview))
        .child(name)
}

#[derive(Clone, Copy)]
enum FolderDropTarget {
    Pins(Option<usize>),
    List(usize),
    Row(RowDestination),
}

impl FolderDropTarget {
    fn add(self, app: &mut XoolFiles, paths: impl IntoIterator<Item = PathBuf>, allow_files: bool, cx: &mut Context<XoolFiles>) {
        app.finish_drag();
        match self {
            Self::Pins(slot) => { app.pin_workspace_shortcuts(paths, slot, allow_files, cx); }
            Self::List(index) => {
                app.insert_workspace_shortcuts(paths, index, allow_files, cx);
            }
            Self::Row(destination) => app.add_workspace_shortcuts_at(destination, paths, allow_files, cx),
        }
    }

    fn accept(self, element: gpui::Stateful<Div>, p: &Colors, cx: &mut Context<XoolFiles>) -> gpui::Stateful<Div> {
        let colors = *p;
        element
            .drag_over::<DragTab>(move |style, _, _, _| style.bg(colors.accent_faint).border_color(colors.accent))
            .drag_over::<ShortcutDrag>(move |style, _, _, _| style.bg(colors.accent_faint).border_color(colors.accent))
            .drag_over::<SidebarFavoriteDrag>(move |style, _, _, _| style.bg(colors.accent_faint).border_color(colors.accent))
            .drag_over::<DragFiles>(move |style, _, _, _| style.bg(colors.accent_faint).border_color(colors.accent))
            .drag_over::<gpui::ExternalPaths>(move |style, _, _, _| style.bg(colors.accent_faint).border_color(colors.accent))
            .on_drop(cx.listener(move |app: &mut XoolFiles, drag: &DragTab, _, cx| {
                if let Some(path) = app.workspace_tab_path(drag.tab_id) { self.add(app, [path], false, cx); }
                cx.stop_propagation();
            }))
            .on_drop(cx.listener(move |app: &mut XoolFiles, drag: &ShortcutDrag, _, cx| {
                self.add(app, [drag.0.clone()], true, cx);
                cx.stop_propagation();
            }))
            .on_drop(cx.listener(move |app: &mut XoolFiles, drag: &SidebarFavoriteDrag, _, cx| {
                self.add(app, [drag.0.clone()], true, cx);
                cx.stop_propagation();
            }))
            .on_drop(cx.listener(move |app: &mut XoolFiles, files: &DragFiles, _, cx| {
                self.add(app, files.paths.iter().cloned(), true, cx);
                cx.stop_propagation();
            }))
            .on_drop(cx.listener(move |app: &mut XoolFiles, files: &gpui::ExternalPaths, _, cx| {
                self.add(app, files.paths().iter().cloned(), true, cx);
                cx.stop_propagation();
            }))
            .when(matches!(self, Self::Pins(_)), |element| element
                .drag_over::<RowDrag>(move |style, drag, _, _| {
                    if drag.path.is_some() { style.bg(colors.accent_faint).border_color(colors.accent) } else { style }
                })
                .on_drop(cx.listener(move |app: &mut XoolFiles, drag: &RowDrag, _, cx| {
                    if let Some(path) = &drag.path { self.add(app, [path.clone()], true, cx); }
                    cx.stop_propagation();
                })))
    }
}

pub(super) fn preferences(search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    let count_title = tr("Iconos fijados por página", "Pinned icons per page");
    let count_description = tr("Elige cuántas tarjetas aparecen en cada página.", "Choose how many tiles appear on each page.");
    let selected = PIN_PAGE_SIZES.iter().position(|&size| size == app.settings.workspaces.pin_page_size).unwrap_or(2);
    div().flex().flex_col()
        .child(settings_section(search, tr("PANEL DE TRABAJO", "WORKSPACE PANEL"), p))
        .child(settings_surface(p)
            .child(toggle_row(search, "workspaces-enabled", tr("Mostrar panel de espacios de trabajo", "Show workspace panel"),
                tr("Muestra u oculta el panel con tus accesos, fijados y divisiones.", "Show or hide the panel with your shortcuts, pins, and dividers."),
                app.settings.workspaces.enabled, p,
                cx.listener(|app: &mut XoolFiles, _, window, cx| {
                    app.toggle_workspace_panel(window, cx);
                })))
            .child(settings_divider(p))
            .child(toggle_row(search, "workspace-titlebar-button", tr("Botón en la barra de pestañas", "Button in the tab bar"),
                tr("Permite mostrar u ocultar el panel desde la barra de pestañas.", "Show or hide the panel directly from the tab bar."),
                app.settings.workspaces.show_titlebar_button, p,
                cx.listener(|app: &mut XoolFiles, _, _, cx| {
                    app.settings.workspaces.show_titlebar_button = !app.settings.workspaces.show_titlebar_button;
                    app.settings.save();
                    cx.notify();
                })))
            .child(settings_divider(p))
            .child(settings_choices(search, "workspace-side",
                (tr("Posición del panel", "Panel position"),
                    Some(tr("Colócalo en el extremo izquierdo o derecho de la ventana.", "Place it on the left or right edge of the window."))),
                [tr("Izquierda", "Left"), tr("Derecha", "Right")],
                app.settings.workspaces.right, p, cx,
                |app, right| app.settings.workspaces.right = right))
            .child(settings_divider(p))
            .child(search.mark(div().p(px(12.)).flex().flex_wrap().items_center().justify_between().gap(px(8.)),
                &[count_title, count_description])
                .child(settings_row_text(count_title, Some(count_description), p))
                .child(settings_segment_group("workspace-pin-page-size", ["2", "4", "6", "8"], selected, None, p, cx,
                    |app, index, _, cx| app.set_workspace_pin_page_size(PIN_PAGE_SIZES[index], cx))))
            .child(settings_divider(p))
            .child(toggle_row(search, "workspaces-pin-names", tr("Nombres en los fijados", "Names on pinned items"),
                tr("Muestra el nombre bajo cada icono. Puedes verlo al pasar el cursor.", "Show the name below each icon. You can also see it on hover."),
                app.settings.workspaces.show_pin_names, p,
                cx.listener(|app: &mut XoolFiles, _, _, cx| {
                    app.settings.workspaces.show_pin_names = !app.settings.workspaces.show_pin_names;
                    app.settings.save();
                    cx.notify();
                })))
            .child(settings_divider(p))
            .child(toggle_row(search, "workspaces-system-icons", tr("Usar iconos de Windows", "Use Windows icons"),
                tr("Muestra los iconos de Windows para las carpetas y archivos del panel.", "Show Windows icons for folders and files in the panel."),
                app.settings.workspaces.use_system_icons, p,
                cx.listener(|app: &mut XoolFiles, _, _, cx| {
                    app.settings.workspaces.use_system_icons = !app.settings.workspaces.use_system_icons;
                    app.settings.save();
                    cx.notify();
                }))))
        .child(settings_section(search, tr("INICIO", "STARTUP"), p))
        .child(settings_surface(p).child(workspace_startup_settings(search, app, p, cx)))
        .child(settings_section(search, tr("PERSONALIZACIÓN", "CUSTOMIZATION"), p))
        .child(workspace_identity_settings(search, app, p, cx))
        .child(settings_section(search, tr("DIVISORES", "DIVIDERS"), p))
        .child(settings_surface(p).child(divider_alignment_settings(search, app, p, cx)))
        .child(settings_section(search, tr("LISTA DE ACCESOS", "SHORTCUT LIST"), p))
        .child(access_list_settings(search, app, p, cx))
        .child(settings_section(search, tr("BOTÓN DE LOS FIJADOS", "PINNED ITEM BUTTON"), p))
        .child(pin_close_settings(search, app, p, cx))
        .into_any_element()
}

fn access_list_settings(search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> Div {
    let prefs = &app.settings.workspaces;
    let density = RowDensity::OPTIONS.iter().position(|&value| value == prefs.row_density).unwrap_or(1);
    let order = AccessOrder::OPTIONS.iter().position(|&value| value == prefs.access_order).unwrap_or(0);
    let density_title = tr("Densidad de las filas", "Row density");
    let order_title = tr("Orden de los accesos", "Shortcut order");
    let order_description = tr("Ordena la lista dentro de cada división. Los fijados conservan su posición.", "Sort the list within each divider. Pinned tiles keep their positions.");
    let setting_row = || div().p(px(12.)).flex().flex_wrap().items_center().justify_between().gap(px(8.));
    settings_surface(p)
        .child(search.mark(setting_row(), &[density_title])
            .child(settings_row_text(density_title, None, p))
            .child(settings_segment_group("workspace-density",
                [tr("Compacta", "Compact"), "Normal", tr("Cómoda", "Comfortable")],
                density, None, p, cx, |app, index, _, cx| {
                    app.settings.workspaces.row_density = RowDensity::OPTIONS[index];
                    app.settings.save();
                    cx.notify();
                })))
        .child(settings_divider(p))
        .child(search.mark(setting_row(), &[order_title, order_description])
            .child(settings_row_text(order_title, Some(order_description), p))
            .child(settings_segment_group("workspace-order",
                ["Manual", tr("Alfabético", "Alphabetical"), tr("Uso reciente", "Recently used")],
                order, None, p, cx, |app, index, _, cx| {
                    app.settings.workspaces.access_order = AccessOrder::OPTIONS[index];
                    app.settings.save();
                    cx.notify();
                })))
        .child(settings_divider(p))
        .child(toggle_row(search, "workspace-width-per-space", tr("Recordar el ancho por espacio", "Remember width per workspace"),
            tr("Guarda un ancho distinto al redimensionar cada espacio.", "Save a separate width when resizing each workspace."),
            prefs.remember_width_per_space, p, cx.listener(|app: &mut XoolFiles, _, _, cx| {
                app.settings.workspaces.remember_width_per_space = !app.settings.workspaces.remember_width_per_space;
                app.settings.save();
                cx.notify();
            })))
        .child(settings_divider(p))
        .child(toggle_row(search, "workspace-highlight-open", tr("Resaltar la carpeta abierta", "Highlight the open folder"),
            tr("Marca la carpeta actual en la lista y en la cuadrícula de fijados.", "Mark the current folder in both the list and pinned grid."),
            prefs.highlight_open_folder, p, cx.listener(|app: &mut XoolFiles, _, _, cx| {
                app.settings.workspaces.highlight_open_folder = !app.settings.workspaces.highlight_open_folder;
                app.settings.save();
                cx.notify();
            })))
}

fn pin_close_settings(search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> Div {
    let prefs = &app.settings.workspaces;
    settings_surface(p)
        .child(toggle_row(search, "workspace-show-pin-close", tr("Mostrar botón Quitar", "Show remove button"),
            tr("Muestra una X al pasar el cursor sobre un acceso o un fijado.", "Show an X when hovering over a shortcut or pinned item."),
            prefs.show_pin_close, p, cx.listener(|app: &mut XoolFiles, _, _, cx| {
                app.settings.workspaces.show_pin_close = !app.settings.workspaces.show_pin_close;
                app.settings.save();
                cx.notify();
            })))
        .child(settings_divider(p))
        .child(toggle_row(search, "workspace-pin-close-left", tr("En el lado izquierdo", "On the left"),
            tr("Coloca la X a la izquierda del elemento.", "Place the X on the left side of the item."),
            prefs.pin_close_left, p, cx.listener(|app: &mut XoolFiles, _, _, cx| {
                app.settings.workspaces.pin_close_left = !app.settings.workspaces.pin_close_left;
                app.settings.save();
                cx.notify();
            })))
        .child(settings_divider(p))
        .child(toggle_row(search, "workspace-pin-close-always", tr("Mostrar siempre la X", "Always show the X"),
            tr("Mantiene visible la X aunque el cursor esté fuera.", "Keep the X visible when the pointer leaves."),
            prefs.pin_close_always, p, cx.listener(|app: &mut XoolFiles, _, _, cx| {
                app.settings.workspaces.pin_close_always = !app.settings.workspaces.pin_close_always;
                app.settings.save();
                cx.notify();
            })))
}

fn workspace_startup_settings(search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> Div {
    let title = tr("Espacio al iniciar", "Workspace on startup");
    let description = tr("Abre el último espacio usado o uno que elijas.", "Open the last used workspace or one you choose.");
    let last = tr("Último espacio usado", "Last used workspace");
    let prefs = &app.settings.workspaces;
    search.mark(div().p(px(12.)).flex().flex_wrap().items_center().justify_between().gap(px(8.)), &[title, description, last])
        .child(settings_row_text(title, Some(description), p))
        .child(workspace_settings_picker("workspace-startup-space", prefs, prefs.startup_space, Some(last), p, cx, |app, index, cx| {
            app.settings.workspaces.startup_space = index;
            app.settings.save();
            cx.notify();
        }))
}

fn workspace_settings_picker(
    id: &'static str, prefs: &Preferences, selected: Option<usize>, automatic: Option<&'static str>,
    p: &Colors, cx: &mut Context<XoolFiles>,
    choose: fn(&mut XoolFiles, Option<usize>, &mut Context<XoolFiles>),
) -> AnyElement {
    let options: Vec<_> = automatic.into_iter().map(|label| (None, label.to_owned()))
        .chain(prefs.spaces.iter().enumerate().map(|(index, space)| (Some(index), space.label().to_owned())))
        .collect();
    let label = options.iter().find(|(option, _)| *option == selected)
        .map(|(_, label)| label.clone()).unwrap_or_default();
    let owner = cx.weak_entity();
    let can_create = automatic.is_none();
    let colors = *p;
    Popover::new(id).anchor(gpui::Corner::TopRight).appearance(false)
        .trigger(settings_dropdown_trigger(id, p, cx).max_w(px(230.))
            .child(div().min_w(px(0.)).truncate().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).child(label))
            .child(glyph("\u{E70D}", 9., p.muted)))
        .content(move |_, _, cx| {
            div().id("options").w(px(230.)).max_h(px(260.)).overflow_y_scroll().p(px(4.))
                .bg(colors.panel).border_1().border_color(colors.border_soft).rounded(px(11.)).shadow_lg()
                .children(options.iter().enumerate().map(|(row, (index, label))| {
                    let index = *index;
                    let owner = owner.clone();
                    settings_dropdown_option((id, row), index == selected, &colors)
                        .child(div().flex_1().min_w(px(0.)).truncate().child(label.clone()))
                        .when(index == selected, |option| option.child(glyph("\u{E73E}", 10., colors.accent)))
                        .on_click(cx.listener(move |popover, _, window, cx| {
                            popover.dismiss(window, cx);
                            let _ = owner.update(cx, |app, cx| choose(app, index, cx));
                        }))
                }))
                .when(can_create, |menu| {
                    let owner = owner.clone();
                    menu.child(settings_dropdown_option(("workspace-settings-new", 0), false, &colors)
                        .child(glyph("\u{E710}", 11., colors.accent))
                        .child(tr("Nuevo espacio", "New workspace"))
                        .on_click(cx.listener(move |popover, _, window, cx| {
                            popover.dismiss(window, cx);
                            let _ = owner.update(cx, |app, cx| {
                                app.edit_workspace_name(NameEdit::SettingsPreset, window, cx);
                            });
                        })))
                })
        }).into_any_element()
}

fn workspace_identity_settings(search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> Div {
    let prefs = &app.settings.workspaces;
    let index = app.workspace_ui.appearance_workspace.filter(|&index| index < prefs.spaces.len()).unwrap_or(prefs.selected);
    let space = &prefs.spaces[index];
    let appearance_title = tr("Apariencia del espacio", "Workspace appearance");
    let appearance_description = tr("Elige un espacio para cambiar su color e icono.", "Choose a workspace to change its color and icon.");
    let automatic_color = tr("Según el tema", "Follow theme");
    let automatic_icon = tr("Iniciales", "Initials");
    let icons = WorkspaceIcon::options();
    let colors = theme::accents();
    settings_surface(p)
        .child(search.mark(div().p(px(12.)).flex().flex_wrap().items_center().justify_between().gap(px(8.)),
                &[appearance_title, appearance_description])
            .child(settings_row_text(appearance_title, Some(appearance_description), p))
            .child(workspace_settings_picker("workspace-appearance-space", prefs, Some(index), None, p, cx, |app, index, cx| {
                app.workspace_ui.appearance_workspace = index;
                cx.notify();
            })))
        .when(app.workspace_ui.edit == Some(NameEdit::SettingsPreset), |card| {
            card.child(div().px(px(12.)).pb(px(10.)).child(name_editor(app, p, cx)))
        })
        .child(settings_divider(p))
        .child(div().p(px(12.)).flex().flex_col().gap(px(7.))
            .child(search.text(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD), tr("Color del espacio", "Workspace color")))
            .child(div().flex().flex_wrap().gap(px(4.)).items_center()
                .children(std::iter::once((None, automatic_color)).chain(colors.map(|(color, label)| (Some(color), label)))
                    .enumerate().map(|(choice, (color, label))| {
                        let button = identity_choice(("workspace-color", choice), label, color == space.color, p, cx,
                            move |app, cx| {
                                app.settings.workspaces.spaces[index].color = color;
                                app.settings.save();
                                cx.notify();
                            });
                        if let Some(color) = color { button.child(div().size(px(13.)).rounded_full().bg(rgb(color.rgb()))) }
                        else { button.child(label) }
                    })))
            .child(search.text(div().mt(px(4.)).text_size(px(13.)).font_weight(FontWeight::SEMIBOLD), tr("Icono del espacio", "Workspace icon")))
            .child(div().flex().flex_wrap().gap(px(4.)).items_center()
                .children(std::iter::once((None, automatic_icon)).chain(icons.map(|(icon, label)| (Some(icon), label)))
                    .enumerate().map(|(choice, (icon, label))| {
                        let button = identity_choice(("workspace-symbol", choice), label, icon == space.icon, p, cx,
                            move |app, cx| {
                                app.settings.workspaces.spaces[index].icon = icon;
                                app.settings.save();
                                cx.notify();
                            });
                        if let Some(icon) = icon { button.child(glyph(icon.glyph(), 14., p.text)) }
                        else { button.child(label) }
                    }))))
}

fn divider_alignment_settings(search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> Div {
    let title = tr("Alineación del texto", "Text alignment");
    let description = tr("Elige dónde aparece el nombre de cada divisor.", "Choose where each divider name appears.");
    let labels = [tr("Izquierda", "Left"), tr("Centro", "Center"), tr("Derecha", "Right")];
    let alignment = app.settings.workspaces.divider_alignment;
    let selected = DividerAlignment::OPTIONS.iter().position(|&option| option == alignment).unwrap_or_default();
    let matched = search.matches(&[title, description]) || search.matches(&labels);
    div().p(px(12.)).flex().flex_wrap().items_center().justify_between().gap(px(8.))
        .when(matched, |row| row.bg(rgba(SETTINGS_HIGHLIGHT)).rounded(px(5.)))
        .child(settings_row_text(title, Some(description), p))
        .child(settings_segment_group("workspace-divider-alignment", labels, selected, None, p, cx,
            |app, index, _, cx| {
                app.settings.workspaces.divider_alignment = DividerAlignment::OPTIONS[index];
                app.settings.save();
                cx.notify();
            }))
}

fn identity_choice(
    id: (&'static str, usize), label: &'static str, selected: bool, p: &Colors, cx: &mut Context<XoolFiles>,
    choose: impl Fn(&mut XoolFiles, &mut Context<XoolFiles>) + 'static,
) -> gpui::Stateful<Div> {
    div().id(id).h(px(28.)).min_w(px(28.)).px(px(6.)).rounded(px(7.))
        .flex().items_center().justify_center().cursor_pointer().text_size(px(11.))
        .bg(if selected { p.accent_faint } else { p.input })
        .border_1().border_color(if selected { p.accent } else { p.border_soft })
        .hover(|item| item.bg(p.card_hover))
        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(label).build(window, cx))
        .on_click(cx.listener(move |app, _, _, cx| choose(app, cx)))
}

pub(super) fn panel(app: &XoolFiles, width: f32, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    let width = width - PANEL_RESIZE_HANDLE;
    let right = app.settings.workspaces.right;
    let workspace = app.settings.workspaces.current();
    let ui = &app.workspace_ui;
    let page_size = app.settings.workspaces.pin_page_size;
    let icons_only = app.settings.workspaces.icon_only_panel;
    let columns = if icons_only { 1 } else if width >= 216. { page_size.div_ceil(2).clamp(2, 4) } else { 2 };
    let padding = if icons_only { 5. } else { 9. };
    let tile_size = ((width - 2. * padding - 1. - (columns - 1) as f32 * 5.) / columns as f32).floor().min(70.);
    let query = ui.search.read(cx).value();
    let owner = cx.weak_entity();
    let tail = workspace.rows.len();
    let blank = FolderDropTarget::List(tail).accept(
        div().id("workspace-list-drop").flex_1().min_h(px(27.)).w_full().rounded(px(7.)), p, cx)
        .when(ui.edit.is_none() && tail > 0, |blank| blank
            .on_drag_move::<RowDrag>(cx.listener(move |app: &mut XoolFiles, event: &gpui::DragMoveEvent<RowDrag>, _, cx| {
                if event.bounds.contains(&event.event.position)
                    && app.workspace_ui.scroll.bounds().contains(&event.event.position)
                {
                    let id = event.drag(cx).id;
                    app.preview_workspace_drop(id, RowDestination::End, event.bounds, cx);
                }
            }))
            .on_drop(cx.listener(move |app: &mut XoolFiles, drag: &RowDrag, _, cx| {
                app.move_workspace_row(drag.id, RowDestination::End, cx);
            })))
        .context_menu(move |menu, _, _| {
            let add_owner = owner.clone();
            let group_owner = owner.clone();
            let url_owner = owner.clone();
            menu.animate_open()
            .item(PopupMenuItem::new(tr("Crear carpeta desplegable", "Create collapsible folder")).on_click(move |_, window, cx| {
                let _ = group_owner.update(cx, |app, cx| {
                    app.clear_workspace_query(window, cx);
                    app.edit_workspace_name(NameEdit::Group(tail), window, cx);
                });
            }))
            .item(PopupMenuItem::new(tr("Agregar división", "Add divider")).on_click(move |_, window, cx| {
                let _ = add_owner.update(cx, |app, cx| {
                    app.clear_workspace_query(window, cx);
                    app.edit_workspace_name(NameEdit::Divider(tail), window, cx);
                });
            }))
            .item(PopupMenuItem::new(tr("Agregar URL", "Add URL")).on_click(move |_, window, cx| {
                let _ = url_owner.update(cx, |app, cx| {
                    app.clear_workspace_query(window, cx);
                    app.edit_workspace_name(NameEdit::Web(tail), window, cx);
                });
            }))
        });
    let content = div().id("workspace-panel-scroll").track_scroll(&ui.scroll)
        .flex_1().min_h(px(0.)).overflow_y_scroll().px(px(padding)).pb(px(padding))
        .flex().flex_col().gap(px(6.))
        .child(pin_pages(app, columns, tile_size, p, cx))
        .child(div().w_full().h(px(1.)).flex_shrink_0().bg(p.border_soft))
        .when(!icons_only, |content| content.child(Input::new(&ui.search).small().text_size(px(12.)).cleanable(true)))
        .child(if icons_only || query.trim().is_empty() {
            workspace_rows(app, p, cx)
        } else {
            folder_matches(app, p, cx)
        })
        .child(blank);
    let panel = div().id(("workspace-panel", app.settings.workspaces.selected))
        .w(px(width)).h_full().flex_shrink_0().flex().flex_col().bg(p.sidebar)
        .when(right, |panel| panel.border_l_1())
        .when(!right, |panel| panel.border_r_1())
        .border_color(p.border_soft)
        .when(!icons_only, |panel| panel.child(div().px(px(5.)).pt(px(6.)).flex_shrink_0().flex().items_center().gap(px(3.))
            .child(div().flex_1().min_w(px(0.)).flex().items_center().justify_center().gap(px(3.))
                .child(div().flex_1().max_w(px(20.)).h(px(1.)).bg(p.border_soft))
                .child(div().min_w(px(0.)).truncate().whitespace_nowrap().text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(tr("Espacios de trabajo", "Workspaces")))
                .child(div().flex_1().max_w(px(20.)).h(px(1.)).bg(p.border_soft)))
            .child(workspace_selector_mode(app, p, cx).size(px(22.)))))
        .child(header(app, width - 2. * padding - 1., p, cx))
        .child(content);
    let handle = panel_resize_handle("workspace-resizer", ui.resize.is_some(),
        app.settings.translucent_background, p, cx.listener(|app: &mut XoolFiles, event: &MouseDownEvent, window, cx| {
            app.begin_workspace_resize(f32::from(event.position.x), window, cx);
            cx.stop_propagation();
        }));
    let edge = div().h_full().flex_shrink_0().flex();
    if right { edge.child(handle).child(panel) } else { edge.child(panel).child(handle) }.into_any_element()
}

fn workspace_mark(color: Option<Accent>, icon: Option<WorkspaceIcon>, fallback: Rgba) -> Option<Div> {
    let tint = color.map(|color| rgb(color.rgb())).unwrap_or(fallback);
    if let Some(icon) = icon {
        Some(div().flex_shrink_0().child(glyph(icon.glyph(), 13., tint)))
    } else {
        color.map(|_| div().size(px(7.)).flex_shrink_0().rounded_full().bg(tint))
    }
}

fn workspace_badge(name: &str, icon: Option<WorkspaceIcon>, color: Rgba) -> Div {
    if let Some(icon) = icon {
        return glyph(icon.glyph(), 13., color);
    }
    let initials: String = name.split_whitespace().filter_map(|word| word.chars().next())
        .take(2).flat_map(char::to_uppercase).collect();
    div().text_size(px(10.)).font_weight(FontWeight::SEMIBOLD).text_color(color).child(initials)
}

fn header(app: &XoolFiles, selector_width: f32, p: &Colors, cx: &mut Context<XoolFiles>) -> Div {
    let prefs = &app.settings.workspaces;
    let icons_only = prefs.icon_only_panel;
    let selected = prefs.selected;
    let selected_icon = prefs.current().icon;
    let name = prefs.current().label().to_owned();
    let colors = *p;
    let picker = if prefs.compact && !icons_only {
        div().id("workspace-icons-scroll").flex().items_center().gap(px(4.)).overflow_x_scroll()
            .flex_1().min_w(px(0.))
            .children(prefs.spaces.iter().enumerate().map(|(index, space)| {
                let label = space.label().to_owned();
                let default = if index == selected { p.accent } else { p.muted };
                let color = space.color.map(|color| rgb(color.rgb())).unwrap_or(default);
                let badge = workspace_badge(&label, space.icon, color);
                div().id(("workspace-icon", index)).size(px(27.)).flex_shrink_0().rounded(px(8.))
                    .flex().items_center().justify_center().cursor_pointer().text_size(px(10.)).font_weight(FontWeight::SEMIBOLD)
                    .bg(if index == selected { Rgba { a: 0.17, ..color } } else { p.card })
                    .text_color(color)
                    .hover(|style| style.bg(p.card_hover))
                    .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(label.clone()).build(window, cx))
                    .child(badge)
                    .on_click(cx.listener(move |app: &mut XoolFiles, _, window, cx| app.select_workspace(index, window, cx)))
            })).into_any_element()
    } else {
        let owner = cx.weak_entity();
        let menu_width = if icons_only { 199. } else { selector_width.max(0.) };
        let spaces: Vec<_> = prefs.spaces.iter().map(|space| (space.label().to_owned(), space.color, space.icon)).collect();
        let color = prefs.current().color.map_or(p.accent, |accent| rgb(accent.rgb()));
        div().flex_1().min_w(px(0.)).child(Popover::new("workspace-presets").appearance(false)
            .trigger(settings_dropdown_trigger("workspace-presets-open", p, cx)
                .w_full().min_w(px(0.)).h(px(30.)).px(px(7.)).gap(px(5.)).rounded(px(8.))
                .when(icons_only, |button| button.tooltip(name.clone())
                    .child(workspace_badge(&name, selected_icon, color)))
                .when_some((!icons_only).then(|| workspace_mark(prefs.current().color, selected_icon, p.accent)).flatten(),
                    |button, mark| button.child(mark))
                .when(!icons_only, |button| button.child(div().flex_1().min_w(px(0.)).truncate().whitespace_nowrap().text_size(px(12.))
                    .font_weight(FontWeight::SEMIBOLD).child(name))
                    .child(glyph("\u{E70D}", 9., p.muted))))
            .content(move |_, _, cx| {
                let list = div().id("workspace-presets-list").w(px(menu_width)).max_h(px(280.)).overflow_y_scroll()
                    .p(px(4.)).bg(colors.panel).border_1().border_color(colors.border_soft).rounded(px(10.)).shadow_lg()
                    .children(spaces.iter().enumerate().map(|(index, (name, color, icon))| {
                        let owner = owner.clone();
                        settings_dropdown_option(("workspace-preset", index), index == selected, &colors)
                            .when_some(workspace_mark(*color, *icon, colors.accent), |row, mark| row.child(mark))
                            .child(div().flex_1().min_w(px(0.)).truncate().child(name.clone()))
                            .when(index == selected, |row| row.child(glyph("\u{E73E}", 10., colors.accent)))
                            .on_click(cx.listener(move |popover, _, window, cx| {
                                popover.dismiss(window, cx);
                                let _ = owner.update(cx, |app, cx| app.select_workspace(index, window, cx));
                            }))
                    }));
                let owner = owner.clone();
                popup_in(list.child(settings_dropdown_option(("workspace-create", 0), false, &colors)
                    .child(glyph("\u{E710}", 11., colors.accent))
                    .child(tr("Nuevo espacio", "New workspace"))
                    .on_click(cx.listener(move |popover, _, window, cx| {
                        popover.dismiss(window, cx);
                        let _ = owner.update(cx, |app, cx| app.edit_workspace_name(NameEdit::Preset, window, cx));
                    }))), "workspaces-list-enter", |panel, slide| panel.relative().top(px(-slide)))
            })).into_any_element()
    };
    if icons_only {
        return div().flex().flex_col().items_center().gap(px(5.)).p(px(5.)).flex_shrink_0()
            .child(div().w_full().child(picker));
    }
    div().flex().flex_col().gap(px(5.)).p(px(9.)).flex_shrink_0()
        .child(div().flex().items_center().gap(px(4.))
            .child(picker)
            .when(prefs.compact, |bar| bar.child(icon_button("new-workspace", "\u{E710}", tr("Nuevo espacio", "New workspace"), p)
                .on_click(cx.listener(|app: &mut XoolFiles, _, window, cx| {
                    app.edit_workspace_name(NameEdit::Preset, window, cx);
                })))))
        .when(app.workspace_ui.edit == Some(NameEdit::Preset),
            |bar| bar.child(name_editor(app, p, cx)))
}

fn workspace_selector_mode(app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> gpui::Stateful<Div> {
    let compact = app.settings.workspaces.compact;
    let hint = if compact { tr("Mostrar desplegable de espacios", "Show workspace dropdown") }
        else { tr("Mostrar espacios como iconos", "Show workspaces as icons") };
    icon_button("workspace-selector-mode", if compact { "\u{E8FD}" } else { "\u{E8A9}" }, hint, p)
        .on_click(cx.listener(|app: &mut XoolFiles, _, _, cx| {
            app.settings.workspaces.compact = !app.settings.workspaces.compact;
            app.settings.save();
            cx.notify();
        }))
}

fn icon_button(id: impl Into<ElementId>, icon: &'static str, hint: &'static str, p: &Colors) -> gpui::Stateful<Div> {
    div().id(id).size(px(25.)).flex_shrink_0().rounded(px(7.)).cursor_pointer()
        .flex().items_center().justify_center().text_color(p.muted)
        .hover(|style| style.bg(p.card_hover).text_color(p.text))
        .child(glyph_inherit(icon, 12.))
        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(hint).build(window, cx))
}

fn name_editor(app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    let name = app.workspace_ui.name.clone();
    popup_in(div().id(("workspace-name-editor", app.workspace_ui.edit_version))
        .flex().items_center().gap(px(3.)).h(px(ROW_HEIGHT)).flex_shrink_0()
        .child(div().flex_1().min_w(px(0.)).child(Input::new(&name).small().text_size(px(12.))))
        .child(icon_button("confirm-workspace-name", "\u{E73E}", tr("Guardar nombre", "Save name"), p)
            .on_click(cx.listener(move |app: &mut XoolFiles, _, window, cx| {
                let text = name.read(cx).value().trim().to_owned();
                app.commit_workspace_name(text, window, cx);
            })))
        .child(icon_button("cancel-workspace-name", "\u{E711}", tr("Cancelar", "Cancel"), p)
            .on_click(cx.listener(|app: &mut XoolFiles, _, window, cx| app.cancel_workspace_name(window, cx)))),
        "workspace-edit-enter", |editor, slide| editor.relative().top(px(-slide)))
}

fn pin_pages(app: &XoolFiles, columns: usize, tile_size: f32, p: &Colors, cx: &mut Context<XoolFiles>) -> Div {
    let workspace = app.settings.workspaces.current();
    let page_size = app.settings.workspaces.pin_page_size;
    let count = workspace.page_count(page_size);
    let page = app.workspace_ui.page.min(count - 1);
    let mut grid = div().flex().flex_col().gap(px(5.)).w_full();
    for start in (0..page_size).step_by(columns) {
        let end = (start + columns).min(page_size);
        grid = grid.child(div().flex().justify_center().gap(px(5.))
            .children((start..end).map(|slot| pin_tile(app, page * page_size + slot, tile_size, p, cx))));
    }
    let grid = if let Some((version, direction)) = app.workspace_ui.page_motion {
        grid.with_animation(("workspace-pin-page", version),
            Animation::new(Duration::from_millis(195)).with_easing(ease_out_quint()),
            move |grid, progress| grid.relative().left(px((1. - progress) * 52. * direction)).opacity(progress))
            .into_any_element()
    } else { grid.into_any_element() };
    div().flex().flex_col().flex_shrink_0()
        .child(FolderDropTarget::Pins(None).accept(
            div().id("workspace-pin-grid").w_full().overflow_hidden().child(grid), p, cx))
        .child(FolderDropTarget::Pins(None).accept(div().id("workspace-page-dots").w_full().h(px(22.)).overflow_x_scroll()
            .flex().items_center().when(count <= if columns == 1 { 2 } else { 12 }, |dots| dots.justify_center())
            .children((0..count).map(|index| {
                let title = crate::localized!(format, "Página {} de {}", "Page {} of {}", index + 1, count);
                div().id(("workspace-page-dot", index)).size(px(17.)).flex_shrink_0().cursor_pointer()
                    .flex().items_center().justify_center().rounded_full().hover(|dot| dot.bg(p.card_hover))
                    .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(title.clone()).build(window, cx))
                    .child(div().size(px(if page == index { 7. } else { 5. })).rounded_full()
                        .bg(if page == index { p.accent } else { p.muted }))
                    .on_click(cx.listener(move |app: &mut XoolFiles, _, _, cx| app.set_workspace_page(index, cx)))
            })), p, cx))
}

fn pin_tile(app: &XoolFiles, index: usize, size: f32, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    let prefs = &app.settings.workspaces;
    let workspace = prefs.current();
    let path = workspace.pins.get(index).and_then(Option::as_ref);
    let file = path.is_some_and(|path| workspace.file_pins.contains(path));
    let occupied = path.is_some();
    let colors = *p;
    let tile = div().id(("workspace-pin", index)).group("workspace-pinned-item").relative().size(px(size)).flex_shrink_0()
        .rounded(px(10.)).border_1().border_color(p.border_soft).bg(p.card)
        .flex().flex_col().items_center().justify_center().gap(px(4.)).px(px(3.))
        .cursor_pointer().hover(|style| style.bg(p.card_hover));
    let tile = FolderDropTarget::Pins(Some(index)).accept(tile, p, cx);
    let editing = app.workspace_ui.edit == Some(NameEdit::RenameAccess(AccessTarget::Pin(index)));
    let tile = if let Some(path) = path {
        let icon = if file { workspace_file_glyph(path) } else { "\u{E8B7}" };
        let active = prefs.highlight_open_folder && crate::workspaces::same_path(&app.active_tab().path, path);
        let label = workspace.access_name(path);
        let tooltip = format!("{}\n{}", label, path.display());
        let drag_label = label.clone();
        let target = path.clone();
        tile.when(active, |tile| tile.bg(p.accent_faint).border_color(p.accent_soft))
            .child(workspace_location_icon(app, path, icon, 17., p.accent))
            .when(editing, |tile| tile.child(div().w_full().child(
                Input::new(&app.workspace_ui.name).small().text_size(px(11.)))))
            .when(!editing && prefs.show_pin_names && !prefs.icon_only_panel, |tile| {
                tile.child(div().w_full().truncate().text_center().text_size(px(10.)).child(label))
            })
            .when(!editing && prefs.show_pin_close, |tile| {
                let close = icon_button(("workspace-unpin", index), "icons/tab-close.svg", tr("Desfijar", "Unpin"), p)
                    .absolute().top(px(2.)).size(px(15.)).rounded(px(4.)).bg(p.card)
                    .when(prefs.pin_close_left, |button| button.left(px(2.)))
                    .when(!prefs.pin_close_left, |button| button.right(px(2.)))
                    .when(!prefs.pin_close_always, |button| button.opacity(0.)
                        .group_hover("workspace-pinned-item", |style| style.opacity(1.)))
                    .on_click(cx.listener(move |app: &mut XoolFiles, _, _, cx| {
                        app.unpin_workspace_folder(index, cx);
                        cx.stop_propagation();
                    }));
                tile.child(close)
            })
            .when(!editing, |tile| tile.tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx))
            .on_click(cx.listener(move |app: &mut XoolFiles, _, window, cx| {
                app.open_workspace_location(target.clone(), window, cx);
            }))
            .on_drag(ShortcutDrag(path.clone()), move |_, _, _, cx| {
                cx.new(|_| DragGhost { label: drag_label.clone().into(), count: 1, colors, icon })
            }))
    } else {
        tile.bg(p.input)
            .child(glyph("\u{E710}", 12., p.dim))
            .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new(
                tr("Arrastra un archivo o una carpeta para fijarlo", "Drag a file or folder here to pin it")).build(window, cx))
            .on_click(cx.listener(|app: &mut XoolFiles, _, window, cx| {
                if app.settings.workspaces.icon_only_panel { app.expand_workspace_panel(cx); }
                app.workspace_ui.search.update(cx, |input, cx| input.focus(window, cx));
            }))
    };
    let owner = cx.weak_entity();
    let tile = if occupied && !editing {
        let default_icon = if file { path.map_or("\u{E8A5}", |path| workspace_file_glyph(path)) } else { "\u{E8B7}" };
        tile.context_menu(move |menu, window, cx| {
            let menu = access_context_options(menu.animate_open(), owner.clone(), AccessTarget::Pin(index), default_icon, window, cx);
            let owner = owner.clone();
            let label = if file { tr("Desfijar archivo", "Unpin file") } else { tr("Desfijar carpeta", "Unpin folder") };
            menu.separator().item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                let _ = owner.update(cx, |app, cx| app.unpin_workspace_folder(index, cx));
            }))
        }).into_any_element()
    } else { tile.into_any_element() };
    let slot = div().id(("workspace-pin-slot", index)).size(px(size)).flex_shrink_0();
    if let Some((version, _)) = app.workspace_ui.pin_motion.filter(|(_, slot)| *slot == index) {
        slot.child(div().size_full().child(tile).with_animation(("workspace-pin-arrival", version),
            Animation::new(Duration::from_millis(190)).with_easing(ease_out_quint()),
            |tile, progress| tile.relative().top(px((1. - progress) * 9.)).opacity(0.45 + progress * 0.55)))
            .into_any_element()
    } else { slot.child(tile).into_any_element() }
}

fn workspace_location_icon(app: &XoolFiles, path: &std::path::Path, fallback: &'static str, size: f32, color: Rgba) -> AnyElement {
    let style = app.settings.workspaces.current().access_styles.get(path);
    let symbol = if style.is_none_or(|style| style.icon.is_none() && style.color.is_none())
        && let Some(icon) = app.workspace_ui.system_icon(path)
    {
        img(icon).size_full().object_fit(ObjectFit::Contain).into_any_element()
    } else {
        styled_workspace_glyph(style, fallback, size, color).into_any_element()
    };
    div().flex().justify_center().items_center().size(px(size + 2.)).flex_shrink_0().child(symbol)
        .into_any_element()
}

fn workspace_file_glyph(path: &std::path::Path) -> &'static str {
    let extension = path.extension().and_then(|value| value.to_str()).unwrap_or_default();
    if ["png", "jpg", "jpeg", "gif", "webp", "bmp", "tiff", "svg", "ico"].iter().any(|kind| extension.eq_ignore_ascii_case(kind)) {
        "\u{EB9F}"
    } else {
        "\u{E8A5}"
    }
}

fn styled_workspace_glyph(style: Option<&AccessStyle>, fallback: &'static str, size: f32, color: Rgba) -> Div {
    let icon = style.and_then(|style| style.icon).map_or(fallback, WorkspaceIcon::glyph);
    let color = style.and_then(|style| style.color).map_or(color, |accent| rgb(accent.rgb()));
    glyph(icon, size, color)
}

fn folder_matches(app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    let ui = &app.workspace_ui;
    let mut list = div().w_full().flex().flex_col().gap(px(3.)).flex_shrink_0();
    if ui.searching || ui.search_failed || ui.results.is_empty() {
        let message = if ui.searching { tr("Buscando carpetas…", "Finding folders…") }
            else if ui.search_failed { tr("El índice no está disponible.", "The index is unavailable.") }
            else { tr("No se encontraron carpetas.", "No folders found.") };
        list = list.child(div().p(px(5.)).text_size(px(11.)).text_color(p.muted).child(message));
    }
    for (index, path) in ui.results.iter().enumerate() {
        let open_path = path.clone();
        let pin_path = path.clone();
        list = list.child(div().w_full().flex().items_center().rounded(px(8.)).bg(p.card)
            .child(div().id(("workspace-result", index)).flex_1().min_w(px(0.)).flex().items_center()
                .gap(px(6.)).p(px(5.)).cursor_pointer().hover(|row| row.bg(p.card_hover))
                .child(workspace_location_icon(app, path, "\u{E8B7}", 13., p.accent))
                .child(div().flex_1().min_w(px(0.)).flex().flex_col().gap(px(1.))
                    .child(div().truncate().text_size(px(11.)).child(folder_name(path)))
                    .child(div().truncate().text_size(px(9.)).text_color(p.muted).child(path.to_string_lossy().into_owned())))
                .on_click(cx.listener(move |app: &mut XoolFiles, _, window, cx| {
                    app.add_workspace_folder(open_path.clone(), window, cx);
                })))
            .child(icon_button(("workspace-pin-result", index), "\u{E718}", tr("Fijar carpeta", "Pin folder"), p)
                .on_click(cx.listener(move |app: &mut XoolFiles, _, _, cx| {
                    app.pin_workspace_folder(pin_path.clone(), None, cx);
                }))));
    }
    FolderDropTarget::List(app.settings.workspaces.current().rows.len())
        .accept(list.id("workspace-search-result-list"), p, cx).into_any_element()
}

fn workspace_rows(app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    let prefs = &app.settings.workspaces;
    let space = prefs.current();
    let editing = app.workspace_ui.edit;
    let insertion = match editing {
        Some(NameEdit::Divider(index) | NameEdit::Group(index) | NameEdit::Web(index)) => Some(index),
        _ => None,
    };
    let stride = prefs.row_density.stride();
    let mut list = div().id("workspace-rows").w_full().flex().flex_col().flex_shrink_0();
    for index in space.row_order(prefs.access_order, None) {
        let row = &space.rows[index];
        list = list.child(workspace_row(app, index, row, p, cx));
        if let Item::Group { collapsed, .. } = row.item {
            let progress = app.workspace_ui.group_progress(row.id, collapsed);
            if progress > 0. {
                let children = space.row_order(prefs.access_order, Some(row.id));
                list = list.child(div().id(("workspace-group-children", row.id))
                    .relative().top(px(app.workspace_ui.row_offset(row.id)))
                    .flex().flex_col().flex_shrink_0().overflow_hidden()
                    .h(px(stride * children.len() as f32 * progress))
                    .when(!prefs.icon_only_panel, |list| list.pl(px(12.)))
                    .child(div().flex().flex_col().flex_shrink_0()
                        .relative().top(px(-5. * (1. - progress)))
                        .children(children.into_iter().map(|index| workspace_row(app, index, &space.rows[index], p, cx)))));
            }
        }
        if insertion == Some(index + 1) && index + 1 != space.rows.len() {
            list = list.child(name_editor(app, p, cx));
        }
    }
    if insertion == Some(space.rows.len()) {
        list = list.child(name_editor(app, p, cx));
    }
    if space.rows.is_empty() && !prefs.icon_only_panel && insertion.is_none() {
        list = list.child(FolderDropTarget::List(0).accept(
            div().id("workspace-empty-list").px(px(4.)).py(px(8.)).text_size(px(11.)).text_color(p.muted)
                .child(tr("Arrastra archivos o carpetas para añadirlos.", "Drag files or folders here to add them.")),
            p, cx));
    }
    list.into_any_element()
}

fn workspace_row(app: &XoolFiles, index: usize, row: &Row, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    let id = row.id;
    let colors = *p;
    let prefs = &app.settings.workspaces;
    let icons_only = prefs.icon_only_panel;
    let editing = app.workspace_ui.edit;
    let editing_row = matches!(editing, Some(NameEdit::RenameDivider(item) | NameEdit::RenameGroup(item)
        | NameEdit::RenameAccess(AccessTarget::Row(item))) if item == id);
    let movable = editing.is_none();
    let stride = prefs.row_density.stride();
    let active = prefs.highlight_open_folder && matches!(&row.item,
        Item::Folder(path) if crate::workspaces::same_path(&app.active_tab().path, path));
    let (label, icon) = match &row.item {
        Item::Folder(path) => (prefs.current().access_name(path), "\u{E8B7}"),
        Item::File(path) => (prefs.current().access_name(path), workspace_file_glyph(path)),
        Item::Web(url) => (crate::workspaces::web_shortcut_title(url), "\u{E774}"),
        Item::Divider(name) => (name.clone(), "\u{E9E9}"),
        Item::Group { name, .. } => (name.clone(), "\u{E8B7}"),
    };
    let reorder_label = label.clone();
    let group = matches!(row.item, Item::Group { .. });
    let divider = matches!(row.item, Item::Divider(_));
    let alignment = prefs.divider_alignment;
    let line = || div().flex_1().h(px(1.)).bg(p.border_soft);
    let body = match &row.item {
        Item::Divider(_) if editing_row => name_editor(app, p, cx),
        Item::Divider(name) if icons_only => {
            let name = name.clone();
            div().id(("workspace-compact-divider", id)).flex_1().h_full().flex().items_center().px(px(3.))
                .child(line())
                .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(name.clone()).build(window, cx))
                .into_any_element()
        }
        Item::Divider(name) if name.is_empty() => line().into_any_element(),
        Item::Divider(_) => div().flex_1().min_w(px(0.)).flex().items_center().gap(px(7.))
            .when(alignment != DividerAlignment::Left, |row| row.child(line()))
            .child(div().max_w(gpui::relative(0.72)).truncate().text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD).text_color(p.muted).child(label))
            .when(alignment != DividerAlignment::Right, |row| row.child(line())).into_any_element(),
        Item::Folder(path) | Item::File(path) => {
            let target = path.clone();
            let hint = format!("{}\n{}", label, path.display());
            div().id(("workspace-folder", id)).flex_1().min_w(px(0.)).h_full()
                .flex().items_center().gap(px(6.))
                .child(workspace_location_icon(app, path, icon, 13., p.accent))
                .when(icons_only, |view| view.justify_center().tooltip(move |window, cx|
                    gpui_component::tooltip::Tooltip::new(hint.clone()).build(window, cx)))
                .when(editing_row, |view| view.child(div().flex_1().min_w(px(0.)).child(name_editor(app, p, cx))))
                .when(!icons_only && !editing_row, |view| view.child(div().flex_1().min_w(px(0.)).truncate().text_size(px(12.)).child(label)))
                .when(!editing_row, |view| view.cursor_pointer()
                    .on_click(cx.listener(move |app: &mut XoolFiles, _, window, cx| {
                        app.open_workspace_location(target.clone(), window, cx);
                    }))).into_any_element()
        }
        Item::Web(url) => {
            let target = url.clone();
            let tooltip = url.clone();
            div().id(("workspace-web", id)).flex_1().min_w(px(0.)).h_full()
                .flex().items_center().gap(px(6.)).cursor_pointer()
                .when(icons_only, |view| view.justify_center())
                .child(glyph(icon, 13., p.accent))
                .when(!icons_only, |view| view.child(div().flex_1().min_w(px(0.)).truncate().text_size(px(12.)).child(label)))
                .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx))
                .on_click(cx.listener(move |app: &mut XoolFiles, _, _, cx| app.open_workspace_url(&target, cx)))
                .into_any_element()
        }
        Item::Group { collapsed, style, .. } => {
            let hint = label.clone();
            div().id(("workspace-group", id)).flex_1().min_w(px(0.)).h_full().flex().items_center().gap(px(5.))
                .when(icons_only, |view| view.justify_center())
                .child(styled_workspace_glyph(Some(style), if *collapsed { "\u{E8B7}" } else { "\u{E838}" }, 13., p.accent))
                .when(editing_row, |view| view.child(div().flex_1().min_w(px(0.)).child(name_editor(app, p, cx))))
                .when(!icons_only && !editing_row, |view| view
                    .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).child(label))
                    .child(glyph(if *collapsed { "\u{E76C}" } else { "\u{E70D}" }, 9., p.muted)))
                .when(!editing_row, |view| view.cursor_pointer()
                    .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(hint.clone()).build(window, cx))
                    .on_click(cx.listener(move |app: &mut XoolFiles, _, _, cx| app.toggle_workspace_group(id, cx))))
                .into_any_element()
        }
    };
    let owner = cx.weak_entity();
    let menu_item = row.item.clone();
    let is_root = row.parent.is_none();
    let path = row.item.path().map(PathBuf::from);
    let drop_target = FolderDropTarget::Row(if group { RowDestination::Group(id) } else { RowDestination::After(id) });
    let drop_mark = app.workspace_ui.row_target;
    let parent_offset = row.parent.map_or(0., |parent| app.workspace_ui.row_offset(parent));
    let close_label = if group { tr("Quitar carpeta desplegable", "Remove collapsible folder") }
        else { tr("Quitar del espacio", "Remove from workspace") };
    let close = (!divider && !editing_row && prefs.show_pin_close).then(|| {
        icon_button(("workspace-remove-row", id), "icons/tab-close.svg", close_label, p)
            .size(px(16.)).rounded(px(4.))
            .when(!prefs.pin_close_always, |button| button.opacity(0.)
                .group_hover("workspace-list-item", |style| style.opacity(1.)))
            .on_click(cx.listener(move |app: &mut XoolFiles, _, _, cx| {
                app.remove_workspace_row(id, cx);
                cx.stop_propagation();
            }))
    });
    let (left_close, right_close) = if prefs.pin_close_left { (close, None) } else { (None, close) };
    let view = div().id(("workspace-row", id)).group("workspace-list-item").relative()
        .top(px(app.workspace_ui.row_offset(id) - parent_offset))
        .flex().items_center().size_full().gap(px(3.)).px(px(4.)).rounded(px(6.)).bg(p.sidebar)
        .when(app.workspace_ui.dragged_row == Some(id), |row| row.opacity(0.24))
        .when(active || drop_mark == Some(RowDestination::Group(id)), |row| row.bg(p.accent_faint))
        .hover(|row| row.bg(p.card_hover)).children(left_close).child(body).children(right_close);
    let view = if editing_row { view.into_any_element() } else {
        drop_target.accept(view, p, cx).context_menu(move |menu, window, cx| {
            let action_owner = owner.clone();
            let menu = menu.animate_open();
            let menu = match &menu_item {
                Item::Divider(_) => menu.item(PopupMenuItem::new(tr("Cambiar nombre", "Rename divider")).on_click(move |_, window, cx| {
                    let _ = action_owner.update(cx, |app, cx| app.edit_workspace_name(NameEdit::RenameDivider(id), window, cx));
                })),
                Item::Group { .. } => {
                    let menu = menu.item(PopupMenuItem::new(tr("Renombrar carpeta", "Rename folder")).on_click(move |_, window, cx| {
                        let _ = action_owner.update(cx, |app, cx| app.edit_workspace_name(NameEdit::RenameGroup(id), window, cx));
                    }));
                    icon_context_options(menu, owner.clone(), AccessTarget::Row(id), "\u{E8B7}", window, cx)
                }
                Item::Web(url) => {
                    let url = url.clone();
                    menu.item(PopupMenuItem::new(tr("Abrir en el navegador", "Open in browser")).on_click(move |_, _, cx| {
                        let _ = action_owner.update(cx, |app, cx| app.open_workspace_url(&url, cx));
                    }))
                }
                Item::Folder(path) | Item::File(path) => {
                    let path = path.clone();
                    let file = matches!(&menu_item, Item::File(_));
                    let label = if file { tr("Fijar archivo", "Pin file") } else { tr("Fijar carpeta", "Pin folder") };
                    access_context_options(menu, owner.clone(), AccessTarget::Row(id), icon, window, cx)
                        .separator().item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                            let _ = action_owner.update(cx, |app, cx| app.pin_workspace_shortcuts([path.clone()], None, true, cx));
                        }))
                }
            };
            let menu = if is_root {
                let add_owner = owner.clone();
                menu.item(PopupMenuItem::new(tr("Agregar división debajo", "Add divider below")).on_click(move |_, window, cx| {
                    let _ = add_owner.update(cx, |app, cx| app.edit_workspace_name(NameEdit::Divider(index + 1), window, cx));
                }))
            } else { menu };
            let remove_owner = owner.clone();
            let label = if group { tr("Quitar carpeta desplegable", "Remove collapsible folder") }
                else if divider { tr("Quitar división", "Remove divider") }
                else { tr("Quitar del espacio", "Remove from workspace") };
            menu.separator().item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                let _ = remove_owner.update(cx, |app, cx| app.remove_workspace_row(id, cx));
            }))
        }).into_any_element()
    };
    let view = if app.workspace_ui.row_arrival == Some(id) {
        div().size_full().child(view).with_animation(("workspace-row-enter", id),
            Animation::new(Duration::from_millis(170)).with_easing(ease_out_quint()),
            |row, progress| row.relative().top(px((1. - progress) * -7.)).opacity(progress))
            .into_any_element()
    } else { view };
    div().id(("workspace-row-slot", id)).relative().h(px(stride)).py(px(ROW_GAP / 2.))
        .flex_shrink_0().child(view)
        .when(matches!(drop_mark, Some(RowDestination::Before(target) | RowDestination::After(target)) if target == id),
            |slot| slot.child(div().absolute().left(px(1.)).right(px(1.)).h(px(1.)).bg(p.accent)
                .when(drop_mark == Some(RowDestination::Before(id)), |line| line.top(px(0.)))
                .when(drop_mark == Some(RowDestination::After(id)), |line| line.bottom(px(-1.)))))
        .when(movable, |slot| slot
            .on_drag(RowDrag { id, path, can_group: row.item.is_shortcut() }, move |_, _, _, cx| {
                cx.new(|_| DragGhost { label: reorder_label.clone().into(), count: 1, colors, icon })
            })
            .on_drag_move::<RowDrag>(cx.listener(move |app: &mut XoolFiles, event: &gpui::DragMoveEvent<RowDrag>, _, cx| {
                let position = event.event.position;
                if event.bounds.contains(&position) && app.workspace_ui.scroll.bounds().contains(&position) {
                    let drag = event.drag(cx);
                    let fraction = f32::from(position.y - event.bounds.top()) / stride;
                    let destination = if group && drag.can_group && (0.22..=0.78).contains(&fraction) {
                        RowDestination::Group(id)
                    } else if fraction < 0.5 { RowDestination::Before(id) }
                    else { RowDestination::After(id) };
                    app.preview_workspace_drop(drag.id, destination, event.bounds, cx);
                }
            }))
            .on_drop(cx.listener(move |app: &mut XoolFiles, drag: &RowDrag, _, cx| {
                let destination = match app.workspace_ui.row_target {
                    Some(destination @ (RowDestination::Before(target) | RowDestination::After(target) | RowDestination::Group(target)))
                        if target == id => destination,
                    _ => RowDestination::After(id),
                };
                app.move_workspace_row(drag.id, destination, cx);
                cx.stop_propagation();
            })))
        .into_any_element()
}
