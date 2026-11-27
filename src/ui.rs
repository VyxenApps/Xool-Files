//! Interfaz de Xool Files: todo dibujado con GPUI.

use crate::i18n::tr;
use std::cell::{Cell, OnceCell};
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt as _, AnyElement, Bounds, ClickEvent, Context, CursorStyle,
    DispatchPhase, Div, ElementId, Entity, FontWeight, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, NavigationDirection, ObjectFit, Pixels, RenderImage, Rgba,
    ScrollDelta, ScrollWheelEvent, SharedString, Window, WindowControlArea, canvas, div,
    ease_out_quint, img, px, rgb, rgba,
};
use gpui_component::input::{Input, InputState};

use crate::app::{
    self, ContextPage, Dialog, DragFiles, DragTab, MenuAction, MenuEntry, MenuRow, MenuTarget, QuickAction, TabDropZone,
    ToastTone, XoolFiles, breadcrumb_parts, folder_name,
};
use crate::fs_model::{self, Entry, FileKind, SortKey};
use crate::media::{AudioPlayback, VideoPlayer};
use crate::quick_search::{QuickResult, QuickSearch};
use crate::pane_layout::{Axis, DIVIDER, PaneLayout, split_extents};
use crate::platform::{self, DriveInfo};
use crate::settings::{AudioVisualization, ContextMenuStyle, DETAIL_COLUMNS, DetailColumn, DetailLabel, Settings, SidebarGroup, ThemeMode, ViewMode};
use crate::shell_menu::{ShellAction, ShellRow};
use crate::sounds::{ActionSound, SoundAction};
use crate::theme::{self, Palette as Colors};
use crate::thumbs::PreviewContent;

#[path = "workspace_panel.rs"]
mod workspace_panel;

const PANE_TABBAR_HEIGHT: f32 = 34.0;
const TITLEBAR_LEFT_PADDING: f32 = 5.;
const TITLEBAR_RIGHT_PADDING: f32 = 7.;
const TITLEBAR_PANEL_BUTTON: f32 = 28.;
const TITLEBAR_WORKSPACE_SLOT: f32 = 31.;
const TITLEBAR_DRAG_SPACER: f32 = 28.;
const TITLEBAR_HISTORY_BUTTON: f32 = 30.;
const TITLEBAR_HISTORY_GAP: f32 = 4.;
const WINDOW_CONTROL_WIDTH: f32 = 46.;
pub(crate) const TITLEBAR_HEIGHT: f32 = 40.0;
pub(crate) const TOOLBAR_HEIGHT: f32 = 52.0;
const GRID_SPACING: f32 = 5.0;
const COMMAND_BAR_HEIGHT: f32 = 32.0;
pub(crate) const STATUSBAR_HEIGHT: f32 = 34.0;

const UI_FONT: &str = "Segoe UI";
const SETTINGS_ICON: &str = "icons/settings-gear.svg";
const ICON_FONT: &str = "Segoe Fluent Icons";
const MONO_FONT: &str = "Consolas";

/// Registra un listener global de ratón para que los arrastres sigan
/// funcionando aunque el cursor salga del elemento que los inició.
fn global_drag_layer(cx: &mut Context<XoolFiles>) -> AnyElement {
    let entity = cx.entity().downgrade();
    canvas(
        |_, _, _| {},
        move |_, _, window, _cx| {
            let down_entity = entity.clone();
            window.on_mouse_event(move |_: &MouseDownEvent, phase, _, cx| {
                if phase == DispatchPhase::Capture {
                    let _ = down_entity.update(cx, XoolFiles::cancel_pending_context_menu);
                }
            });

            let move_entity = entity.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture {
                    let _ = move_entity.update(cx, |this, cx| {
                        crate::marquee::update(this, event, cx);
                        this.drag_image_resize(event, cx);
                        this.drag_audio_seek(event.position.x, cx);
                        this.drag_media_volume(event.position.x, cx);
                    });
                    return;
                }
                let _ = move_entity.update(cx, |this, cx| {
                    if let Some((start, width)) = this.settings_nav_drag {
                        this.settings.settings_nav_width = (width + f32::from(event.position.x) - start).clamp(118., 256.);
                        cx.notify();
                    }
                    this.drag_sidebar(event, window, cx);
                    this.drag_workspace_resize(event, window, cx);
                    this.drag_preview(event, window, cx);
                    this.drag_split(event, cx);
                    this.drag_column(event, cx);
                    this.drag_video_seek(event, cx);
                });
            });

            let up_entity = entity.clone();
            window.on_mouse_event(move |event: &MouseUpEvent, phase, _window, cx| {
                if phase == DispatchPhase::Capture {
                    if event.button == MouseButton::Left {
                        let _ = up_entity.update(cx, |this, cx| {
                            if let Some(controls) = this.image_resize_controls_mut() {
                                controls.drag = None;
                            }
                            this.end_audio_seek_drag(Some(event.position.x), cx);
                            this.drag_media_volume(event.position.x, cx);
                            this.end_media_volume_drag(cx);
                            if crate::marquee::finish(this, cx) { cx.stop_propagation(); }
                        });
                    }
                    return;
                }
                let _ = up_entity.update(cx, |this, cx| {
                    if event.button == MouseButton::Left && this.settings_nav_drag.take().is_some() {
                        this.settings.save();
                        cx.notify();
                    }
                    this.end_sidebar_resize(cx);
                    if event.button == MouseButton::Left {
                        this.end_workspace_resize(cx);
                    }
                    this.end_preview_resize(cx);
                    this.end_split_resize(cx);
                    this.end_column_resize(cx);
                    this.end_video_seek_drag(cx);
                    if event.button == MouseButton::Left {
                        let reorder_pending = this.sidebar_group_target.is_some() || this.sidebar_favorite_target.is_some() || this.menu_tool_target.is_some() || this.tab_reorder_target.is_some() || this.column_drag_target.is_some();
                        this.finish_drag();
                        if reorder_pending { cx.notify(); }
                    }
                });
            });

            // La rueda con Shift o Ctrl cambia el zoom de la cuadrícula. Se
            // atiende en la fase de captura para que la lista con scroll no
            // consuma el evento ni desplace la carpeta.
            let wheel_entity = entity.clone();
            window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture { return; }
                if !event.modifiers.shift && !event.modifiers.control {
                    let _ = wheel_entity.update(cx, |this, cx| {
                        let nav_axis = settings_navigation_axis(f32::from(window.viewport_size().width));
                        if !cx.has_active_drag() && this.scroll_wheel_smoothly(event, window.line_height(), nav_axis) {
                            cx.notify();
                            cx.stop_propagation();
                        }
                    });
                    return;
                }
                let _ = wheel_entity.update(cx, |this, cx| {
                    if this.dialog.is_some()
                        || this.context_menu.is_some()
                        || this.sort_menu
                        || this.view_menu
                        || this.address_history_open
                    {
                        return;
                    }
                    let (left, width) = this.files_area(window);
                    let x = f32::from(event.position.x);
                    let y = f32::from(event.position.y);
                    let (above, below) = status_bar_insets(this);
                    let top = titlebar_height(this, window) + above + TOOLBAR_HEIGHT + command_bar_height(this);
                    let bottom = f32::from(window.viewport_size().height) - below;
                    if x < left || x > left + width || y < top || y > bottom {
                        return;
                    }
                    // En Windows, Shift+rueda llega como desplazamiento
                    // horizontal (x), por eso se mira el eje con más valor.
                    let delta = match event.delta {
                        ScrollDelta::Lines(point) => {
                            if point.y.abs() >= point.x.abs() {
                                point.y
                            } else {
                                point.x
                            }
                        }
                        ScrollDelta::Pixels(point) => {
                            let dy = f32::from(point.y);
                            let dx = f32::from(point.x);
                            let value = if dy.abs() >= dx.abs() { dy } else { dx };
                            value / 60.0
                        }
                    };
                    if delta.abs() < 0.01 {
                        return;
                    }
                    this.step_zoom(delta > 0.0, cx);
                    cx.stop_propagation();
                });
            });
        },
    )
    .absolute()
    .top(px(0.))
    .left(px(0.))
    .w(px(1.))
    .h(px(1.))
    .into_any_element()
}

// ---------------------------------------------------------------------------
// Render principal
// ---------------------------------------------------------------------------

fn status_bar_insets(app: &XoolFiles) -> (f32, f32) {
    if app.settings.status_bar_above { (STATUSBAR_HEIGHT, 0.) } else { (0., STATUSBAR_HEIGHT) }
}

fn titlebar_height(app: &XoolFiles, window: &Window) -> f32 {
    if app.settings.flush_tabs_when_maximized && window.is_maximized() { TAB_HEIGHT } else { TITLEBAR_HEIGHT }
}

pub fn render(
    app: &mut XoolFiles,
    window: &mut Window,
    cx: &mut Context<XoolFiles>,
) -> impl IntoElement {
    let p = theme::palette(&app.settings);
    let translucent = app.settings.translucent_background;
    let background_image = app.background_image.clone().filter(|_| translucent);
    let tint = Rgba { a: f32::from(app.settings.background_tint) / 100., ..p.bg };
    let frame = if translucent {
        Colors {
            titlebar: Rgba { a: 0.42, ..p.titlebar },
            toolbar: Rgba { a: 0.26, ..p.toolbar },
            sidebar: Rgba { a: 0.32, ..p.sidebar },
            ..p
        }
    } else { p };
    app.sync_watcher();
    let viewport = window.viewport_size();
    let vw = f32::from(viewport.width);
    let vh = f32::from(viewport.height);

    let sidebar_w = app.sidebar_extent(vw);
    let workspace_w = app.workspace_extent(vw);
    let show_sidebar = sidebar_w > 0.0;
    let show_preview = app.preview_visible_at(vw);
    // Los paneles usan un ancho explícito para que el divisor central pueda
    // arrastrarse y la proporción se conserve entre sesiones.
    let (files_left, files_w) = app.files_area(window);
    let title_h = titlebar_height(app, window);
    let (above, below) = status_bar_insets(app);
    let toolbar_bottom = title_h + above + TOOLBAR_HEIGHT;
    let panes_h = (vh - toolbar_bottom - command_bar_height(app) - below).max(0.0);

    let focus = app.focus_handle.clone();

    let mut root = div()
        .id("xool-root")
        .relative()
        .flex()
        .flex_col()
        .w_full()
        .h_full()
        .bg(if translucent && background_image.is_none() { tint } else { p.bg })
        .text_color(p.text)
        .font_family(UI_FONT)
        .key_context("XoolFiles")
        .track_focus(&focus)
        .on_key_down(cx.listener(XoolFiles::on_key_down))
        .on_mouse_down(
            MouseButton::Navigate(NavigationDirection::Back),
            cx.listener(XoolFiles::on_mouse_back),
        )
        .on_mouse_down(
            MouseButton::Navigate(NavigationDirection::Forward),
            cx.listener(XoolFiles::on_mouse_forward),
        )
        .on_action(cx.listener(XoolFiles::on_new_tab))
        .on_action(cx.listener(XoolFiles::on_close_tab))
        .on_action(cx.listener(XoolFiles::on_next_tab))
        .on_action(cx.listener(XoolFiles::on_prev_tab))
        .on_action(cx.listener(XoolFiles::on_back))
        .on_action(cx.listener(XoolFiles::on_forward))
        .on_action(cx.listener(XoolFiles::on_up))
        .on_action(cx.listener(XoolFiles::on_refresh))
        .on_action(cx.listener(XoolFiles::on_focus_address))
        .on_action(cx.listener(XoolFiles::on_focus_search))
        .on_action(cx.listener(XoolFiles::on_toggle_recursive_search))
        .on_action(cx.listener(XoolFiles::on_toggle_hidden))
        .on_action(cx.listener(XoolFiles::on_toggle_split))
        .on_action(cx.listener(XoolFiles::on_toggle_preview))
        .on_action(cx.listener(XoolFiles::on_toggle_sidebar))
        .on_action(cx.listener(XoolFiles::on_new_folder))
        .on_action(cx.listener(XoolFiles::on_rename))
        .on_action(cx.listener(XoolFiles::on_delete))
        .on_action(cx.listener(XoolFiles::on_copy))
        .on_action(cx.listener(XoolFiles::on_cut))
        .on_action(cx.listener(XoolFiles::on_paste))
        .on_action(cx.listener(XoolFiles::on_open))
        .on_action(cx.listener(XoolFiles::on_select_all))
        .on_action(cx.listener(XoolFiles::on_settings))
        .on_action(cx.listener(XoolFiles::on_toggle_view))
        .on_action(cx.listener(XoolFiles::on_favorite))
        .on_action(cx.listener(XoolFiles::on_zoom_in))
        .on_action(cx.listener(XoolFiles::on_zoom_out))
        .on_action(cx.listener(XoolFiles::on_zoom_reset))
        .on_action(cx.listener(XoolFiles::on_escape))
        .on_action(cx.listener(XoolFiles::on_sort_name))
        .on_action(cx.listener(XoolFiles::on_sort_size))
        .on_action(cx.listener(XoolFiles::on_sort_kind))
        .on_action(cx.listener(XoolFiles::on_sort_modified))
        .child(global_drag_layer(cx))
        .when_some(background_image, |root, image| root.child(
            div().absolute().top(px(0.)).left(px(0.)).size_full().overflow_hidden()
                .child(img(image).size_full().object_fit(ObjectFit::Cover))
                .child(div().absolute().top(px(0.)).left(px(0.)).size_full().bg(tint))))
        .child(titlebar(app, files_left, files_w, &frame, window, cx))
        .when(app.settings.status_bar_above, |root| root.child(status_bar(app, &frame, vw, cx)));

    let mut body = div()
        .flex()
        .flex_1()
        .min_h(px(0.))
        .w_full()
        .overflow_hidden();

    if workspace_w > 0. && !app.settings.workspaces.right {
        body = body.child(animated_workspace_panel(app, workspace_w, vw, &frame, cx));
    }

    if show_sidebar && !app.settings.sidebar_right {
        body = body.child(animated_sidebar(app, sidebar_w, &frame, cx));
    }

    let mut content = div().flex().flex_col().flex_1().min_w(px(0.)).min_h(px(0.));

    content = content.child(toolbar(app, &frame, (vw - sidebar_w - workspace_w).max(0.), window, cx));

    if app.settings.command_bar {
        content = content.child(command_bar(app, &p, cx));
    }
    let panes_only = pane_layout_view(app, &app.pane_layout, files_w, panes_h, &mut Vec::new(),
        false, true, &frame, cx);
    let mut panes_area = div().flex().flex_1().min_h(px(0.)).w_full();
    if show_preview {
        let left = app.settings.preview_left;
        let preview_colors = if translucent { Colors { panel: Rgba { a: 0.38, ..p.panel }, ..p } } else { p };
        let preview = div().h_full().flex_shrink_0().child(preview_panel(app, &preview_colors, cx)).with_animation(
            ("preview-panel-in", left as usize),
            Animation::new(Duration::from_millis(180)).with_easing(ease_out_quint()),
            move |element, progress| {
                let offset = if left { -18.0 } else { 18.0 };
                element.relative().left(px((1.0 - progress) * offset)).opacity(progress)
            },
        );
        let resizer = preview_resizer(app, &p, cx);
        panes_area = if left {
            panes_area.child(preview).child(resizer).child(panes_only)
        } else {
            panes_area.child(panes_only).child(resizer).child(preview)
        };
    } else {
        panes_area = panes_area.child(panes_only);
    }

    content = content.child(panes_area);
    body = body.child(content);

    if show_sidebar && app.settings.sidebar_right {
        body = body.child(animated_sidebar(app, sidebar_w, &frame, cx));
    }
    if workspace_w > 0. && app.settings.workspaces.right {
        body = body.child(animated_workspace_panel(app, workspace_w, vw, &frame, cx));
    }
    root = root.child(body);
    if !app.settings.status_bar_above {
        root = root.child(status_bar(app, &frame, vw, cx));
    }

    if let Some(bounds) = app.marquee.as_ref().and_then(|drag| drag.bounds) {
        root = root.child(canvas(|_, _, _| {}, move |_, _, window, _| {
            window.paint_quad(gpui::quad(bounds, gpui::Corners::all(px(5.)), p.accent_faint,
                gpui::Edges::all(px(1.)), p.accent, gpui::BorderStyle::Solid));
        }).absolute().size_full());
    }

    // Menús flotantes.
    if app.recently_closed_open && app.dialog.is_none() {
        root = root.child(div().id("closed-tabs-dismiss").absolute().size_full().occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(|this: &mut XoolFiles, _, _, cx| {
                this.recently_closed_open = false;
                cx.notify();
                cx.stop_propagation();
            })));
        root = root.child(recently_closed_popup(app, &p, vw, vh, title_h, cx));
    } else if let Some((path, column, x, y)) = &app.detail_label_menu {
        root = root.child(detail_label_popup(app, path, *column, (*x, *y), (vw, vh), &p, cx));
    } else if let Some((x, y)) = app.columns_menu {
        root = root.child(columns_popup(app, x, y, vw, vh, &p, cx));
    } else if let Some(menu) = &app.context_menu {
        if let Some(system_rows) = menu.system_rows.as_deref() {
            root = root.child(context_menu(app, &p, menu, system_rows, vw, vh, window, cx));
        }
    } else if app.sort_menu {
        root = root.child(menu_backdrop(&p, cx));
        root = root.child(sort_menu(app, &p, toolbar_bottom, cx));
    } else if app.view_menu {
        root = root.child(menu_backdrop(&p, cx));
        root = root.child(view_menu_popup(app, &p, vw, vh, title_h, cx));
    } else if app.address_history_open {
        let popup_left = if show_sidebar && !app.settings.sidebar_right {
            sidebar_w + 139.0
        } else {
            143.0
        };
        let (popup_left, popup_w) = app.address_bounds
            .map(|b| (f32::from(b.origin.x), f32::from(b.size.width)))
            .unwrap_or((popup_left, (vw - popup_left - 360.0).max(140.0)));
        root = root.child(address_history_backdrop(toolbar_bottom, cx));
        root = root.child(address_history_popup(app, &p, popup_left, popup_w, toolbar_bottom, cx));
    }

    if app.dialog.is_some() {
        root = root.child(dialog_overlay(app, &p, vw, vh, cx));
    }

    if let Some(toast) = app.toast.as_ref() {
        root = root.child(toast_view(toast, app.toast_token, vw, below + 12., &p));
    }

    gpui_component::window_border().child(root)
}

// ---------------------------------------------------------------------------
// Barra de título
// ---------------------------------------------------------------------------

fn titlebar(app: &XoolFiles, files_left: f32, files_w: f32, p: &Colors, window: &Window, cx: &mut Context<XoolFiles>) -> AnyElement {
    let is_max = window.is_maximized();
    let can_drag = app.dialog.is_none() && app.context_menu.is_none();
    let (sidebar_icon, sidebar_hint) = if app.settings.sidebar_visible {
        ("icons/titlebar-sidebar.svg", tr("Ocultar barra lateral", "Hide sidebar"))
    } else {
        ("icons/titlebar-sidebar-hidden.svg", tr("Mostrar barra lateral", "Show sidebar"))
    };
    let workspace_visible = app.settings.workspaces.enabled;
    let button_progress = app.workspace_ui.button_progress();
    let accent_progress = app.workspace_ui.panel_progress();
    let mix = |muted: f32, accent: f32| muted + (accent - muted) * accent_progress;
    let workspace_color = Rgba {
        r: mix(p.muted.r, p.accent.r), g: mix(p.muted.g, p.accent.g),
        b: mix(p.muted.b, p.accent.b), a: mix(p.muted.a, p.accent.a),
    };
    let workspace_hint = if workspace_visible {
        tr("Ocultar espacios de trabajo", "Hide workspaces")
    } else {
        tr("Mostrar espacios de trabajo", "Show workspaces")
    };
    let mut bar = div()
        .id("titlebar")
        .h(px(titlebar_height(app, window)))
        .w_full()
        .flex_shrink_0()
        .flex()
        .items_center()
        .bg(p.titlebar)
        .border_b_1()
        .border_color(p.border_soft)
        .child(div().flex().items_center().flex_shrink_0().pl(px(TITLEBAR_LEFT_PADDING)).pr(px(TITLEBAR_RIGHT_PADDING))
            .when(button_progress > 0., |group| {
                group.child(div().w(px(TITLEBAR_WORKSPACE_SLOT * button_progress)).h(px(TITLEBAR_PANEL_BUTTON)).flex_shrink_0()
                    .overflow_hidden().flex().opacity(button_progress)
                    .child(div().id("titlebar-workspace-toggle").size(px(TITLEBAR_PANEL_BUTTON)).flex_shrink_0().rounded(px(6.))
                    .flex().items_center().justify_center().cursor_pointer()
                    .text_color(workspace_color)
                    .hover(|style| style.bg(p.card_hover).text_color(p.text))
                    .on_click(cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                        this.toggle_workspace_panel(window, cx);
                    }))
                    .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(workspace_hint).build(window, cx))
                    .child(glyph_inherit("icons/settings-workspaces.svg", 16.))))
            })
            .child(div().id("titlebar-sidebar-toggle").size(px(TITLEBAR_PANEL_BUTTON)).flex_shrink_0().rounded(px(6.))
                .flex().items_center().justify_center().cursor_pointer()
                .text_color(if app.settings.sidebar_visible { p.accent } else { p.muted })
                .hover(|style| style.bg(p.card_hover).text_color(p.text))
                .on_click(cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.toggle_sidebar(cx);
                }))
                .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(sidebar_hint).build(window, cx))
                .child(glyph_inherit(sidebar_icon, 16.))));

    let groups_in_titlebar = app.pane_layout.has_split_titlebar();
    let mut tabs = div()
        .flex_1()
        .min_w(px(0.))
        .h_full()
        .flex()
        .items_center()
        .gap(px(if groups_in_titlebar { 0. } else { 2. }));

    if groups_in_titlebar {
        let strip_left = TITLEBAR_LEFT_PADDING + TITLEBAR_RIGHT_PADDING
            + TITLEBAR_PANEL_BUTTON + TITLEBAR_WORKSPACE_SLOT * button_progress;
        let controls_width = TITLEBAR_DRAG_SPACER + TITLEBAR_HISTORY_BUTTON
            + TITLEBAR_HISTORY_GAP + 3. * WINDOW_CONTROL_WIDTH;
        let strip_width = (f32::from(window.viewport_size().width) - strip_left - controls_width).max(0.);
        let groups = app.pane_layout.titlebar_groups(files_left, files_w, strip_left, strip_width);
        for (index, (pane, width)) in groups.into_iter().enumerate() {
            let divider = index != 0;
            let empty_space = div().flex_1().h_full()
                .when(divider && can_drag, |space| space.window_control_area(WindowControlArea::Drag))
                .when(!divider, |space| space.child(title_drag_area(0, can_drag)));
            tabs = tabs.child(div().id(("titlebar-pane", pane))
                .w(px(width)).h_full().min_w(px(0.)).flex_shrink_0()
                .flex().items_center().overflow_hidden().pr(px(3.))
                .when(divider, |group| group.border_l_1().border_color(p.border))
                .child(pane_tab_strip(app, pane, if divider { 8. } else { 4. }, p, cx))
                .child(empty_space));
        }
    } else {
        let first_pane = app.pane_layout.first_pane();
        let strip = pane_tab_buttons(div().id("titlebar-tab-strip")
            .min_w(px(0.)).h_full().flex().items_center()
            .gap(px(if app.settings.flush_tabs_when_maximized && is_max { 0. } else { TAB_GAP })).flex_shrink()
            .overflow_x_scroll()
            .track_scroll(&app.panes[first_pane].tab_scroll)
            .on_scroll_wheel(tab_wheel_listener(first_pane, cx)), app, first_pane, p, cx);
        tabs = tabs.child(strip);

        tabs = tabs.child(
            div().id("top-tab-new").w(px(25.)).h(px(25.)).flex_shrink_0().ml(px(2.))
                .flex().items_center().justify_center().rounded(px(6.)).text_color(p.dim)
                .hover(|style| style.bg(p.card_hover).text_color(p.text)).cursor_pointer()
                .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                    if this.active_pane != first_pane { this.activate_pane(first_pane, window, cx); }
                    this.new_tab(window, cx);
                }))
                .child(glyph("\u{E710}", 10., p.dim)),
        );

        tabs = tabs.child(
            div().id("titlebar-tabs-space").flex_1().h_full().child(title_drag_area(0, can_drag)),
        );
    }

    bar = bar.child(tabs);
    bar = bar
        .child(
            div()
                .id("titlebar-spacer")
                .w(px(TITLEBAR_DRAG_SPACER))
                .flex_shrink_0()
                .h_full()
                .child(title_drag_area(1, can_drag)),
        )
        .child(icon_button("closed-tabs-toggle", "\u{E70D}", 11., p,
            app.recently_closed_open, true,
            cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                this.close_context_menu(cx);
                this.columns_menu = None;
                this.sort_menu = false;
                this.view_menu = false;
                this.address_history_open = false;
                this.recently_closed_open = !this.recently_closed_open;
                this.focus_root(window);
                cx.notify();
            }))
            .w(px(TITLEBAR_HISTORY_BUTTON)).mr(px(TITLEBAR_HISTORY_GAP))
            .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new(tr("Pestañas cerradas recientemente", "Recently closed tabs")).build(window, cx)))
        .child(window_control(
            "win-min",
            "\u{E921}",
            p,
            false,
            WindowControlArea::Min,
            cx.listener(|_: &mut XoolFiles, _: &ClickEvent, window, _cx| {
                window.minimize_window();
            }),
        ))
        .child(window_control(
            "win-max",
            if is_max { "\u{E923}" } else { "\u{E922}" },
            p,
            false,
            WindowControlArea::Max,
            cx.listener(|_: &mut XoolFiles, _: &ClickEvent, window, _cx| {
                if window.is_maximized() {
                    platform::restore_window(window);
                } else {
                    window.zoom_window();
                }
            }),
        ))
        .child(window_control(
            "win-close",
            "\u{E8BB}",
            p,
            true,
            WindowControlArea::Close,
            cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                if this.request_window_close(window, cx) { window.remove_window(); }
            }),
        ));
    bar.into_any_element()
}

fn tab_location_icon(app: &XoolFiles, path: &std::path::Path, color: Rgba) -> AnyElement {
    let icon = app.settings.use_system_tab_icons.then(|| app.quick.iter()
        .find(|item| item.path.as_os_str().eq_ignore_ascii_case(path.as_os_str()))
        .and_then(|item| item.icon.clone())
        .or_else(|| app.drives.iter().find(|drive| drive.path.as_os_str().eq_ignore_ascii_case(path.as_os_str()))
            .and_then(|drive| drive.icon.clone()))
        .or_else(|| (!fs_model::is_recycle_bin(path)).then(platform::default_folder_icon).flatten())).flatten();
    if let Some(image) = icon {
        div().size(px(16.)).flex_shrink_0()
            .child(img(image).size_full().object_fit(ObjectFit::Contain)).into_any_element()
    } else {
        glyph(if fs_model::is_recycle_bin(path) { "\u{E74D}" } else { "\u{E8B7}" }, 15., color)
            .into_any_element()
    }
}

fn popup_in<E: Styled + IntoElement + 'static>(
    panel: E, id: &'static str, offset: impl Fn(E, f32) -> E + 'static,
) -> AnyElement {
    panel.with_animation(id, Animation::new(Duration::from_millis(165)).with_easing(ease_out_quint()),
        move |panel, progress| offset(panel.opacity(progress), (1. - progress) * 7.))
        .into_any_element()
}

fn recently_closed_popup(app: &XoolFiles, p: &Colors, width: f32, height: f32, title_h: f32, cx: &mut Context<XoolFiles>) -> AnyElement {
    let top = title_h + 4.;
    let panel = div().id("closed-tabs-popup").absolute().top(px(top))
        .right(px(138.).min(px(width / 4.))).w(px((width - 90.).clamp(170., 310.)))
        .max_h(px((height - title_h - 12.).max(72.))).overflow_y_scroll().occlude()
        .p(px(6.)).rounded(px(10.)).bg(p.panel).border_1().border_color(p.border).shadow_lg()
        .child(div().pl(px(8.)).pr(px(2.)).flex().items_center().justify_between().gap(px(4.))
            .child(div().flex_1().min_w(px(0.)).text_size(px(11.)).font_weight(FontWeight::SEMIBOLD)
                .child(tr("Pestañas cerradas recientemente", "Recently closed tabs")))
            .child(icon_button("clear-closed-tabs", "\u{E74D}", 13., p, false, !app.settings.closed_tabs.is_empty(),
                cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| this.clear_closed_tabs(cx)))
                .size(px(26.))
                .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new(tr("Borrar historial de pestañas cerradas", "Clear closed tab history")).build(window, cx))))
        .when(app.settings.closed_tabs.is_empty(), |panel| panel.child(
            div().px(px(8.)).py(px(8.)).text_size(px(11.)).text_color(p.muted).child(tr("Todavía no hay pestañas cerradas.", "No recently closed tabs."))))
        .children(app.settings.closed_tabs.iter().take(usize::from(app.settings.closed_tab_limit)).enumerate().map(|(index, path)| {
            let hint = path.display().to_string();
            div().id(("closed-tab", index)).h(px(31.)).px(px(8.)).rounded(px(6.))
                .flex().items_center().gap(px(8.)).cursor_pointer().hover(|style| style.bg(p.card_hover))
                .child(tab_location_icon(app, path, p.accent))
                .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(12.)).child(folder_name(path)))
                .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(hint.clone()).build(window, cx))
                .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                    this.reopen_closed_tab(index, window, cx);
                }))
        }));
    popup_in(panel, "closed-tabs-enter", move |panel, slide| panel.top(px(top - slide)))
}

const TAB_HEIGHT: f32 = 31.;
pub(crate) const TAB_WIDTH: f32 = 160.;
pub(crate) const TAB_GAP: f32 = 2.;

fn mix_tab_color(base: Rgba, tint: Rgba, strength: f32) -> Rgba {
    let mix = |base: f32, tint: f32| base + (tint - base) * strength;
    Rgba {
        r: mix(base.r, tint.r),
        g: mix(base.g, tint.g),
        b: mix(base.b, tint.b),
        a: base.a,
    }
}

fn apply_active_tab_gradient<T: Styled>(
    tab: T, p: &Colors, prefs: crate::settings::TabGradient,
) -> T {
    let strength = [0.24, 0.40, 0.58][prefs.intensity];
    let background = gpui::linear_gradient(
        90.,
        gpui::linear_color_stop(mix_tab_color(p.card, p.accent, strength), 0.),
        gpui::linear_color_stop(p.card, 1.),
    );
    let border = match prefs.border {
        0 => rgba(0x00000000),
        2 => mix_tab_color(p.card, p.accent, 0.72),
        _ => p.accent_soft,
    };
    tab.border_color(border).bg(background)
}

fn pane_tab_buttons<E: ParentElement>(
    mut tabs: E, app: &XoolFiles, pane_index: usize, p: &Colors, cx: &mut Context<XoolFiles>,
) -> E {
    let pane = &app.panes[pane_index];
    let active = pane.active;
    let active_pane = app.active_pane;
    for (tab_index, tab) in pane.tabs.iter().enumerate() {
        let in_active_pane = pane_index == active_pane;
        let active_tab = tab_index == active;
        let selected = in_active_pane && active_tab;
        let name = folder_name(&tab.path);
        let tab_id = tab.id;
        let close_index = tab_index;
        let drag_colors = *p;
        let prefs = &app.settings;
        let gradient = prefs.active_tab_gradient && selected;
        let close_left = prefs.show_tab_close && prefs.tab_close_left;
        let mut close_button = prefs.show_tab_close.then(|| {
            div().id(("top-tab-close", tab_id)).size(px(18.)).flex_shrink_0()
                .flex().items_center().justify_center().rounded(px(4.)).text_color(p.dim)
                .when(!prefs.tab_close_always, |button| button.opacity(0.)
                    .group_hover("file-tab", |style| style.opacity(1.)))
                .hover(|style| style.bg(p.danger_soft).text_color(p.danger))
                .cursor_pointer()
                .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                    this.close_tab(pane_index, close_index, window, cx);
                    cx.stop_propagation();
                }))
                .child(glyph_inherit("icons/tab-close.svg", 12.))
        });

        let tab_element = div()
            .id(("top-tab", tab.id))
            .group("file-tab")
            .relative()
            .h(px(TAB_HEIGHT))
            .w(px(TAB_WIDTH))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(if selected { p.accent_soft } else if active_tab { p.border } else { rgba(0x00000000) })
            .bg(if selected { p.accent_faint } else if active_tab { p.card } else { p.titlebar })
            .when(gradient, |tab| apply_active_tab_gradient(tab, p, prefs.tab_gradient))
            .text_color(if active_tab { p.text } else { p.muted })
            .when(!gradient, |tab| tab.hover(|style| style.bg(p.card_hover)))
            .cursor_pointer()
            .drag_over::<DragTab>({
                let accent = p.accent_soft;
                move |style, _, _, _| style.bg(accent)
            })
            .on_drag(
                DragTab {
                    tab_id,
                    label: name.clone(),
                },
                move |drag, _offset, _window, cx| {
                    tab_drag_ghost(drag, drag_colors, cx)
                },
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this: &mut XoolFiles, _: &MouseDownEvent, window, cx| {
                    this.middle_click_tab(tab_id, window, cx);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_down(MouseButton::Right, cx.listener(
                move |this: &mut XoolFiles, event: &MouseDownEvent, window, cx| {
                    this.open_context_menu(
                        MenuTarget::Tab(tab_id),
                        f32::from(event.position.x), f32::from(event.position.y), window, cx,
                    );
                    cx.stop_propagation();
                },
            ))
            .on_click(cx.listener(
                move |this: &mut XoolFiles, event: &ClickEvent, window, cx| {
                    if !event.standard_click() {
                        return;
                    }
                    this.switch_tab(pane_index, close_index, window, cx);
                },
            ))
            .when(close_left || !prefs.hide_tab_icons, |element| {
                element.child(div().relative().size(px(18.)).flex_shrink_0()
                    .flex().items_center().justify_center()
                    .when(!prefs.hide_tab_icons && !(close_left && prefs.tab_close_always), |slot| {
                        slot.child(div().when(close_left, |icon| icon.group_hover("file-tab", |style| style.opacity(0.)))
                            .child(tab_location_icon(app, &tab.path, if active_tab { p.accent } else { p.dim })))
                    })
                    .children(if close_left { close_button.take() } else { None }
                        .map(|button| button.absolute().top(px(0.)).left(px(0.)))))
            })
            .child(div().flex_1().min_w(px(0.)).text_size(px(12.)).font_weight(FontWeight::SEMIBOLD)
                .line_clamp(1).text_ellipsis().child(SharedString::from(name)))
            .children(close_button.filter(|_| !close_left));

        let tab_element = if fs_model::is_recycle_bin(&tab.path) { tab_element } else {
            attach_drop_target(tab_element, tab.path.clone(), p, cx)
        };

        // Las pestañas nuevas entran deslizándose desde la derecha y quedan
        // seleccionadas al terminar la animación.
        let offset = app.tab_reorder_offset(tab_id);
        let dragged = app.tab_reorder_target.is_some_and(|(source, target)| source == tab_id && target != source);
        let animated = tab_element.with_animation(
            ("tab-in", tab.id),
            Animation::new(Duration::from_millis(220)).with_easing(ease_out_quint()),
            move |element, delta| element.left(px(offset + (1.0 - delta) * 18.0))
                .opacity(delta * if dragged { 0.25 } else { 1.0 }),
        );
        tabs = tabs.child(div().id(("tab-slot", tab_id)).w(px(TAB_WIDTH)).h(px(TAB_HEIGHT)).flex_shrink_0()
            .on_drag_move::<DragFiles>(cx.listener(
                move |this: &mut XoolFiles, event: &gpui::DragMoveEvent<DragFiles>, window, cx| {
                    this.hover_file_drag_tab(tab_id, event.bounds,
                        event.bounds.contains(&event.event.position), window, cx);
                },
            ))
            .on_drag_move::<gpui::ExternalPaths>(cx.listener(
                move |this: &mut XoolFiles, event: &gpui::DragMoveEvent<gpui::ExternalPaths>, window, cx| {
                    this.hover_file_drag_tab(tab_id, event.bounds,
                        event.bounds.contains(&event.event.position), window, cx);
                },
            ))
            .on_drag_move::<DragTab>(cx.listener(move |this: &mut XoolFiles, event: &gpui::DragMoveEvent<DragTab>, _, cx| {
                if event.bounds.contains(&event.event.position)
                    && this.preview_tab_reorder(Some((event.drag(cx).tab_id, tab_id)))
                {
                    cx.notify();
                }
            }))
            .on_drop(cx.listener(move |this: &mut XoolFiles, drag: &DragTab, window, cx| {
                this.reorder_tab(drag.tab_id, tab_id, window, cx);
            }))
            .child(animated));
    }
    tabs
}

fn tab_wheel_listener(pane: usize, cx: &mut Context<XoolFiles>) -> impl Fn(&ScrollWheelEvent, &mut Window, &mut gpui::App) + 'static {
    cx.listener(move |this: &mut XoolFiles, event: &ScrollWheelEvent, window, cx| {
        if !this.settings.wheel_tabs || event.modifiers.control || event.modifiers.shift { return; }
        let vertical = match event.delta {
            ScrollDelta::Lines(delta) => delta.y,
            ScrollDelta::Pixels(delta) => if delta.y.abs() >= px(12.) { f32::from(delta.y) } else { 0. },
        };
        if vertical != 0. {
            this.wheel_tab(pane, if vertical > 0. { -1 } else { 1 }, window, cx);
            cx.stop_propagation();
        }
    })
}

fn pane_tab_strip(app: &XoolFiles, pane: usize, padding: f32, p: &Colors, cx: &mut Context<XoolFiles>) -> Div {
    let tabs = pane_tab_buttons(
        div().id(("pane-tabs-scroll", pane)).flex().items_center().gap(px(TAB_GAP))
            .min_w(px(0.)).flex_shrink().overflow_x_scroll().track_scroll(&app.panes[pane].tab_scroll)
            .on_scroll_wheel(tab_wheel_listener(pane, cx)),
        app, pane, p, cx,
    );
    div().flex().items_center().min_w(px(0.)).h_full().pl(px(padding))
        .child(tabs)
        .child(div().id(("pane-tab-new", pane)).w(px(25.)).h(px(24.)).ml(px(4.)).flex_shrink_0()
            .flex().items_center().justify_center().cursor_pointer()
            .rounded(px(4.)).hover(|style| style.bg(p.card_hover))
            .child(glyph("\u{E710}", 10., p.dim))
            .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                if this.active_pane != pane { this.activate_pane(pane, window, cx); }
                this.new_tab(window, cx);
            })))
}

fn pane_tabbar(
    app: &XoolFiles, pane: usize, width: f32, below_divider: bool, p: &Colors, cx: &mut Context<XoolFiles>,
) -> AnyElement {
    let curve_width = width.min(24.);
    let curve_color = p.titlebar;
    let curve = canvas(|_, _, _| {}, move |bounds, _, window, _| {
        let top = bounds.origin;
        let bottom = gpui::point(top.x, top.y + bounds.size.height);
        let end = gpui::point(top.x + bounds.size.width, top.y);
        let mut path = gpui::PathBuilder::fill();
        path.move_to(bottom);
        path.cubic_bezier_to(
            end,
            gpui::point(top.x + bounds.size.width * 0.5, bottom.y),
            gpui::point(top.x + bounds.size.width * 0.5, top.y),
        );
        path.line_to(top);
        path.close();
        if let Ok(shape) = path.build() {
            window.paint_path(shape, curve_color);
        }
    }).w(px(curve_width)).h_full().flex_shrink_0();
    div().w(px(width)).h(px(PANE_TABBAR_HEIGHT)).flex_shrink_0().flex()
        .when(!below_divider, |bar| bar.border_t_1().border_color(p.border_soft))
        .overflow_hidden()
        .child(pane_tab_strip(app, pane, 4., p, cx)
            .max_w(px((width - curve_width).max(0.))).bg(p.titlebar))
        .child(curve)
        .into_any_element()
}

fn title_drag_area(slot: usize, enabled: bool) -> impl IntoElement {
    canvas(
        move |bounds, window, _| platform::record_title_drag_area(window, slot, enabled.then_some(bounds)),
        |_, _, _, _| {},
    ).size_full()
}

fn window_control(
    id: &'static str,
    icon: &'static str,
    p: &Colors,
    close: bool,
    area: WindowControlArea,
    handler: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let ink = p.muted;
    let hover_bg = if close { rgb(0xe81123) } else { p.card_hover };
    let hover_fg = if close { rgb(0xffffff) } else { p.text };
    div()
        .id(id)
        .w(px(WINDOW_CONTROL_WIDTH))
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .window_control_area(area)
        .text_color(p.muted)
        .hover(move |style| style.bg(hover_bg).text_color(hover_fg))
        .active(move |style| style.bg(hover_bg).text_color(hover_fg))
        .cursor_pointer()
        .on_click(handler)
        .child(canvas(|_, _, _| {}, move |bounds, _, window, _| {
            let scale = window.scale_factor();
            let center = gpui::point(bounds.origin.x + bounds.size.width * 0.5, bounds.origin.y + bounds.size.height * 0.5);
            let align = |value: Pixels| px(((f32::from(value) * scale).floor() + 0.5) / scale);
            let at = |x, y| gpui::point(align(center.x + px(x)), align(center.y + px(y)));
            let mut path = gpui::PathBuilder::stroke(px(1. / scale));
            let mut trace = |start: (f32, f32), rest: &[(f32, f32)]| {
                path.move_to(at(start.0, start.1));
                for &(x, y) in rest {
                    path.line_to(at(x, y));
                }
            };
            match icon {
                "\u{E921}" => trace((-5., 0.), &[(5., 0.)]),
                "\u{E922}" => trace((-5., -5.), &[(5., -5.), (5., 5.), (-5., 5.), (-5., -5.)]),
                "\u{E923}" => {
                    trace((-5., -3.), &[(3., -3.), (3., 5.), (-5., 5.), (-5., -3.)]);
                    trace((-3., -3.), &[(-3., -5.), (5., -5.), (5., 3.), (3., 3.)]);
                }
                _ => {
                    trace((-5., -5.), &[(5., 5.)]);
                    trace((5., -5.), &[(-5., 5.)]);
                }
            }
            if let Ok(shape) = path.build() {
                let color = if bounds.contains(&window.mouse_position()) { hover_fg } else { ink };
                window.paint_path(shape, color);
            }
        }).size_full())
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Barra lateral
// ---------------------------------------------------------------------------

fn animated_sidebar(app: &XoolFiles, width: f32, p: &Colors, cx: &mut Context<XoolFiles>) -> Div {
    let panel = sidebar(app, p, cx);
    let handle = if app.settings.sidebar_icons_only {
        div().w(px(4.)).flex_shrink_0().when(app.settings.translucent_background, |spacer| spacer.bg(p.sidebar))
            .into_any_element()
    } else { sidebar_resizer(app, p, cx) };
    let wrapper = div().w(px(width)).h_full().flex_shrink_0().overflow_hidden().flex();
    if app.settings.sidebar_right {
        wrapper.child(handle).child(panel)
    } else {
        wrapper.justify_end().child(panel).child(handle)
    }
}

fn animated_workspace_panel(app: &XoolFiles, visible_width: f32, viewport_width: f32, p: &Colors, cx: &mut Context<XoolFiles>) -> Div {
    let panel = workspace_panel::panel(app, app.workspace_panel_width(viewport_width), p, cx);
    div().w(px(visible_width)).h_full().flex_shrink_0().overflow_hidden().flex()
        .when(!app.settings.workspaces.right, |wrapper| wrapper.justify_end())
        .child(panel)
}

fn sidebar(app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    let current = app.active_tab().path.clone();

    let mut sidebar_list = div()
        .id("sidebar-scroll")
        .flex_1()
        .min_h(px(0.))
        .overflow_y_scroll()
        .px(px(if app.settings.sidebar_icons_only { 6. } else { 9. }))
        .pb(px(8.));

    for (index, group) in app.settings.sidebar_groups.iter().copied().enumerate() {
        let only = app.settings.sidebar_icons_only;
        let progress = if only { 1.0 } else { app.sidebar_group_progress(group) };
        let row_height = app.settings.sidebar_row_height();
        let mut list = div().flex().flex_col().flex_shrink_0();
        match group {
            SidebarGroup::Favorites => {
                if progress > 0. && !only && app.settings.favorites.is_empty() {
                    list = list.child(
                        div()
                            .px(px(8.))
                            .py(px(6.))
                            .text_color(p.dim)
                            .text_size(px(10.5))
                            .child(tr("Marca archivos o carpetas con el clic derecho.", "Right-click files or folders to add them to Favorites.")),
                    );
                } else if progress > 0. {
                    for (index, favorite) in app.settings.favorites.iter().enumerate() {
                        let active = current == favorite.path;
                        let row = sidebar_row(
                            ("favorite", index),
                            "\u{E735}",
                            rgb(crate::settings::favorite_color(favorite.color)),
                            None,
                            &favorite.name,
                            None,
                            active,
                &app.settings,
                            p,
                            |_, _, _| {},
                            cx.listener({
                                let path = favorite.path.clone();
                                move |this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                                    this.open_location(path.clone(), window, cx);
                                }
                            }),
                            cx.listener({
                                let path = favorite.path.clone();
                                move |this: &mut XoolFiles, event: &MouseDownEvent, window, cx| {
                                    this.open_context_menu(
                                        MenuTarget::SidebarFavorite(path.clone()),
                                        f32::from(event.position.x),
                                        f32::from(event.position.y),
                                        window,
                                        cx,
                                    );
                                    cx.stop_propagation();
                                }
                            }),
                            cx.listener({
                                let path = favorite.path.clone();
                                move |this: &mut XoolFiles, _: &MouseDownEvent, window, cx| {
                                    if path.is_dir() || fs_model::is_recycle_bin(&path) {
                                        this.open_with_middle_click(path.clone(), window, cx);
                                    }
                                    cx.stop_propagation();
                                }
                            }),
                        );
                        let path = favorite.path.clone();
                        let drag = SidebarFavoriteDrag(path.clone());
                        let drag_label: SharedString = favorite.name.clone().into();
                        let drag_colors = *p;
                        let offset = app.sidebar_favorite_offset(index);
                        let is_dragged = app.sidebar_favorite_target.is_some_and(|(source, _)| source == index);
                        list = list.child(div().id(("favorite-drop", index)).h(px(row_height)).flex_shrink_0()
                            .when(app.settings.reorder_sidebar_favorites || app.settings.workspaces.enabled, |slot| {
                                slot.on_drag(drag, move |_, _, _, cx| {
                                    cx.new(|_| DragGhost {
                                        label: drag_label.clone(), count: 1, colors: drag_colors, icon: "\u{E735}",
                                    })
                                })
                            })
                            .when(app.settings.reorder_sidebar_favorites, |slot| {
                                slot.on_drag_move::<SidebarFavoriteDrag>(cx.listener(
                                    move |this: &mut XoolFiles, event: &gpui::DragMoveEvent<SidebarFavoriteDrag>, _, cx| {
                                        if event.bounds.contains(&event.event.position) {
                                            let source = event.drag(cx).0.clone();
                                            this.hover_sidebar_favorite(&source, index, cx);
                                        }
                                    }))
                                .on_drop(cx.listener(move |this: &mut XoolFiles, source: &SidebarFavoriteDrag, _, cx| {
                                    this.move_sidebar_favorite(&source.0, index, cx);
                                    cx.stop_propagation();
                                }))
                            })
                            .child(div().relative().top(px(offset)).when(is_dragged, |row| row.opacity(0.35)).child(row)));
                    }
                }
            }
            SidebarGroup::Quick => {
                if progress > 0. {
            
                for (index, item) in app.quick.iter().enumerate() {
                    let active = current == item.path;
                    let action = item.action;
                    list = list.child(sidebar_row(
                        ("quick", index),
                        item.glyph,
                        rgb(item.color),
                        item.icon.clone(),
                        &item.label,
                        None,
                        active,
                &app.settings,
                        p,
                        location_menu_hover(&item.path, cx),
                        cx.listener({
                            let path = item.path.clone();
                            move |this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                                match action {
                                    Some(QuickAction::RecycleBin) => {
                                        this.navigate(PathBuf::from(fs_model::RECYCLE_BIN_PATH), window, cx)
                                    }
                                    None => this.navigate(path.clone(), window, cx),
                                }
                            }
                        }),
                        sidebar_location_menu(&item.path, cx),
                        cx.listener({
                            let path = item.path.clone();
                            move |this: &mut XoolFiles, _: &MouseDownEvent, window, cx| {
                                match action {
                                    Some(QuickAction::RecycleBin) => this.open_with_middle_click(
                                        PathBuf::from(fs_model::RECYCLE_BIN_PATH),
                                        window,
                                        cx,
                                    ),
                                    None => this.open_with_middle_click(path.clone(), window, cx),
                                }
                                cx.stop_propagation();
                            }
                        }),
                    ));
                }
                }
            }
            SidebarGroup::Drives => {
                if progress > 0. {
                    for (index, drive) in app.drives.iter().enumerate() {
                        let active = current == drive.path;
                        let root = drive_row(
                            index,
                            drive,
                            active,
                &app.settings,
                            p,
                            location_menu_hover(&drive.path, cx),
                            cx.listener({
                                let path = drive.path.clone();
                                move |this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                                    this.navigate(path.clone(), window, cx);
                                }
                            }),
                            sidebar_location_menu(&drive.path, cx),
                            cx.listener({
                                let path = drive.path.clone();
                                move |this: &mut XoolFiles, _: &MouseDownEvent, window, cx| {
                                    this.open_with_middle_click(path.clone(), window, cx);
                                    cx.stop_propagation();
                                }
                            }),
                        );
                        if app.settings.sidebar_drive_tree && !app.settings.sidebar_icons_only {
                            list = list.child(div().flex().items_center()
                                .child(sidebar_drive_branch(&drive.path, app, p, cx))
                                .child(div().flex_1().min_w(px(0.)).child(root)));
                            if let Some(Some(children)) = app.sidebar_drive_nodes.get(&drive.path) {
                                for (child_index, path) in children.iter().enumerate() {
                                    let on_hover = location_menu_hover(path, cx);
                                    let on_right = sidebar_location_menu(path, cx);
                                    let path = path.clone();
                                    list = list.child(div().id(("sidebar-drive-folder", index * 1000 + child_index))
                                        .h(px(app.settings.sidebar_row_height())).pl(px(22.)).pr(px(5.)).flex()
                                        .items_center().gap(px(5.)).cursor_pointer().rounded(px(5.))
                                        .hover(|row| row.bg(p.card_hover)).when(current == path, |row| row.bg(p.selection))
                                        .child(glyph("\u{E8B7}", 12., p.accent))
                                        .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(11.)).child(folder_name(&path)))
                                        .on_hover(on_hover)
                                        .on_mouse_down(MouseButton::Right, on_right)
                                        .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, window, cx| this.navigate(path.clone(), window, cx))));
                                }
                            }
                        } else {
                            list = list.child(root);
                        }

                    }
                }
            }
            SidebarGroup::Recent => {
                if progress > 0. {
                    let recent = &app.settings.recent_file_paths;
                    if recent.is_empty() && !only {
                        list = list.child(sidebar_group_hint(tr("Aquí aparecerán los archivos que abras.", "Files you open will appear here."), p));
                    }
                    for (index, path) in recent.iter().take(app.settings.sidebar_recent_limit).enumerate() {
                        list = list.child(sidebar_saved_location(("sidebar-recent-file", index), path,
                            false, false, app, p, cx));
                    }
                }
            }
            SidebarGroup::Custom(id) => {
                if progress > 0. && let Some(custom) = app.settings.custom_sidebar_group(id) {
                    for (index, path) in custom.folders.iter().enumerate() {
                        list = list.child(sidebar_saved_location(
                            ("custom-folder", index), path,
                            true, current == *path, app, p, cx));
                    }
                    if custom.folders.is_empty() {
                        list = list.child(div().px(px(4.)).child(icon_button(("add-sidebar-folder", id),
                            "\u{E710}", 13., p, false, true,
                            cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| this.add_sidebar_group_folders(id, cx)))
                            .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new(tr("Añadir carpetas al grupo", "Add folders to the group")).build(window, cx))));
                        if !only {
                            list = list.child(sidebar_group_hint(tr("Arrastra carpetas a este grupo.", "Drag folders into this group."), p));
                        }
                    }
                }
            }
        }
        let mut group_panel = div().id(group.heading_id()).flex().flex_col().mb(px(3.))
            .when(index > 0 && only, |row| row.border_t_1().border_color(p.border_soft).pt(px(4.)))
            .when(!only, |row| row.child(sidebar_section(
                match group {
                    SidebarGroup::Custom(_) => app.settings.sidebar_group_name(group).to_uppercase().into(),
                    _ => group.heading().into(),
                },
                match group {
                    SidebarGroup::Favorites => Some("icons/sidebar-heading-favorites.svg"),
                    SidebarGroup::Quick => Some("icons/sidebar-heading-quick.svg"),
                    SidebarGroup::Drives => Some("icons/sidebar-heading-drives.svg"),
                    SidebarGroup::Recent => Some("icons/sidebar-heading-recent.svg"),
                    SidebarGroup::Custom(_) => None,
                },
                app.settings.sidebar_group_collapsed(group), p,
                cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| this.toggle_sidebar_group(group, cx)))));
        if let SidebarGroup::Custom(id) = group {
            let hover = p.accent_faint;
            group_panel = group_panel
                .on_drop(cx.listener(move |this: &mut XoolFiles, files: &DragFiles, _, cx| {
                    this.pin_sidebar_group_folders(id, &files.paths, cx);
                    cx.stop_propagation();
                }))
                .on_drop(cx.listener(move |this: &mut XoolFiles, files: &gpui::ExternalPaths, _, cx| {
                    this.pin_sidebar_group_folders(id, files.paths(), cx);
                    cx.stop_propagation();
                }))
                .drag_over::<DragFiles>(move |style, _, _, _| style.bg(hover))
                .drag_over::<gpui::ExternalPaths>(move |style, _, _, _| style.bg(hover));
        }
        let height = match group {
            SidebarGroup::Favorites => (app.settings.favorites.len() as f32 * row_height).max(40.),
            SidebarGroup::Quick => app.quick.len() as f32 * row_height,
            SidebarGroup::Recent => (app.settings.recent_file_paths.len().min(app.settings.sidebar_recent_limit) as f32 * row_height).max(28.),
            SidebarGroup::Custom(id) => app.settings.custom_sidebar_group(id)
                .map_or(0., |group| (group.folders.len() as f32 * row_height).max(55.)),
            SidebarGroup::Drives => {
                let nodes: usize = app.sidebar_drive_nodes.values().filter_map(|paths| paths.as_ref().map(Vec::len)).sum();
                app.drives.len() as f32 * row_height.max(35.) + nodes as f32 * row_height
            },
        };
        sidebar_list = sidebar_list.child(group_panel.child(
            div().flex().flex_col().when(progress < 1.0, |body| {
                body.max_h(px(height * progress)).overflow_hidden().opacity(progress)
            }).child(list)
        ));
    }

    let hidden = app.settings.show_hidden;
    let footer = div()
        .flex_shrink_0()
        .px(px(9.))
        .py(px(8.))
        .border_t_1()
        .border_color(p.border_soft)
        .child(
            div()
                .id("sidebar-hidden")
                .h(px(30.))
                .px(px(8.))
                .rounded(px(7.))
                .flex()
                .items_center()
                .gap(px(8.))
                .text_size(px(11.))
                .text_color(if hidden { p.accent } else { p.muted })
                .hover(|style| style.bg(p.card_hover).text_color(p.text))
                .on_click(cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.toggle_hidden(cx);
                }))
                .child(glyph(
                    if hidden { "\u{E8F4}" } else { "\u{E8F5}" },
                    12.,
                    if hidden { p.accent } else { p.muted },
                ))
                .when(!app.settings.sidebar_icons_only, |row| row.child(div().flex_1().child(tr("Archivos ocultos", "Hidden files"))).child(switch(hidden, p))),
        );

    div()
        .w(px(app.settings.sidebar_display_width()))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .bg(p.sidebar)
        .border_r_1()
        .border_color(p.border_soft)
        .child(sidebar_list)
        .child(footer)
        .into_any_element()
}

const PANEL_RESIZE_HANDLE: f32 = 4.;

fn panel_resize_handle(
    id: &'static str, active: bool, translucent: bool, p: &Colors,
    on_mouse_down: impl Fn(&MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let idle = if translucent { p.sidebar } else { rgba(0x00000000) };
    let hover = if translucent { p.border } else { p.accent_soft };
    div()
        .id(id)
        .w(px(PANEL_RESIZE_HANDLE))
        .h_full()
        .flex_shrink_0()
        .bg(if active { p.accent } else { idle })
        .when(!active, |style| style.hover(move |style| style.bg(hover)))
        .cursor(CursorStyle::ResizeLeftRight)
        .on_mouse_down(MouseButton::Left, on_mouse_down)
        .into_any_element()
}

fn sidebar_resizer(app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    panel_resize_handle("sidebar-resizer", app.sidebar_resizing, app.settings.translucent_background, p,
        cx.listener(|this: &mut XoolFiles, _: &MouseDownEvent, _, cx| {
            this.begin_sidebar_resize(cx);
            cx.stop_propagation();
        }))
}

fn preview_resizer(app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    let active = app.preview_resizing;
    div()
        .id("preview-resizer")
        .w(px(4.))
        .h_full()
        .flex_shrink_0()
        .bg(if active { p.accent } else { rgba(0x00000000) })
        .when(!active, |style| {
            style.hover(|style| style.bg(p.accent_soft))
        })
        .cursor(CursorStyle::ResizeLeftRight)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this: &mut XoolFiles, _: &MouseDownEvent, _window, cx| {
                this.begin_preview_resize(cx);
                cx.stop_propagation();
            }),
        )
        .into_any_element()
}

fn sidebar_group_hint(text: &'static str, p: &Colors) -> Div {
    div().px(px(8.)).py(px(5.)).text_size(px(10.5)).text_color(p.dim).child(text)
}

struct SidebarFavoriteDrag(std::path::PathBuf);

fn sidebar_saved_location(
    id: impl Into<ElementId>, path: &std::path::Path, folder: bool, active: bool,
    app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>,
) -> AnyElement {
    let location = path.to_path_buf();
    let context = if folder { MenuTarget::Sidebar(location.clone()) } else { MenuTarget::SidebarRecentFile(location.clone()) };
    let middle = location.clone();
    let hover = location_menu_hover(path, cx);
    let image = if folder { platform::default_folder_icon() }
        else { app.recent_icons.get(path).cloned().flatten() };
    sidebar_row(
        id, if folder { "\u{E8B7}" } else { "\u{E8A5}" }, p.accent,
        image, &folder_name(path), None, active, &app.settings, p,
        move |hovered, window, cx| { if folder { hover(hovered, window, cx); } },
        cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, window, cx| this.open_location(location.clone(), window, cx)),
        cx.listener(move |this: &mut XoolFiles, event: &MouseDownEvent, window, cx| {
            this.open_context_menu(context.clone(), f32::from(event.position.x), f32::from(event.position.y), window, cx);
            cx.stop_propagation();
        }),
        cx.listener(move |this: &mut XoolFiles, _: &MouseDownEvent, window, cx| {
            if folder { this.open_with_middle_click(middle.clone(), window, cx); }
            else { this.open_location(middle.clone(), window, cx); }
            cx.stop_propagation();
        }),
    )
}

fn sidebar_section(
    label: SharedString,
    icon: Option<&'static str>,
    collapsed: bool,
    p: &Colors,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    div().id("sidebar-group-heading").h(px(24.)).mt(px(8.)).mb(px(2.)).px(px(8.))
        .flex().items_center().justify_between().rounded(px(5.))
        .text_size(px(8.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.dim)
        .cursor_pointer().hover(|style| style.bg(p.card_hover).text_color(p.text))
        .when_some(icon, |heading, icon| heading.gap(px(5.))
            .child(glyph_inherit(icon, 12.)))
        .child(div().min_w(px(0.)).flex_1().truncate().child(label))
        .child(glyph_inherit(if collapsed { "\u{E76C}" } else { "\u{E70D}" }, 8.5))
        .on_click(on_click)
        .into_any_element()
}

fn location_menu_hover(
    path: &std::path::Path, cx: &mut Context<XoolFiles>,
) -> impl Fn(&bool, &mut Window, &mut gpui::App) + 'static {
    let path = path.to_path_buf();
    cx.listener(move |this: &mut XoolFiles, hovered: &bool, window, cx| {
        this.hover_location_context_menu(&path, *hovered, window, cx);
    })
}

fn sidebar_location_menu(
    path: &std::path::Path, cx: &mut Context<XoolFiles>,
) -> impl Fn(&MouseDownEvent, &mut Window, &mut gpui::App) + 'static {
    let path = path.to_path_buf();
    cx.listener(move |this: &mut XoolFiles, event: &MouseDownEvent, window, cx| {
        this.open_context_menu(
            MenuTarget::Sidebar(path.clone()), f32::from(event.position.x),
            f32::from(event.position.y), window, cx,
        );
        cx.stop_propagation();
    })
}

fn sidebar_row(
    id: impl Into<ElementId>,
    icon: &'static str,
    color: Rgba,
    image: Option<Arc<RenderImage>>,
    label: &str,
    sub: Option<String>,
    active: bool,
    settings: &Settings,
    p: &Colors,
    on_hover: impl Fn(&bool, &mut Window, &mut gpui::App) + 'static,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
    on_right: impl Fn(&MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
    on_middle: impl Fn(&MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let only = settings.sidebar_icons_only;
    let tooltip = label.to_string();
    div()
        .id(id)
        .h(px(settings.sidebar_row_height()))
        .when(only, |row| row.justify_center().tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)))
        .px(px(8.))
        .rounded(px(7.))
        .flex()
        .items_center()
        .gap(px(9.))
        .text_size(px(13.))
        .text_color(if active { p.text } else { p.muted })
        .when(active, |style| style.bg(p.selection).text_color(p.text))
        .when(!active, |style| {
            style.hover(|style| style.bg(p.card_hover).text_color(p.text))
        })
        .cursor_pointer()
        .on_hover(on_hover)
        .on_click(on_click)
        .on_mouse_down(MouseButton::Right, on_right)
        .on_mouse_down(MouseButton::Middle, on_middle)
        .child(match image {
            Some(image) => div()
                .size(px(15.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .child(img(image).w_full().h_full().object_fit(ObjectFit::Contain))
                .into_any_element(),
            None => glyph(icon, 13., color).into_any_element(),
        })
        .when(!only, |row| row.child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_ellipsis()
                        .child(SharedString::from(label.to_string())),
                )
                .when_some(sub, |this, sub| {
                    this.child(
                        div()
                            .text_size(px(8.5))
                            .text_color(p.dim)
                            .text_ellipsis()
                            .child(sub),
                    )
                }),
        ))
        .when(active && !only, |style| {
            style.child(div().w(px(3.)).h(px(14.)).rounded(px(2.)).bg(p.accent))
        })
        .into_any_element()
}

fn drive_row(
    index: usize,
    drive: &DriveInfo,
    active: bool,
    settings: &Settings,
    p: &Colors,
    on_hover: impl Fn(&bool, &mut Window, &mut gpui::App) + 'static,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
    on_right: impl Fn(&MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
    on_middle: impl Fn(&MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let title = match (settings.drive_name_style, &drive.label) {
        (0, Some(label)) => format!("{label} ({})", drive.letter),
        (1, Some(label)) => format!("{} {label}", drive.letter),
        _ => drive.letter.clone(),
    };
    let only = settings.sidebar_icons_only;
    let tooltip = title.clone();
    let used = drive.used_fraction();
    let free_label = if drive.total == 0 {
        tr("Sin datos", "No data").to_string()
    } else {
        crate::localized!(format, "{} libres", "{} free", fs_model::format_size(drive.free))
    };
    div()
        .id(("drive", index))
        .px(px(8.))
        .min_h(px(settings.sidebar_row_height()))
        .py(px(if only { 0. } else { 4. }))
        .when(only, |row| row.justify_center().tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)))
        .rounded(px(7.))
        .flex()
        .items_center()
        .gap(px(9.))
        .text_color(if active { p.text } else { p.muted })
        .when(active, |style| style.bg(p.selection))
        .when(!active, |style| style.hover(|style| style.bg(p.card_hover)))
        .cursor_pointer()
        .on_hover(on_hover)
        .on_click(on_click)
        .on_mouse_down(MouseButton::Right, on_right)
        .on_mouse_down(MouseButton::Middle, on_middle)
        .child(match drive.icon.clone() {
            Some(image) => div()
                .size(px(15.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .child(img(image).w_full().h_full().object_fit(ObjectFit::Contain))
                .into_any_element(),
            None => {
                glyph("\u{E7F1}", 13., if active { p.accent } else { p.muted }).into_any_element()
            }
        })
        .when(!only, |row| row.child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .text_size(px(13.))
                                .text_ellipsis()
                                .text_color(if active { p.text } else { p.muted })
                                .child(title),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(10.))
                                .text_color(p.dim)
                                .child(free_label),
                        ),
                )
                .when(!settings.hide_drive_usage, |info| info.child(
                    div()
                        .h(px(3.))
                        .w_full()
                        .rounded_full()
                        .bg(p.border_soft)
                        .child(
                            div()
                                .h(px(3.))
                                .w(gpui::relative(used))
                                .rounded_full()
                                .bg(if used >= 0.90 { p.danger } else { p.accent }),
                        ),
                )),
        ))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Barra de herramientas
// ---------------------------------------------------------------------------

fn command_bar_height(app: &XoolFiles) -> f32 {
    if app.settings.command_bar { COMMAND_BAR_HEIGHT } else { 0.0 }
}

fn command_bar(app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    type Command = fn(&mut XoolFiles, &mut Window, &mut Context<XoolFiles>);
    let count = app.active_tab().selection.len();
    let recycle = app.in_recycle_bin();
    let commands: [(&str, &str, &str, bool, Command); 6] = [
        ("folder", "\u{E8F4}", tr("Nueva carpeta", "New folder"), !recycle, XoolFiles::open_new_folder_dialog),
        ("cut", "\u{E8C6}", tr("Cortar", "Cut"), count > 0 && !recycle, |this, window, cx| this.copy_selection(true, window, cx)),
        ("copy", "\u{E8C8}", tr("Copiar", "Copy"), count > 0 && !recycle, |this, window, cx| this.copy_selection(false, window, cx)),
        ("paste", "\u{E77F}", tr("Pegar", "Paste"), !recycle, XoolFiles::paste),
        ("rename", "\u{E8AC}", tr("Renombrar", "Rename"), count == 1 && !recycle, XoolFiles::rename_selected),
        ("delete", "\u{E74D}", tr("Eliminar", "Delete"), count > 0, XoolFiles::delete_selected),
    ];
    div().id("command-bar").h(px(COMMAND_BAR_HEIGHT)).flex_shrink_0()
        .flex().items_center().gap(px(3.)).px(px(10.)).overflow_x_scroll()
        .bg(p.panel).border_b_1().border_color(p.border_soft)
        .children(commands.into_iter().map(|(id, icon, title, enabled, command)| {
            div().id(id).h(px(25.)).flex_shrink_0()
                .flex().items_center().gap(px(5.)).px(px(7.)).rounded(px(5.))
                .text_size(px(11.)).text_color(if enabled { p.text } else { p.dim })
                .when(enabled, |button| button.cursor_pointer().hover(|style| style.bg(p.card_hover))
                    .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                        command(this, window, cx);
                    })))
                .child(glyph(icon, 12., if enabled { p.accent } else { p.dim }))
                .child(title)
        }))
        .into_any_element()
}

fn toolbar(app: &XoolFiles, p: &Colors, width: f32, window: &Window, cx: &mut Context<XoolFiles>) -> AnyElement {
    let tab = app.active_tab();
    let can_back = tab.hist_index > 0;
    let can_forward = tab.hist_index + 1 < tab.history.len();
    let can_up = !fs_model::is_recycle_bin(&tab.path) && tab.path.parent().is_some();
    let narrow = width < 420.;
    let compact_ui = width < 680.;
    let address_compact = width < 550.;
    let essentials_only = width < 270.;

    let mut bar = div()
        .h_full()
        .flex_1()
        .min_w(px(0.))
        .overflow_hidden()
        .flex()
        .items_center()
        .gap(px(8.));

    // Navegación
    let nav = div()
        .h(px(34.))
        .flex()
        .items_center()
        .rounded(px(8.))
        .px(px(2.)).gap(px(2.))
        .bg(p.card)
        .child(icon_button(
            "nav-back",
            "icons/nav-back.svg",
            12.,
            p,
            false,
            can_back,
            cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                this.travel(-1, window, cx);
            }),
        ).size(px(28.)).rounded(px(5.)))
        .child(icon_button(
            "nav-forward",
            "icons/nav-forward.svg",
            12.,
            p,
            false,
            can_forward,
            cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                this.travel(1, window, cx);
            }),
        ).size(px(28.)).rounded(px(5.)))
        .child(div().w(px(1.)).h(px(16.)).bg(p.border))
        .child(icon_button(
            "nav-up",
            "icons/nav-up.svg",
            12.,
            p,
            false,
            can_up,
            cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                this.go_up(window, cx);
            }),
        ).size(px(28.)).rounded(px(5.)))
        .child(icon_button(
            "nav-refresh",
            "icons/nav-refresh.svg",
            12.,
            p,
            false,
            true,
            cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                this.refresh_active(cx);
            }),
        ).size(px(28.)).rounded(px(5.)).bg(p.panel_soft));

    if !narrow {
        bar = bar.child(nav);
    }

    // Dirección / migas de pan
    let address_entity = cx.entity().downgrade();
    let address_measure = canvas(move |bounds, _, cx| {
        let _ = address_entity.update(cx, |this, cx| {
            if this.address_bounds != Some(bounds) {
                this.address_bounds = Some(bounds);
                cx.notify();
            }
        });
    }, |_, _, _, _| {}).absolute().top(px(0.)).left(px(0.)).size_full();
    let address = if app.address_editing {
        div()
            .relative()
            .child(address_measure)
            .flex_1()
            .min_w(px(0.))
            .overflow_hidden()
            .h(px(34.))
            .flex()
            .items_center()
            .px(px(9.))
            .rounded(px(9.))
            .bg(p.input)
            .border_1()
            .border_color(p.accent)
            .child(
                div().w_full().child(
                    Input::new(&app.address)
                        .appearance(false)
                        .bordered(false)
                        .h(px(30.)),
                ),
            )
            .into_any_element()
    } else {
        div()
            .id("address-bar")
            .relative()
            .child(address_measure)
            .flex_1()
            .min_w(px(0.))
            .overflow_hidden()
            .h(px(34.))
            .flex()
            .items_center()
            .gap(px(4.))
            .px(px(6.))
            .rounded(px(9.))
            .bg(p.input)
            .border_1()
            .border_color(p.border)
            .cursor_pointer()
            .on_click(
                cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                    this.begin_address_edit(window, cx);
                }),
            )
            .on_mouse_down(MouseButton::Right, cx.listener(|this: &mut XoolFiles, event: &MouseDownEvent, window, cx| {
                let path = this.active_tab().path.clone();
                this.open_context_menu(MenuTarget::Address(path), f32::from(event.position.x),
                    f32::from(event.position.y), window, cx);
                cx.stop_propagation();
            }))
            .child(breadcrumbs(app, address_compact, p, window, cx))
            .when(!address_compact, |style| {
                style
                    .child(icon_button(
                        "address-history",
                        "\u{E81C}",
                        11.,
                        p,
                        app.address_history_open,
                        true,
                        cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                            this.toggle_address_history(cx);
                            cx.stop_propagation();
                        }),
                    ))
                    .child(icon_button(
                        "address-edit",
                        "\u{E70F}",
                        11.,
                        p,
                        false,
                        true,
                        cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                            this.begin_address_edit(window, cx);
                            cx.stop_propagation();
                        }),
                    ))
            })
            .into_any_element()
    };
    bar = bar.child(address);

    // Acciones
    let sort_label = tab.sort.label();
    let mut sort_button = div()
        .id("sort-button")
        .h(px(34.))
        .px(px(if compact_ui { 7. } else { 9. }))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(9.))
        .bg(p.input)
        .border_1()
        .border_color(if app.sort_menu {
            p.accent_soft
        } else {
            p.border
        })
        .text_color(p.muted)
        .text_size(px(10.5))
        .hover(|style| style.bg(p.card_hover).text_color(p.text))
        .on_click(cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
            this.sort_menu = !this.sort_menu;
            this.context_menu = None;
            cx.notify();
        }))
        .child(glyph("\u{E8CB}", 11., p.muted));
    if !compact_ui {
        sort_button = sort_button.child(SharedString::from(sort_label));
    }
    sort_button = sort_button.child(glyph(
        if tab.sort_desc {
            "\u{E70D}"
        } else {
            "\u{E70E}"
        },
        7.,
        p.dim,
    ));
    bar = bar.child(sort_button);

    let grid = tab.view == ViewMode::Grid;
    let view_hint = if grid { tr("Cambiar a Detalles", "Switch to Details") } else { tr("Cambiar a Iconos", "Switch to Icons") };
    bar = bar.when(!essentials_only, |bar| bar.child(icon_button(
        "view-toggle",
        if grid { "icons/toolbar-view-details.svg" } else { "icons/toolbar-view-grid.svg" },
        12.,
        p,
        false,
        true,
        cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
            let zoom = if this.active_tab().view == ViewMode::Grid { 0 } else { 25 };
            this.set_zoom(zoom, cx);
        }),
    ).tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(view_hint).build(window, cx))));

    // Dividir y Vista previa comparten un control segmentado para que se
    // lean como dos estados del mismo grupo, igual que Lista | Cuadrícula.
    let mut view_actions = div()
        .id("view-actions")
        .h(px(34.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(2.))
        .p(px(2.))
        .rounded(px(9.))
        .bg(p.input)
        .border_1()
        .border_color(p.border);
    view_actions = view_actions.child(segment_button(
        "split-toggle",
        "",
        if compact_ui { None } else { Some(tr("Dividir", "Split")) },
        p,
        app.panes.len() > 1,
        cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| {
            this.toggle_split(window, cx);
        }),
    ));
    view_actions = view_actions.child(div().w(px(1.)).h(px(14.)).flex_shrink_0().bg(p.border));
    view_actions = view_actions.child(segment_button(
        "preview-toggle",
        "\u{E8A1}",
        if compact_ui { None } else { Some(tr("Vista previa", "Preview")) },
        p,
        app.settings.show_preview,
        cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
            this.toggle_preview(cx);
        }),
    ));
    bar = bar.when(!essentials_only, |bar| bar.child(view_actions));

    let settings = icon_button(
        "settings-button",
        "icons/toolbar-settings.svg",
        16.,
        p,
        false,
        true,
        cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| {
            this.open_settings(window, cx);
        }),
    ).size(px(34.)).flex_shrink_0().rounded(px(9.)).bg(p.card);

    div().h(px(TOOLBAR_HEIGHT)).w_full().flex_shrink_0()
        .flex().items_center().gap(px(8.)).px(px(12.))
        .bg(p.toolbar).border_b_1().border_color(p.border_soft)
        .child(bar).child(settings).into_any_element()
}

fn breadcrumbs(app: &XoolFiles, controls_hidden: bool, p: &Colors, window: &Window, cx: &mut Context<XoolFiles>) -> AnyElement {
    let path = &app.active_tab().path;
    let mut parts = breadcrumb_parts(path);
    if !app.settings.path_breadcrumbs || parts.is_empty() {
        let label = if fs_model::is_recycle_bin(path) { tr("Papelera", "Recycle Bin").to_owned() } else { fs_model::normalize_path(path) };
        return div().flex_1().min_w(px(0.)).px(px(6.)).truncate()
            .text_size(px(11.5)).text_color(p.text).child(label).into_any_element();
    }

    let mut collapsed = None;
    if app.settings.compact_paths && parts.len() > 1 && let Some(bounds) = app.address_bounds {
        let width = (f32::from(bounds.size.width) - if controls_hidden { 14. } else { 86. }).max(0.);
        let widths: Vec<_> = parts.iter().map(|(label, _)| measure_grid_text(label, 11.5, window) + 22.).collect();
        if widths.iter().sum::<f32>() > width {
            let last = widths.len() - 1;
            let keep_root = last > 1 && widths[0] + widths[last] + 36. <= width;
            let first_hidden = usize::from(keep_root);
            let mut space = width - widths[last] - 36. - if keep_root { widths[0] } else { 0. };
            let mut end = last;
            while end > first_hidden + 1 && widths[end - 1] <= space {
                end -= 1;
                space -= widths[end];
            }
            let hidden = parts.drain(first_hidden..end).collect::<Vec<_>>();
            collapsed = Some((first_hidden, hidden));
        }
    }
    let last = parts.len().saturating_sub(1);

    let mut row = div()
        .id("breadcrumbs")
        .flex_1()
        .min_w(px(0.))
        .flex()
        .items_center()
        .gap(px(1.))
        .when(!app.settings.compact_paths, |row| row.overflow_x_scroll())
        .when(app.settings.compact_paths, |row| row.overflow_hidden());

    for (index, (label, crumb_path)) in parts.into_iter().enumerate() {
        let has_collapsed = collapsed.as_ref().is_some_and(|(position, _)| *position == index);
        if has_collapsed {
            if index > 0 { row = row.child(glyph("\u{E76C}", 7., p.dim)); }
            let (_, hidden) = collapsed.take().unwrap();
            row = row.child(div().id("address-hidden-folders").h(px(24.)).px(px(6.))
                .flex_shrink_0().flex().items_center().rounded(px(5.)).text_color(p.muted)
                .cursor_pointer().hover(|item| item.bg(p.card_hover).text_color(p.text))
                .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new(tr("Mostrar carpetas de la ruta", "Show folders in this path")).build(window, cx))
                .on_click(cx.listener(move |this: &mut XoolFiles, event: &ClickEvent, window, cx| {
                    let position = event.position();
                    this.open_context_menu(MenuTarget::AddressAncestors(hidden.clone()), f32::from(position.x),
                        f32::from(position.y + px(12.)), window, cx);
                    cx.stop_propagation();
                }))
                .child("…"));
        }
        if index > 0 || has_collapsed {
            row = row.child(glyph("\u{E76C}", 7., p.dim));
        }
        let active = index == last;
        let navigation_path = crumb_path.clone();
        let new_tab_path = crumb_path.clone();
        let context_path = crumb_path.clone();
        let crumb = div()
                .id(("crumb", index))
                .h(px(24.))
                .px(px(6.))
                .when(active && app.settings.compact_paths, |item| item.min_w(px(0.)).flex_shrink())
                .when(!active || !app.settings.compact_paths, |item| item.flex_shrink_0())
                .flex()
                .items_center()
                .rounded(px(5.))
                .text_size(px(11.5))
                .text_color(if active { p.text } else { p.muted })
                .when(active, |style| style.font_weight(FontWeight::MEDIUM))
                .hover(|style| style.bg(p.card_hover).text_color(p.text))
                .cursor_pointer()
                .on_click(
                    cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                        if active {
                            this.begin_address_edit(window, cx);
                        } else {
                            this.navigate(navigation_path.clone(), window, cx);
                        }
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_down(
                    MouseButton::Middle,
                    cx.listener(move |this: &mut XoolFiles, _: &MouseDownEvent, window, cx| {
                        this.open_with_middle_click(new_tab_path.clone(), window, cx);
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_down(MouseButton::Right, cx.listener(
                    move |this: &mut XoolFiles, event: &MouseDownEvent, window, cx| {
                        this.open_context_menu(MenuTarget::Address(context_path.clone()),
                            f32::from(event.position.x), f32::from(event.position.y), window, cx);
                        cx.stop_propagation();
                    },
                ))
                .child(div().min_w(px(0.)).truncate().child(SharedString::from(label)));
        let crumb = if fs_model::is_recycle_bin(&crumb_path) { crumb } else {
            folder_drop_target(crumb, crumb_path, None, p, cx)
        };
        row = row.child(crumb);
    }

    row.into_any_element()
}

// ---------------------------------------------------------------------------
// Paneles, pestañas y listas
// ---------------------------------------------------------------------------

fn pane_layout_view(
    app: &XoolFiles, layout: &PaneLayout, width: f32, height: f32,
    branch: &mut Vec<bool>, below_divider: bool, tabs_in_titlebar: bool, p: &Colors, cx: &mut Context<XoolFiles>,
) -> AnyElement {
    match layout {
        PaneLayout::Leaf(index) => {
            let has_tabs = !tabs_in_titlebar;
            let header = if has_tabs { PANE_TABBAR_HEIGHT } else { 0. };
            div().flex().flex_col().flex_shrink_0().w(px(width)).h(px(height)).overflow_hidden()
                .when(has_tabs, |element| {
                    element.child(pane_tabbar(app, *index, width, below_divider, p, cx))
                })
                .child(pane_view(app, *index, width, (height - header).max(0.), p, cx))
                .into_any_element()
        }
        PaneLayout::Split { axis, ratio, first, second } => {
            let across = *axis == Axis::Across;
            let extent = axis.extent(width, height);
            let (before, after) = split_extents(extent, *ratio);
            let divider = pane_divider(app, branch, *axis, extent, *ratio, p, cx);
            branch.push(false);
            let first = pane_layout_view(app, first, if across { before } else { width },
                if across { height } else { before }, branch, below_divider, tabs_in_titlebar, p, cx);
            *branch.last_mut().expect("nested branch") = true;
            let second = pane_layout_view(app, second, if across { after } else { width },
                if across { height } else { after }, branch, below_divider || !across, tabs_in_titlebar && across, p, cx);
            branch.pop();

            div().flex().flex_shrink_0().w(px(width)).h(px(height)).overflow_hidden()
                .when(!across, |element| element.flex_col())
                .child(first).child(divider).child(second).into_any_element()
        }
    }
}

fn pane_divider(
    app: &XoolFiles, branch: &[bool], axis: Axis, extent: f32, ratio: f32,
    p: &Colors, cx: &mut Context<XoolFiles>,
) -> AnyElement {
    let active = app.resizing_branch(branch);
    let across = axis == Axis::Across;
    let branch = branch.to_vec();
    let grip = px(DIVIDER.min(extent));
    let line = px(if active { 2. } else { 1. });
    div()
        .flex_shrink_0().flex().justify_center().items_center()
        .when(across, |style| style.w(grip).h_full().cursor(CursorStyle::ResizeLeftRight))
        .when(!across, |style| style.h(grip).w_full().cursor(CursorStyle::ResizeUpDown))
        .on_mouse_down(MouseButton::Left, cx.listener(
            move |this: &mut XoolFiles, event: &MouseDownEvent, _, cx| {
                let start = axis.extent(f32::from(event.position.x), f32::from(event.position.y));
                this.begin_split_resize(branch.clone(), axis, start, extent, ratio, cx);
                cx.stop_propagation();
            },
        ))
        .child(div()
            .when(across, |style| style.w(line).h_full())
            .when(!across, |style| style.h(line).w_full())
            .bg(if active { p.accent } else { p.border }))
        .into_any_element()
}

fn pane_view(
    app: &XoolFiles,
    pane_index: usize,
    pane_w: f32,
    pane_h: f32,
    p: &Colors,
    cx: &mut Context<XoolFiles>,
) -> AnyElement {
    let pane = &app.panes[pane_index];
    let active = pane.active.min(pane.tabs.len().saturating_sub(1));
    let active_id = pane.tabs[active].id;
    let is_active_pane = pane_index == app.active_pane;

    let file_area = file_area(app, pane_index, active_id, pane_w, p, cx);

    div()
        .id(("pane", pane_index))
        .flex()
        .flex_col()
        .w(px(pane_w))
        .h(px(pane_h))
        .flex_shrink_0()
        .min_w(px(0.))
        .min_h(px(0.))
        .when(app.panes.len() > 1, |style| {
            style.border_t_2().border_color(if is_active_pane { p.accent_faint } else { rgba(0x00000000).into() })
        })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this: &mut XoolFiles, _: &MouseDownEvent, window, cx| {
                if this.active_pane != pane_index {
                    this.activate_pane(pane_index, window, cx);
                } else {
                    let _ = window;
                }
            }),
        )
        .child(file_area)
        .into_any_element()
}

struct VisibleThumbnails {
    app: gpui::WeakEntity<XoolFiles>,
    tab_id: u64,
    columns: usize,
    gap: f32,
    grid_cell_width: Option<f32>,
}

impl gpui::UniformListDecoration for VisibleThumbnails {
    fn compute(
        &self,
        rows: Range<usize>,
        bounds: Bounds<Pixels>,
        scroll_offset: gpui::Point<Pixels>,
        item_height: Pixels,
        _row_count: usize,
        _window: &mut Window,
        cx: &mut gpui::App,
    ) -> AnyElement {
        let range = rows.start * self.columns..rows.end * self.columns;
        let _ = self.app.update(cx, |app, cx| {
            app.marquee_layouts.insert(self.tab_id, crate::marquee::Layout {
                viewport: bounds, offset: scroll_offset, row_height: item_height, rows,
                columns: self.columns, gap: self.gap, grid_cell_width: self.grid_cell_width,
            });
            app.load_visible_thumbnails(self.tab_id, range.clone(), cx);
            app.load_folder_counts(self.tab_id, range, cx);
        });
        div().into_any_element()
    }
}

fn file_area(
    app: &XoolFiles,
    pane_index: usize,
    tab_id: u64,
    pane_w: f32,
    p: &Colors,
    cx: &mut Context<XoolFiles>,
) -> AnyElement {
    let Some(tab) = app.tab_by_id(tab_id) else {
        return div().into_any_element();
    };

    let query = tab.search_query.clone();
    let tab_path = tab.path.clone();

    // El cuerpo cambia según el estado de la carpeta, pero el área que lo
    // contiene siempre existe para que el arrastre de pestañas funcione
    // también sobre carpetas vacías, con error o cargando.
    let body: AnyElement = if tab.loading && tab.entries.is_empty() && fs_model::is_recycle_bin(&tab.path) {
        div().flex_1().into_any_element()
    } else if tab.loading && tab.entries.is_empty() {
        empty_state(
            "\u{E895}",
            tr("Cargando carpeta…", "Loading folder…"),
            tr("Leyendo el contenido del disco.", "Reading the contents of the drive."),
            p,
            None,
        )
    } else if let Some(error) = tab.error.clone().filter(|_| tab.entries.is_empty()) {
        empty_state(
            "\u{E7BA}",
            tr("No se pudo abrir esta carpeta", "Could not open this folder"),
            &error,
            p,
            Some((
                tr("Reintentar", "Retry"),
                Box::new(cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.refresh_active(cx);
                })),
            )),
        )
    } else if tab.visible.is_empty() {
        if !query.trim().is_empty() {
            empty_state(
                "\u{E721}",
                tr("Sin coincidencias", "No matches"),
                tr("Prueba con otro término o activa la búsqueda en subcarpetas.", "Try a different term or include subfolders in your search."),
                p,
                Some((
                    tr("Limpiar búsqueda", "Clear search"),
                    Box::new(
                        cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                            this.clear_search(window, cx);
                        }),
                    ),
                )),
            )
        } else if fs_model::is_recycle_bin(&tab.path) {
            empty_state("\u{E8B7}", tr("Esta carpeta está vacía", "This folder is empty"), "", p, None)
        } else {
            empty_state(
                "\u{E8B7}",
                tr("Esta carpeta está vacía", "This folder is empty"),
                tr("Crea una carpeta o pega archivos para empezar.", "Create a folder or paste files to get started."),
                p,
                Some((
                    tr("Nueva carpeta", "New folder"),
                    Box::new(
                        cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                            this.open_new_folder_dialog(window, cx);
                        }),
                    ),
                )),
            )
        }
    } else {
        let view = tab.view;
        let compact = tab.compact;
        let show_ext = app.settings.show_extensions;

        match view {
            ViewMode::Grid => {
                let spacing = if compact && app.settings.compact_grid_tight { 3.0 } else { GRID_SPACING };
                let show_size = !compact || app.settings.compact_grid_show_size;
                let scale = if pane_index == app.active_pane { app.icon_scale_display } else { tab.icon_scale };
                let icon_size = crate::app::icon_size_for_scale(scale);
                let min_cell = (icon_size + 46.0).max(96.0);
                let inner_w = (pane_w - 24.0).max(120.0);
                let cols = (((inner_w + spacing) / (min_cell + spacing)).floor() as usize).max(1);
                let cell_w = (inner_w - spacing * (cols - 1) as f32) / cols as f32;
                let name_lines = usize::from(app.settings.grid_name_lines.clamp(1, 4));
                let row_h = grid_row_height(icon_size, compact, name_lines, show_size) + spacing;
                let row_count = tab.visible.len().div_ceil(cols);
                let handle = tab.scroll.clone();
                let colors = *p;

                let list = gpui::uniform_list(
                    ("grid-list", tab_id),
                    row_count,
                    cx.processor(
                        move |this: &mut XoolFiles, range: Range<usize>, window, cx| {
                            grid_rows(
                                this, tab_id, range, cols, cell_w, icon_size, row_h, compact, spacing, show_size, name_lines,
                                show_ext, &colors, window, cx,
                            )
                        },
                    ),
                )
                .with_decoration(VisibleThumbnails { app: cx.entity().downgrade(), tab_id, columns: cols, gap: spacing, grid_cell_width: Some(cell_w) })
                .track_scroll(handle)
                .h_full();

                div()
                    .id("grid-scroll-wrap")
                    .flex_1()
                    .min_h(px(0.))
                    .px(px(12.))
                    .pt(px(8.))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this: &mut XoolFiles, event: &MouseDownEvent, window, _| {
                        crate::marquee::begin(this, tab_id, event, window);
                    }))
                    .on_click(cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                        this.clear_selection(cx);
                    }))
                    .child(list)
                    .into_any_element()
            }
            mode @ (ViewMode::List | ViewMode::Tree | ViewMode::Details) => {
                let row_h = if compact {
                    compact_row_height(app)
                } else if mode == ViewMode::List { 25.0 } else { 30.0 };
                let handle = tab.scroll.clone();
                let sort = tab.sort;
                let sort_desc = tab.sort_desc;
                let widths = app::detail_widths(&app.settings, tab.detail_widths, pane_w);
                let header = (mode != ViewMode::List)
                    .then(|| list_header(app, sort, sort_desc, pane_index, pane_w, widths, p, cx));
                let columns = if mode == ViewMode::List {
                    app::compact_list_columns(pane_w)
                } else {
                    1
                };
                let row_count = tab.visible.len().div_ceil(columns);
                let colors = *p;

                let list = gpui::uniform_list(
                    ("list-list", tab_id),
                    row_count,
                    cx.processor(
                        move |this: &mut XoolFiles, range: Range<usize>, _window, cx| {
                            list_rows(
                                this, tab_id, range, row_h, columns, mode, widths, show_ext, &colors, cx,
                            )
                        },
                    ),
                )
                .with_decoration(VisibleThumbnails { app: cx.entity().downgrade(), tab_id, columns, gap: if columns > 1 { 8. } else { 0. }, grid_cell_width: None })
                .track_scroll(handle)
                .h_full();

                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .children(header)
                    .child(
                        div()
                            .id("list-scroll-wrap")
                            .flex_1()
                            .min_h(px(0.))
                            .px(px(8.))
                            .pt(px(4.))
                            .on_mouse_down(MouseButton::Left, cx.listener(move |this: &mut XoolFiles, event: &MouseDownEvent, window, _| {
                                crate::marquee::begin(this, tab_id, event, window);
                            }))
                            .on_click(cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                                this.clear_selection(cx);
                            }))
                            .child(list),
                    )
                    .into_any_element()
            }
        }
    };

    let mut area = div()
        .id(("file-area", pane_index))
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.))
        .w_full()
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this: &mut XoolFiles, event: &MouseDownEvent, window, cx| {
                this.open_pane_context_menu(tab_id, MenuTarget::Background, event, window, cx);
                cx.stop_propagation();
            }),
        )
        .child(body);

    area = attach_drop_target(area, tab_path, p, cx)
        .on_drag_move::<DragTab>(cx.listener(
            move |this: &mut XoolFiles, event: &gpui::DragMoveEvent<DragTab>, _, cx| {
                let can_drop = this.tab_position(event.drag(cx).tab_id)
                    .is_some_and(|(source, _)| this.can_drop_tab_in_pane(source, pane_index));
                let zone = (can_drop && event.bounds.contains(&event.event.position))
                    .then(|| tab_drop_zone(event.bounds, event.event.position, 0.30));
                update_pane_drop(this, pane_index, zone, None, cx);
            },
        ))
        .on_drop::<DragTab>(cx.listener(
            move |this: &mut XoolFiles, drag: &DragTab, window, cx| {
                let zone = if this.tab_drop_pane == Some(pane_index) {
                    this.tab_drop_zone.unwrap_or(TabDropZone::Center)
                } else {
                    TabDropZone::Center
                };
                this.drop_tab(drag, pane_index, zone, window, cx);
                this.finish_drag();
                cx.notify();
            },
        ))
        .on_drag_move::<DragFiles>(cx.listener(
            move |this: &mut XoolFiles, event: &gpui::DragMoveEvent<DragFiles>, _, cx| {
                let zone = if event.drag(cx).folder && event.bounds.contains(&event.event.position) {
                    let zone = tab_drop_zone(event.bounds, event.event.position, 0.20);
                    (zone != TabDropZone::Center).then_some(zone)
                } else {
                    None
                };
                update_pane_drop(this, pane_index, zone, Some(event.bounds), cx);
            },
        ));

    if let Some(zone) = app.tab_drop_zone.filter(|_| app.tab_drop_pane == Some(pane_index)) {
        let overlay = tab_drop_overlay(zone, p);
        area = area.child(if let Some(bounds) = app.folder_drop_bounds {
            overlay.occlude().on_drop::<DragFiles>(cx.listener(
                move |this: &mut XoolFiles, files: &DragFiles, window, cx| {
                    let position = window.mouse_position();
                    if bounds.contains(&position) {
                        let zone = tab_drop_zone(bounds, position, 0.20);
                        this.drop_folder_split(files, pane_index, zone, window, cx);
                    }
                    this.finish_drag();
                    cx.notify();
                },
            )).into_any_element()
        } else {
            overlay.into_any_element()
        });
    }

    if tab.loading && !tab.entries.is_empty() {
        area = area.child(
            div()
                .absolute()
                .top(px(8.))
                .right(px(12.))
                .px(px(8.))
                .py(px(3.))
                .rounded(px(6.))
                .bg(p.panel)
                .border_1()
                .border_color(p.border)
                .text_size(px(9.))
                .text_color(p.muted)
                .child(tr("Actualizando…", "Refreshing…")),
        );
    }

    if !query.trim().is_empty() {
        area = area.child(
            div()
                .absolute()
                .bottom(px(8.))
                .left(px(12.))
                .px(px(8.))
                .py(px(3.))
                .rounded(px(6.))
                .bg(p.panel)
                .border_1()
                .border_color(p.border)
                .text_size(px(9.))
                .text_color(p.dim)
                .child(crate::localized!(format,
                    "{} resultado(s) para «{}»", "{} result(s) for “{}”",
                    tab.visible.len(),
                    query
                )),
        );
    }

    area.into_any_element()
}

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static>;

fn empty_state(
    icon: &'static str,
    title: &str,
    subtitle: &str,
    p: &Colors,
    action: Option<(&'static str, ClickHandler)>,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.))
        .w_full()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .px(px(30.))
        .child(
            div()
                .w(px(62.))
                .h(px(62.))
                .rounded(px(18.))
                .bg(p.card)
                .border_1()
                .border_color(p.border_soft)
                .flex()
                .items_center()
                .justify_center()
                .child(glyph(icon, 24., p.dim)),
        )
        .child(
            div()
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.text)
                .child(SharedString::from(title.to_string())),
        )
        .when(!subtitle.is_empty(), |state| state.child(
            div()
                .max_w(px(330.))
                .text_size(px(10.5))
                .text_color(p.muted)
                .text_center()
                .child(SharedString::from(subtitle.to_string())),
        ))
        .when_some(action, |this, (label, handler)| {
            this.child(
                div()
                    .id("empty-action")
                    .mt(px(4.))
                    .h(px(30.))
                    .px(px(12.))
                    .rounded(px(8.))
                    .bg(p.accent)
                    .text_color(p.accent_fg)
                    .text_size(px(10.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .hover(|style| style.bg(p.accent_hover))
                    .on_click(handler)
                    .child(label),
            )
        })
        .into_any_element()
}

fn list_header(
    app: &XoolFiles,
    sort: SortKey,
    sort_desc: bool,
    pane_index: usize,
    pane_w: f32,
    widths: [f32; DETAIL_COLUMNS],
    p: &Colors,
    cx: &mut Context<XoolFiles>,
) -> AnyElement {
    let mut header = div().id("detail-header")
        .h(px(28.)).w_full().flex_shrink_0().flex().items_center()
        .px(px(16.)).border_b_1().border_color(p.border_soft)
        .text_size(px(9.)).font_weight(FontWeight::SEMIBOLD).text_color(p.dim)
        .on_mouse_down(MouseButton::Right, cx.listener(
            |this: &mut XoolFiles, event: &MouseDownEvent, _, cx| {
                this.close_context_menu(cx);
                this.columns_menu = Some((f32::from(event.position.x), f32::from(event.position.y)));
                cx.notify();
                cx.stop_propagation();
            },
        ))
        .child(header_cell("NOMBRE", SortKey::Name, None, sort, sort_desc, pane_index, p, cx));
    for &column in &app.settings.detail_column_order {
        let index = column.index();
        if widths[index] == 0.0 { continue; }
        let active = app.column_resize.as_ref().is_some_and(|drag| drag.index == index && drag.tab_id == app.panes[pane_index].active_tab().id);
        header = header.child(
            div().id(("column-divider", index)).w(px(10.)).h_full().flex_shrink_0()
                .flex().items_center().justify_center().cursor(CursorStyle::ResizeLeftRight)
                .hover(|style| style.bg(p.accent_soft))
                .child(div().w(px(1.)).h(px(17.)).bg(if active { p.accent } else { p.border }))
                .on_mouse_down(MouseButton::Left, cx.listener(
                    move |this: &mut XoolFiles, event: &MouseDownEvent, _, cx| {
                        this.begin_column_resize(pane_index, index, pane_w, widths, f32::from(event.position.x), cx);
                        cx.stop_propagation();
                    },
                )),
        ).child(if let Some(key) = column.sort_key() {
            header_cell(column.label(), key, Some(widths[index]), sort, sort_desc, pane_index, p, cx)
        } else {
            div().w(px(widths[index])).flex_shrink_0().px(px(4.)).truncate()
                .child(column.label()).into_any_element()
        });
    }
    header.into_any_element()
}

fn header_cell(
    label: &'static str,
    key: SortKey,
    width: Option<f32>,
    sort: SortKey,
    sort_desc: bool,
    pane_index: usize,
    p: &Colors,
    cx: &mut Context<XoolFiles>,
) -> AnyElement {
    let active = sort == key;
    let mut cell = div()
        .id(("header", sort_index(key)))
        .h(px(22.))
        .px(px(4.))
        .rounded(px(4.))
        .flex()
        .items_center()
        .gap(px(4.))
        .text_color(if active { p.text } else { p.dim })
        .hover(|style| style.bg(p.card_hover).text_color(p.text))
        .cursor_pointer()
        .on_click(
            cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                // Ordenar desde la cabecera debe aplicarse al panel pulsado,
                // aunque no sea el panel activo.
                if this.active_pane != pane_index {
                    this.activate_pane(pane_index, window, cx);
                }
                this.perform_menu_action(MenuAction::SetSort(key), window, cx);
            }),
        );

    cell = match width {
        Some(width) => cell.w(px(width)).flex_shrink_0().min_w(px(0.)).overflow_hidden(),
        None => cell.flex_1().min_w(px(80.)),
    };

    cell = cell.child(label);
    if active {
        cell = cell.child(glyph(
            if sort_desc { "\u{E70D}" } else { "\u{E70E}" },
            7.,
            p.accent,
        ));
    }
    cell.into_any_element()
}

fn sort_index(key: SortKey) -> usize {
    match key {
        SortKey::Name => 0,
        SortKey::Modified => 1,
        SortKey::Kind => 2,
        SortKey::Size => 3,
    }
}

fn grid_name_size(icon_size: f32) -> f32 {
    (icon_size * 0.19).clamp(9.5, 14.0)
}

fn grid_name_line(icon_size: f32) -> f32 {
    (grid_name_size(icon_size) * 1.35).ceil()
}

fn grid_meta_size(icon_size: f32) -> f32 {
    (grid_name_size(icon_size) * 0.82).clamp(8.0, 10.5)
}

fn grid_meta_line(icon_size: f32) -> f32 {
    (grid_meta_size(icon_size) * 1.3).ceil()
}

/// Altura uniforme de una celda de cuadrícula. Reserva espacio para el
/// nombre y una para el tamaño, de modo que los textos nunca se recorten por
/// falta de espacio al cambiar el zoom.
fn grid_row_height(icon_size: f32, compact: bool, max_lines: usize, show_size: bool) -> f32 {
    let pad = if compact { 4.0 } else { 5.0 };
    let gap = if compact { 4.0 } else { 5.0 };
    let name_h = grid_name_line(icon_size) * max_lines as f32;
    let meta_h = if show_size { grid_meta_line(icon_size) } else { 0.0 };
    // El peso va pegado al nombre: un solo hueco entre el icono y el texto.
    (pad * 2.0 + gap + icon_size + name_h + meta_h + 2.0).ceil()
}

fn measure_grid_text(text: &str, font_size: f32, window: &Window) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    let style = window.text_style();
    let run = style.to_run(text.len());
    let layout = window
        .text_system()
        .layout_line(text, px(font_size), &[run], None);
    f32::from(layout.width)
}

/// Reparte el nombre en dos líneas que quepan en el ancho de la celda. La
/// segunda línea se recorta con puntos suspensivos si el nombre sigue siendo
/// demasiado largo, de modo que nunca se sale de la celda ni desaparece.
fn split_grid_name(
    name: &str,
    max_width: f32,
    font_size: f32,
    window: &Window,
) -> (String, String) {
    if max_width <= 1.0 || measure_grid_text(name, font_size, window) <= max_width {
        return (name.to_string(), String::new());
    }

    let mut split = None;
    for (index, ch) in name.char_indices() {
        if ch != ' ' {
            continue;
        }
        let head = name[..index].trim_end();
        if head.is_empty() {
            continue;
        }
        if measure_grid_text(head, font_size, window) <= max_width {
            split = Some(index);
        } else {
            break;
        }
    }

    let split = split.unwrap_or_else(|| {
        let mut end = 0;
        for (index, ch) in name.char_indices() {
            let next = index + ch.len_utf8();
            if measure_grid_text(&name[..next], font_size, window) <= max_width {
                end = next;
            } else {
                break;
            }
        }
        if end == 0 {
            name.chars().next().map(|ch| ch.len_utf8()).unwrap_or(0)
        } else {
            end
        }
    });

    let head = name[..split].trim_end().to_string();
    let tail = name[split..].trim_start().to_string();
    (head, tail)
}

fn grid_rows(
    app: &mut XoolFiles,
    tab_id: u64,
    range: Range<usize>,
    cols: usize,
    cell_w: f32,
    icon_size: f32,
    row_h: f32,
    compact: bool,
    spacing: f32,
    show_size: bool,
    max_lines: usize,
    show_ext: bool,
    p: &Colors,
    window: &Window,
    cx: &mut Context<XoolFiles>,
) -> Vec<AnyElement> {
    let Some(tab) = app.tab_by_id(tab_id) else {
        return Vec::new();
    };
    let rename_path = app.rename_path.clone().filter(|_| app.rename_tab == Some(tab_id));
    let rename_input = app.rename_input.clone();
    let selection_paths = OnceCell::<Arc<[PathBuf]>>::new();
    let selection = &tab.selection;

    let mut rows = Vec::new();
    for row_index in range {
        let start = row_index * cols;
        let mut cells = Vec::new();
        for slot in 0..cols {
            let visible_index = start + slot;
            let entry = tab
                .visible
                .get(visible_index)
                .and_then(|index| tab.entries.get(*index));

            match entry {
                Some(entry) => {
                    let selected = selection.contains(&entry.path);
                    let renaming = rename_path.as_deref() == Some(entry.path.as_path());
                    let drag_paths = if selected {
                        selection_paths.get_or_init(|| selection.iter().cloned().collect()).clone()
                    } else {
                        Arc::from([entry.path.clone()])
                    };
                    cells.push(grid_cell(
                        entry,
                        selected,
                        app.is_cut(&entry.path),
                        show_ext,
                        app.settings.hover_details.then_some(app.settings.hover_fields),
                        cell_w,
                        icon_size,
                        row_h - spacing,
                        compact,
                        show_size,
                        max_lines,
                        tab_id,
                        visible_index,
                        drag_paths,
                        renaming,
                        &rename_input,
                        p,
                        window,
                        cx,
                    ));
                }
                None => {
                    cells.push(
                        div()
                            .w(px(cell_w))
                            .h(px(row_h))
                            .flex_shrink_0()
                            .into_any_element(),
                    );
                }
            }
        }
        rows.push(
            div()
                .w_full()
                .h(px(row_h))
                .flex()
                .items_start()
                .gap(px(spacing))
                .children(cells)
                .into_any_element(),
        );
    }
    rows
}

fn grid_cell(
    entry: &Entry,
    selected: bool,
    cut: bool,
    show_ext: bool,
    hover_fields: Option<crate::settings::HoverFields>,
    cell_w: f32,
    icon_size: f32,
    row_h: f32,
    compact: bool,
    show_size: bool,
    max_lines: usize,
    tab_id: u64,
    visible_index: usize,
    drag_paths: Arc<[PathBuf]>,
    renaming: bool,
    rename_input: &Entity<InputState>,
    p: &Colors,
    window: &Window,
    cx: &mut Context<XoolFiles>,
) -> AnyElement {
    let uid = tab_id as usize * 100_000 + visible_index;
    let path = entry.path.clone();
    let path_menu = path.clone();
    let path_hover = path.clone();
    let path_middle = path.clone();
    let name = entry.display_name(show_ext);
    let is_dir = entry.is_dir;
    let kind = entry.kind;
    let extension = entry.extension();
    let thumb = entry.thumb.clone();
    let drag_colors = *p;
    let view = cx.entity().downgrade();
    let pad = if compact { 4.0 } else { 5.0 };
    let gap = if compact { 4.0 } else { 5.0 };
    let name_size = grid_name_size(icon_size);
    let name_line = grid_name_line(icon_size);
    let name_w = (cell_w - pad * 2.0 - 4.0).max(0.0);
    let mut lines = Vec::with_capacity(max_lines);
    let mut remaining = name.clone();
    for _ in 1..max_lines {
        let (line, tail) = split_grid_name(&remaining, name_w, name_size, window);
        lines.push(line);
        remaining = tail;
        if remaining.is_empty() { break; }
    }
    if !remaining.is_empty() || lines.is_empty() { lines.push(remaining); }
    // Si el nombre cabe en una línea no se reserva la segunda: el peso sube
    // justo debajo del nombre y la celda queda más compacta.
    let name_h = name_line * lines.len() as f32;
    let meta_size = grid_meta_size(icon_size);
    let meta_line = grid_meta_line(icon_size);

    let name_block = if renaming {
        let text_width = measure_grid_text(&lines[0], name_size, window);
        let inset = ((name_w - text_width) / 2.0).max(0.0);
        div()
            .id(("rename-cell", uid))
            .w(px((cell_w - pad * 2.0).max(0.)))
            .h(px(name_h))
            .pl(px(2.0 + inset))
            .pr(px(2.))
            .flex_shrink_0()
            .on_click(cx.listener(|_: &mut XoolFiles, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
            }))
            .child(
                Input::new(rename_input)
                    .appearance(false)
                    .bordered(false)
                    .p(px(0.))
                    .text_size(px(name_size))
                    .text_color(p.text)
                    .line_height(px(name_line))
                    .items_start()
                    .h(px(name_line))
                    .w_full(),
            )
    } else {
        div()
            .id(("rename-cell-name", uid))
            .w(px((cell_w - pad * 2.0).max(0.)))
            .h(px(name_h))
            .px(px(2.))
            .flex()
            .flex_col()
            .children(lines.into_iter().map(|line| {
                div().w_full().h(px(name_line)).flex_shrink_0()
                    .text_size(px(name_size)).line_height(px(name_line))
                    .truncate().text_center().text_color(if selected { p.text } else { p.muted })
                    .child(SharedString::from(line))
            }))
    };

    let mut cell = div()
        .id(("cell", uid))
        .w(px(cell_w))
        .h(px(row_h))
        .flex_shrink_0()
        .p(px(pad))
        .flex()
        .flex_col()
        .items_center()
        .overflow_hidden()
        .rounded(px(10.))
        .when_some(hover_fields.filter(|fields| fields.any()), |cell, fields| cell.tooltip(file_details(entry, *p, fields)))
        // El borde se reserva siempre para que el contenido no se desplace
        // al seleccionar la celda; solo cambia su color.
        .border_1()
        .border_color(if selected {
            p.accent_soft
        } else {
            rgba(0x00000000)
        })
        .when(selected, |style| style.bg(p.selection))
        .when(!selected, |style| {
            style.hover(|style| style.bg(p.card_hover))
        })
        .cursor_pointer()
        .on_drag(
            DragFiles { folder: is_dir && drag_paths.len() == 1, paths: drag_paths },
            move |files, _offset, window, cx| {
                let task = start_native_drag(files, window, cx);
                let _ = view.update(cx, |this, _| this.native_drag_task = Some(task));
                drag_ghost(files, drag_colors, cx)
            },
        )
        .on_click(
            cx.listener(move |this: &mut XoolFiles, event: &ClickEvent, window, cx| {
                let single_click = this.settings.single_click_files && !this.in_recycle_bin()
                    && this.active_tab().entry_for(&path).is_some_and(|entry| !entry.is_dir && !entry.folder_shortcut);
                if if single_click { event.click_count() == 1 && !event.modifiers().modified() }
                    else { event.click_count() >= 2 } {
                    this.activate_entry(path.clone(), event.modifiers(), window, cx);
                } else {
                    this.click_file_view_entry(path.clone(), event.modifiers(), window, cx);
                }
                cx.stop_propagation();
            }),
        )
        .on_hover(cx.listener(move |this: &mut XoolFiles, hovering: &bool, window, cx| {
            this.hover_file_context_menu(tab_id, &path_hover, *hovering, window, cx);
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(
                move |this: &mut XoolFiles, event: &MouseDownEvent, window, cx| {
                    this.open_pane_context_menu(
                        tab_id, MenuTarget::Item(path_menu.clone()), event, window, cx,
                    );
                    cx.stop_propagation();
                },
            ),
        )
        .on_mouse_down(
            MouseButton::Middle,
            cx.listener(move |this: &mut XoolFiles, _: &MouseDownEvent, window, cx| {
                if is_dir {
                    this.open_from_file_pane(tab_id, path_middle.clone(), window, cx);
                }
                cx.stop_propagation();
            }),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .w(px((cell_w - pad * 2.0).max(0.)))
                .h(px(icon_size))
                .mb(px(gap))
                .child(visual(
                    entry,
                    thumb,
                    is_dir,
                    kind,
                    &extension,
                    icon_size,
                    (cell_w - pad * 2.0).max(0.),
                )),
        )
        .child(name_block)
        .when(!is_dir && show_size, |style| {
            style.child(
                div()
                    .w(px((cell_w - pad * 2.0).max(0.)))
                    .h(px(meta_line))
                    .px(px(2.))
                    .text_size(px(meta_size))
                    .line_height(px(meta_line))
                    .truncate()
                    .text_center()
                    .text_color(p.dim)
                    .child(SharedString::from(entry.size_label()))
            )
        });

    if is_dir {
        cell = folder_drop_target(cell, entry.path.clone(), Some(tab_id), p, cx);
    }

    cell.when(cut, |cell| cell.opacity(0.48)).into_any_element()
}

fn list_rows(
    app: &mut XoolFiles,
    tab_id: u64,
    range: Range<usize>,
    row_h: f32,
    columns: usize,
    mode: ViewMode,
    widths: [f32; DETAIL_COLUMNS],
    show_ext: bool,
    p: &Colors,
    cx: &mut Context<XoolFiles>,
) -> Vec<AnyElement> {
    let Some(tab) = app.tab_by_id(tab_id) else {
        return Vec::new();
    };
    let rename_path = app.rename_path.clone().filter(|_| app.rename_tab == Some(tab_id));
    let rename_input = app.rename_input.clone();
    let selection_paths = OnceCell::<Arc<[PathBuf]>>::new();
    let selection = &tab.selection;
    let (stripe, hover, highlight) = if app.settings.translucent_background {
        (
            Rgba { a: 0.32, ..p.panel_soft },
            Rgba { a: 0.58, ..p.card_hover },
            Rgba { a: p.selection.a.min(0.72), ..p.selection },
        )
    } else {
        (p.panel_soft, p.card_hover, p.selection)
    };

    let mut rows = Vec::new();
    for visible_index in range.start * columns..range.end * columns {
        let Some(entry) = tab
            .visible
            .get(visible_index)
            .and_then(|index| tab.entries.get(*index))
        else {
            continue;
        };
        let selected = selection.contains(&entry.path);
        let path = entry.path.clone();
        let path_menu = path.clone();
        let path_hover = path.clone();
        let path_middle = path.clone();
        let name = entry.display_name(show_ext);
        let is_dir = entry.is_dir;
        let kind = entry.kind;
        let extension = entry.extension();
        // En la lista las carpetas conservan su icono; la miniatura de
        // contenido se reserva para la cuadrícula.
        let thumb = if is_dir { None } else { entry.thumb.clone() };
        let uid = tab_id as usize * 100_000 + visible_index;
        let renaming = rename_path.as_deref() == Some(entry.path.as_path());
        let drag_paths = if selected {
            selection_paths.get_or_init(|| selection.iter().cloned().collect()).clone()
        } else {
            Arc::from([entry.path.clone()])
        };
        let drag_colors = *p;
        let view = cx.entity().downgrade();
        let tree = mode == ViewMode::Tree;
        let depth = if tree { tab.tree_depth(entry).min(20) } else { 0 };
        let branch_path = entry.path.clone();
        let expandable = tree && is_dir && !tab.recursive_search && !fs_model::is_recycle_bin(&tab.path);
        let expanded = expandable && tab.tree_expanded.contains(&branch_path);

        let mut row = div()
            .id(("row", uid))
            .h(px(row_h))
            .when(columns == 1, |row| row.w_full())
            .when(columns > 1, |row| row.flex_1().min_w(px(0.)).overflow_hidden())
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(8.))
            .rounded(px(7.))
            .when(app.settings.hover_details && app.settings.hover_fields.any(), |row| row.tooltip(file_details(entry, *p, app.settings.hover_fields)))
            .when(selected, |style| style.bg(highlight))
            .when(!selected, |style| {
                style.when(visible_index % 2 == 1, |style| style.bg(stripe))
                    .hover(move |style| style.bg(hover))
            })
            .cursor_pointer()
            .on_drag(
                DragFiles { folder: is_dir && drag_paths.len() == 1, paths: drag_paths },
                move |files, _offset, window, cx| {
                    let task = start_native_drag(files, window, cx);
                    let _ = view.update(cx, |this, _| this.native_drag_task = Some(task));
                    drag_ghost(files, drag_colors, cx)
                },
            )
            .on_click(
                cx.listener(move |this: &mut XoolFiles, event: &ClickEvent, window, cx| {
                    let single_click = this.settings.single_click_files && !this.in_recycle_bin()
                    && this.active_tab().entry_for(&path).is_some_and(|entry| !entry.is_dir && !entry.folder_shortcut);
                if if single_click { event.click_count() == 1 && !event.modifiers().modified() }
                    else { event.click_count() >= 2 } {
                        this.activate_entry(path.clone(), event.modifiers(), window, cx);
                    } else {
                        this.click_file_view_entry(path.clone(), event.modifiers(), window, cx);
                    }
                    cx.stop_propagation();
                }),
            )
            .on_hover(cx.listener(move |this: &mut XoolFiles, hovering: &bool, window, cx| {
                this.hover_file_context_menu(tab_id, &path_hover, *hovering, window, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(
                    move |this: &mut XoolFiles, event: &MouseDownEvent, window, cx| {
                        this.open_pane_context_menu(
                        tab_id, MenuTarget::Item(path_menu.clone()), event, window, cx,
                    );
                        cx.stop_propagation();
                    },
                ),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this: &mut XoolFiles, _: &MouseDownEvent, window, cx| {
                    if is_dir {
                        this.open_from_file_pane(tab_id, path_middle.clone(), window, cx);
                    }
                    cx.stop_propagation();
                }),
            )
            .when(tree, |row| row.child(
                div().w(px(depth as f32 * 14.0 + 16.0)).h_full()
                    .flex_shrink_0().flex().items_center().justify_end()
                    .when(expandable, |slot| slot.child(
                        div().id(("tree-expand", uid)).w(px(16.)).h(px(20.))
                            .rounded(px(4.)).flex().items_center().justify_center()
                            .cursor_pointer().hover(|style| style.bg(p.card_hover))
                            .child(glyph(if expanded { "\u{E70D}" } else { "\u{E76C}" }, 9., p.muted))
                            .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                                this.toggle_tree_folder(tab_id, branch_path.clone(), cx);
                                cx.stop_propagation();
                            })),
                    )),
            ))
            .child(
                div()
                    .w(px(22.))
                    .h(px(22.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(visual(entry, thumb, is_dir, kind, &extension, 22., 22.)),
            )
            .child(
                if renaming {
                    div()
                        .id(("rename-row", uid))
                        .flex_1()
                        .min_w(px(80.))
                        .on_click(cx.listener(|_: &mut XoolFiles, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                        }))
                        .child(
                            Input::new(&rename_input)
                                .appearance(false)
                                .bordered(false)
                                .p(px(0.))
                                .text_size(px(11.5))
                                .text_color(p.text)
                                .line_height(px(16.))
                                .h(px(16.)),
                        )
                        .into_any_element()
                } else {
                    div()
                        .flex_1()
                        .min_w(px(80.))
                        .text_size(px(11.5))
                        .line_height(px(16.))
                        .text_color(if selected { p.text } else { p.muted })
                        .text_ellipsis()
                        .child(SharedString::from(name))
                        .into_any_element()
                },
            )
            .when(mode != ViewMode::List, |row| row.children(
                app.settings.detail_column_order.iter().filter_map(|&column| {
                    let width = widths[column.index()];
                    (width > 0.).then(|| {
                        if DetailLabel::choices(column).is_some() {
                            detail_label_cell(app, &entry.path, column, width, p, cx)
                        } else {
                            detail_cell(
                                detail_value(entry, column, tab.folder_counts.get(&entry.path).copied().flatten()),
                                width, column == DetailColumn::Folders, p,
                            ).into_any_element()
                        }
                    })
                })
            ));

        if is_dir {
            row = folder_drop_target(row, entry.path.clone(), Some(tab_id), p, cx);
        }

        rows.push(row.when(app.is_cut(&entry.path), |row| row.opacity(0.48)).into_any_element());
    }
    if columns == 1 {
        return rows;
    }
    let row_count = rows.len().div_ceil(columns);
    let mut cells = rows.into_iter();
    (0..row_count).map(|_| {
        let mut row = div().w_full().h(px(row_h)).flex().gap(px(8.));
        for _ in 0..columns {
            row = row.child(cells.next().unwrap_or_else(|| div().flex_1().into_any_element()));
        }
        row.into_any_element()
    }).collect()
}

fn detail_value(entry: &Entry, column: DetailColumn, folders: Option<usize>) -> String {
    match column {
        DetailColumn::Modified => entry.modified_label(),
        DetailColumn::Kind => {
            if entry.is_dir {
                return tr("Carpeta", "Folder").into();
            }
            let extension = entry.extension();
            if extension.is_empty() {
                "—".into()
            } else {
                extension.to_uppercase()
            }
        }
        DetailColumn::Size => entry.size_label(),
        DetailColumn::Created => entry.created.map_or_else(|| "—".into(), fs_model::format_date),
        DetailColumn::Accessed => entry.accessed.map_or_else(|| "—".into(), fs_model::format_date),
        DetailColumn::Status | DetailColumn::Priority => String::new(),
        DetailColumn::Folders => folders.map_or_else(|| "—".into(), |count| count.to_string()),
    }
}

fn columns_popup(
    app: &XoolFiles, x: f32, y: f32, vw: f32, vh: f32, p: &Colors, cx: &mut Context<XoolFiles>,
) -> AnyElement {
    let (x, width) = context_menu_axis(x, vw, 252.);
    let (y, height) = context_menu_axis(y, vh, 12. + 30. * (DETAIL_COLUMNS + 1) as f32);
    div().id("detail-columns-popup").absolute().left(px(x)).top(px(y)).w(px(width)).max_h(px(height))
        .overflow_y_scroll().occlude().p(px(6.)).rounded(px(9.))
        .border_1().border_color(p.border).bg(p.panel).shadow_xl()
        .on_mouse_down_out(cx.listener(|this: &mut XoolFiles, _: &MouseDownEvent, _, cx| {
            this.columns_menu = None;
            this.preview_column_order(None);
            cx.notify();
        }))
        .child(div().h(px(30.)).px(px(8.)).flex().items_center().gap(px(8.))
            .text_color(p.dim).text_size(px(11.))
            .child(glyph("\u{E73E}", 11., p.accent)).child(tr("Nombre", "Name")))
        .children(app.detail_column_order().into_iter().enumerate().map(|(slot, column)| {
            let on = app.settings.detail_column_visible[column.index()];
            let shifted = app.column_offset(column);
            let p = *p;
            div().id(("detail-column-slot", slot)).h(px(30.)).flex_shrink_0()
                .on_drag_move::<DetailColumn>(cx.listener(
                    move |this: &mut XoolFiles, event: &gpui::DragMoveEvent<DetailColumn>, _, cx| {
                        let target = (*event.drag(cx), slot);
                        if event.bounds.contains(&event.event.position) && this.column_drag_target != Some(target) {
                            this.preview_column_order(Some(target));
                            cx.notify();
                        }
                    },
                ))
                .on_drop(cx.listener(move |this: &mut XoolFiles, column: &DetailColumn, _, cx| {
                    this.commit_column_order(*column, slot, cx);
                }))
                .child(div().id(("detail-column-toggle", column.index())).relative().top(px(shifted))
                    .h_full().px(px(8.)).flex().items_center().gap(px(8.)).rounded(px(5.))
                    .text_size(px(11.)).cursor_pointer().hover(move |row| row.bg(p.card_hover))
                    .when(app.column_drag_target.is_some_and(|(moving, _)| moving == column), |row| row.opacity(0.45))
                    .child(div().w(px(11.)).flex_shrink_0().when(on, |mark| mark.child(glyph("\u{E73E}", 11., p.accent))))
                    .child(div().flex_1().child(column.label()))
                    .child(glyph("\u{E700}", 11., p.dim))
                    .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                        this.settings.detail_column_visible[column.index()] = !this.settings.detail_column_visible[column.index()];
                        this.settings.save();
                        cx.notify();
                    }))
                    .on_drag(column, move |column, _, _, cx| {
                        cx.new(|_| DragGhost { label: column.label().into(), count: 1, colors: p, icon: "\u{E700}" })
                    }))
        }))
        .into_any_element()
}

fn detail_cell(value: String, width: f32, align_right: bool, p: &Colors) -> Div {
    div().w(px(width)).flex_shrink_0().text_size(px(10.5)).text_color(p.dim)
        .text_ellipsis().when(align_right, |cell| cell.text_right())
        .child(SharedString::from(value))
}

fn detail_label_chip(label: DetailLabel, p: &Colors) -> Div {
    let (name, color) = label.appearance();
    let color = rgb(color);
    div().min_w(px(0.)).max_w_full().px(px(6.)).h(px(19.)).rounded_full()
        .bg(Rgba { a: 0.24, ..color }).border_1().border_color(Rgba { a: 0.65, ..color })
        .text_color(p.text).text_size(px(10.)).flex().items_center().gap(px(5.))
        .child(div().size(px(6.)).flex_shrink_0().rounded_full().bg(color))
        .child(div().min_w(px(0.)).truncate().child(name))
}

struct DetailLabelDrag {
    path: PathBuf,
    column: DetailColumn,
}

struct DetailLabelGhost(DetailLabel, Colors);

impl gpui::Render for DetailLabelGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child(detail_label_chip(self.0, &self.1).bg(self.1.panel).shadow_md())
            .with_animation("label-drag", Animation::new(Duration::from_millis(130)).with_easing(ease_out_quint()),
                |ghost, progress| ghost.relative().top(px(-4. * progress)).opacity(0.65 + 0.35 * progress))
    }
}

fn detail_label_cell(
    app: &XoolFiles, path: &std::path::Path, column: DetailColumn, width: f32, p: &Colors,
    cx: &mut Context<XoolFiles>,
) -> AnyElement {
    let value = app.settings.file_labels.get(path).and_then(|labels| labels.get(column));
    let arrival = app.detail_label_arrival.as_ref().filter(|(_, from, to, moved)| {
        *moved == column && (path == from || path == to)
    }).map(|(version, _, _, _)| *version);
    let path = path.to_path_buf();
    let destination = path.clone();
    let colors = *p;
    let content = if let Some(label) = value {
        let chip = detail_label_chip(label, p);
        if let Some(version) = arrival {
            chip.with_animation(("detail-label-arrival", version),
                Animation::new(Duration::from_millis(185)).with_easing(ease_out_quint()),
                |chip, progress| chip.relative().top(px(-7. * (1. - progress))).opacity(progress))
                .into_any_element()
        } else { chip.into_any_element() }
    } else {
        div().text_size(px(10.)).text_color(p.dim).child("—").into_any_element()
    };
    div().id(("file-label", column.index())).w(px(width)).h_full().min_w(px(0.)).flex_shrink_0()
        .flex().items_center().px(px(4.)).rounded(px(4.)).cursor_pointer()
        // Keep the pointer in this cell so the parent row cannot start a file drag or selection.
        .block_mouse_except_scroll()
        .hover(|style| style.bg(p.card_hover))
        .child(content)
        .when_some(value, |cell, label| {
            cell.cursor(CursorStyle::OpenHand)
                .on_drag(DetailLabelDrag { path: path.clone(), column }, move |_, _, _, cx| {
                    cx.new(|_| DetailLabelGhost(label, colors))
                })
        })
        .drag_over::<DetailLabelDrag>({
            let destination = destination.clone();
            move |style, drag, _, _| {
                if drag.column == column && !crate::workspaces::same_path(&drag.path, &destination) {
                    style.bg(colors.accent_soft)
                } else { style }
            }
        })
        .on_drop(cx.listener(move |this: &mut XoolFiles, drag: &DetailLabelDrag, _, cx| {
            if drag.column == column && this.settings.move_file_label(&drag.path, &destination, column) {
                let version = this.detail_label_arrival.as_ref().map_or(0, |(version, ..)| version + 1);
                this.detail_label_arrival = Some((version, drag.path.clone(), destination.clone(), column));
                this.settings.save();
                cx.notify();
            }
            cx.stop_propagation();
        }))
        .on_click(cx.listener(move |this: &mut XoolFiles, event: &ClickEvent, _, cx| {
            let point = event.position();
            this.close_context_menu(cx);
            this.columns_menu = None;
            this.sort_menu = false;
            this.view_menu = false;
            this.address_history_open = false;
            this.detail_label_menu = Some((path.clone(), column, f32::from(point.x), f32::from(point.y) + 8.));
            cx.notify();
            cx.stop_propagation();
        }))
        .into_any_element()
}

fn detail_label_popup(
    app: &XoolFiles, path: &std::path::Path, column: DetailColumn, position: (f32, f32),
    viewport: (f32, f32), p: &Colors, cx: &mut Context<XoolFiles>,
) -> AnyElement {
    let Some(choices) = DetailLabel::choices(column) else { return div().into_any_element(); };
    let selected = app.settings.file_labels.get(path).and_then(|labels| labels.get(column));
    let (x, width) = context_menu_axis(position.0, viewport.0, 172.);
    let (y, height) = context_menu_axis(position.1, viewport.1, 12. + 28. * (choices.len() + 1) as f32);
    let path = path.to_path_buf();
    div().id("detail-label-popup").absolute().left(px(x)).top(px(y)).w(px(width)).max_h(px(height))
        .overflow_y_scroll().occlude().p(px(5.)).rounded(px(8.))
        .border_1().border_color(p.border).bg(p.panel).shadow_lg()
        .on_mouse_down_out(cx.listener(|this: &mut XoolFiles, _: &MouseDownEvent, _, cx| {
            this.detail_label_menu = None;
            cx.notify();
        }))
        .children(std::iter::once(None).chain(choices.iter().copied().map(Some)).enumerate().map(|(index, value)| {
            let path = path.clone();
            div().id(("detail-label-choice", index)).h(px(28.)).px(px(6.)).gap(px(7.))
                .flex().items_center().justify_between().rounded(px(4.))
                .cursor_pointer().hover(|style| style.bg(p.card_hover))
                .child(value.map_or_else(|| div().text_size(px(11.)).text_color(p.muted)
                    .child(tr("Quitar etiqueta", "Clear label")), |label| detail_label_chip(label, p)))
                .when(value == selected, |row| row.child(glyph("\u{E73E}", 10., p.accent)))
                .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.settings.set_file_label(&path, column, value);
                    this.settings.save();
                    this.detail_label_menu = None;
                    cx.notify();
                    cx.stop_propagation();
                }))
        }))
        .with_animation("detail-label-popup-in", Animation::new(Duration::from_millis(155)).with_easing(ease_out_quint()),
            move |menu, progress| menu.top(px(y - (1. - progress) * 6.)).opacity(progress))
        .into_any_element()
}

fn visual(
    entry: &Entry,
    thumb: Option<std::sync::Arc<gpui::RenderImage>>,
    is_dir: bool,
    kind: FileKind,
    extension: &str,
    size: f32,
    thumb_width: f32,
) -> AnyElement {
    if let Some(thumb) = thumb {
        // El marco toma la proporción real de la imagen y aprovecha el ancho
        // disponible de la celda; así no hay franjas ni espacio sin usar.
        let pixels = thumb.size(0);
        let ratio = (pixels.width.0.max(1) as f32 / pixels.height.0.max(1) as f32).clamp(0.3, 4.0);
        let (mut box_w, mut box_h) = if ratio >= 1.0 {
            (thumb_width, thumb_width / ratio)
        } else {
            (thumb_width * ratio, thumb_width)
        };
        if box_h > size {
            let factor = size / box_h;
            box_w *= factor;
            box_h = size;
        }
        let radius = (box_w.min(box_h) * 0.14).max(4.);
        return div()
            .w(px(box_w))
            .h(px(box_h))
            .rounded(px(radius))
            .overflow_hidden()
            .child(img(thumb).w_full().h_full().object_fit(ObjectFit::Cover))
            .into_any_element();
    }

    if let Some(icon) = entry.icon.clone()
        .or_else(|| platform::cached_shell_icon(&entry.path, is_dir, 256))
        .or_else(|| (is_dir || entry.folder_shortcut).then(platform::default_folder_icon).flatten())
    {
        return div()
            .w(px(size))
            .h(px(size))
            .flex()
            .items_center()
            .justify_center()
            .child(
                img(icon)
                    .max_w_full()
                    .max_h_full()
                    .object_fit(ObjectFit::Contain),
            )
            .into_any_element();
    }

    if is_dir || entry.folder_shortcut {
        let body_w = size * 0.86;
        let left = (size - body_w) / 2.0;
        return div()
            .relative().w(px(size)).h(px(size))
            .child(div().absolute().left(px(left)).top(px(size * 0.15))
                .w(px(size * 0.37)).h(px(size * 0.27))
                .rounded_t(px(size * 0.06)).bg(rgb(0xffbc23)))
            .child(div().absolute().left(px(left)).top(px(size * 0.25))
                .w(px(body_w)).h(px(size * 0.58)).rounded(px(size * 0.055))
                .border_1().border_color(rgb(0xf0b42d)).bg(rgb(0xffd063))
                .child(div().absolute().left(px(size * 0.03)).right(px(size * 0.03))
                    .top(px(1.)).h(px(1.)).bg(rgb(0xffeab3))))
            .into_any_element();
    }

    let color = rgb(kind.color());
    let tint = rgba((kind.color() << 8) | 0x24);
    let icon = match kind {
        FileKind::Image => "\u{E91B}",
        FileKind::Video => "\u{E714}",
        FileKind::Audio => "\u{E8D6}",
        FileKind::Document => "\u{E8A5}",
        FileKind::Code => "\u{E943}",
        FileKind::Archive => "\u{E7B8}",
        FileKind::Executable => "\u{E7C3}",
        FileKind::Font => "\u{E8D2}",
        _ => "\u{E8A5}",
    };
    let badge = kind.badge(extension);
    let badge_size = (size * 0.22).clamp(7.5, 10.0);

    div()
        .relative()
        .w(px(size))
        .h(px(size))
        .rounded(px((size * 0.2).max(4.)))
        .bg(tint)
        .border_1()
        .border_color(rgba((kind.color() << 8) | 0x30))
        .flex()
        .items_center()
        .justify_center()
        .child(glyph(icon, size * 0.44, color))
        .child(
            div()
                .absolute()
                .right(px((size * 0.07).max(2.)))
                .bottom(px((size * 0.07).max(2.)))
                .px(px((size * 0.045).max(2.)))
                .rounded(px(3.))
                .bg(color)
                .text_color(rgb(0x10151c))
                .text_size(px(badge_size))
                .font_weight(FontWeight::BOLD)
                .child(SharedString::from(badge)),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Vista previa
// ---------------------------------------------------------------------------

fn preview_panel(app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    let tab = app.active_tab();
    let path = tab.primary_path();
    let entry = path.as_ref().and_then(|path| tab.entry_for(path)).cloned();

    let mut panel = div()
        .w(px(app.settings.preview_width.clamp(260.0, 720.0)))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .bg(p.panel)
        .when(app.settings.preview_left, |style| style.border_r_1())
        .when(!app.settings.preview_left, |style| style.border_l_1())
        .border_color(p.border_soft)
        .child(
            div()
                .h(px(38.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(7.))
                .px(px(13.))
                .border_b_1()
                .border_color(p.border_soft)
                .child(glyph("\u{E8A1}", 11., p.muted))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(8.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.dim)
                        .child(tr("VISTA PREVIA", "PREVIEW")),
                )
                .child(icon_button(
                    "preview-close",
                    "\u{E8BB}",
                    9.,
                    p,
                    false,
                    true,
                    cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                        this.toggle_preview(cx);
                    }),
                )),
        );

    let edit = app.text_edit.as_ref();
    let editing_selected = edit.is_some_and(|(path, _, _)|
        entry.as_ref().is_some_and(|entry| entry.path == *path));
    if let Some((path, original, input)) = edit && !editing_selected {
        return panel.child(preview_scroll()
            .child(div().text_color(p.muted).truncate()
                .child(crate::localized!(format, "Edición pendiente de {}", "Unsaved edits to {}", folder_name(path))))
            .child(preview_text_editor(input, original, app.settings.preview_width, p, cx)))
            .into_any_element();
    }
    let body = match (&entry, &app.preview) {
        (None, _) => div()
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .justify_center()
            .gap(px(9.))
            .px(px(24.))
            .child(
                div()
                    .w(px(58.))
                    .h(px(58.))
                    .rounded(px(17.))
                    .bg(p.card)
                    .border_1()
                    .border_color(p.border_soft)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(glyph("\u{E8A1}", 23., p.dim)),
            )
            .child(
                div()
                    .text_size(px(11.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.muted)
                    .child(tr("Nada seleccionado", "Nothing selected")),
            )
            .child(
                div()
                    .text_size(px(9.5))
                    .text_color(p.dim)
                    .text_center()
                    .child(tr("Selecciona un archivo o carpeta para ver sus detalles.", "Select a file or folder to see its details.")),
            )
            .into_any_element(),
        (Some(_), _) if app.preview_loading && !editing_selected => div()
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .justify_center()
            .gap(px(8.))
            .child(glyph("\u{E895}", 22., p.accent))
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(p.muted)
                    .child(tr("Preparando vista previa…", "Preparing preview…")),
            )
            .into_any_element(),
        (Some(entry), content) if editing_selected || content.is_some() => {
            let mut content_area = preview_scroll();
            if let Some((_, original, input)) = edit {
                content_area = content_area.child(preview_text_editor(input, original, app.settings.preview_width, p, cx));
            } else if let Some(content) = content {
                content_area = content_area.child(preview_content_view(app, entry, content, p, cx));
                if matches!(content, PreviewContent::Image(_))
                    && crate::image_edits::format_for(&entry.path).is_some()
                    && !app.in_recycle_bin()
                {
                    content_area = content_area.child(preview_image_actions(app, &entry.path, p, cx));
                }
                if app.settings.edit_text_preview && !entry.is_dir && XoolFiles::can_edit_preview_text(&entry.path) {
                    let path = entry.path.clone();
                    content_area = content_area.child(secondary_button("edit-preview-text", tr("Editar", "Edit"), p, cx.listener(
                        move |this: &mut XoolFiles, _: &ClickEvent, window, cx| this.begin_preview_text(path.clone(), window, cx),
                    )));
                }
            }

            let meta = div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .p(px(10.))
                .rounded(px(10.))
                .bg(p.card)
                .border_1()
                .border_color(p.border_soft)
                .child(meta_row(tr("Nombre", "Name"), &entry.name, p))
                .child(meta_row(tr("Tipo", "Type"), &entry.type_label(), p))
                .child(meta_row(tr("Tamaño", "Size"), &entry.size_label(), p))
                .child(meta_row(tr("Modificado", "Modified"), &entry.modified_label(), p))
                .child(meta_row(tr("Ruta", "Path"), &fs_model::normalize_path(&entry.path), p));

            content_area = content_area.child(meta);

            let open_path = entry.path.clone();
            let reveal_path = entry.path.clone();
            content_area = content_area.child(
                div()
                    .flex()
                    .gap(px(7.))
                    .child(
                        div()
                            .id("preview-open")
                            .flex_1()
                            .h(px(31.))
                            .rounded(px(8.))
                            .bg(p.accent)
                            .text_color(p.accent_fg)
                            .text_size(px(10.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|style| style.bg(p.accent_hover))
                            .on_click(cx.listener(
                                move |this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                                    this.open_path(open_path.clone(), window, cx);
                                },
                            ))
                            .child(tr("Abrir", "Open")),
                    )
                    .child(
                        div()
                            .id("preview-reveal")
                            .h(px(31.))
                            .px(px(10.))
                            .rounded(px(8.))
                            .bg(p.card)
                            .border_1()
                            .border_color(p.border)
                            .text_color(p.muted)
                            .text_size(px(10.5))
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .cursor_pointer()
                            .hover(|style| style.bg(p.card_hover).text_color(p.text))
                            .on_click(cx.listener(
                                move |_: &mut XoolFiles, _: &ClickEvent, window, _| {
                                    crate::platform::reveal(window, &reveal_path);
                                },
                            ))
                            .child(glyph("\u{E8DA}", 10., p.muted))
                            .child(tr("Carpeta", "Folder")),
                    ),
            );

            content_area.into_any_element()
        }
        (Some(_), _) => div()
            .flex()
            .flex_1()
            .items_center()
            .justify_center()
            .child(glyph("\u{E8A1}", 22., p.dim))
            .into_any_element(),
    };

    panel = panel.child(body);

    if let Some(entry) = entry {
        panel = panel.child(
            div()
                .flex_shrink_0()
                .px(px(13.))
                .py(px(8.))
                .border_t_1()
                .border_color(p.border_soft)
                .text_size(px(8.5))
                .text_color(p.dim)
                .text_ellipsis()
                .child(SharedString::from(fs_model::normalize_path(&entry.path))),
        );
    }

    panel.into_any_element()
}

fn preview_image_actions(app: &XoolFiles, path: &std::path::Path, p: &Colors, cx: &mut Context<XoolFiles>) -> Div {
    use crate::app::PreviewImageTool;
    use crate::image_edits::Edit;

    let source_format = crate::image_edits::format_for(path);
    let expanded = matches!(&app.preview_image_tool, Some(PreviewImageTool::Convert(current)) if current == path);
    let resize = match &app.preview_image_tool {
        Some(PreviewImageTool::Resize(current, controls)) if current == path => Some(controls.as_ref()),
        _ => None,
    };
    let actions = div().flex().flex_wrap().gap(px(5.)).children([
        ("preview-convert", tr("Convertir", "Convert"), "icons/image-convert.svg", None),
        ("preview-resize-image", tr("Redimensionar", "Resize"), "icons/image-resize.svg", None),
        ("preview-flip-h", tr("Espejo horizontal", "Mirror horizontally"), "icons/image-mirror-horizontal.svg", Some(Edit::FlipHorizontal)),
        ("preview-flip-v", tr("Espejo vertical", "Mirror vertically"), "icons/image-mirror-vertical.svg", Some(Edit::FlipVertical)),
    ].into_iter().map(|(id, label, symbol, edit)| {
        let path = path.to_path_buf();
        let open = if id == "preview-convert" { expanded } else { resize.is_some() };
        image_action_button(id, label, Some(symbol), p)
            .when(edit.is_none(), |button| {
                button.child(glyph_inherit(if open { "\u{E70E}" } else { "\u{E70D}" }, 8.))
                    .when(open, |button| button.bg(p.accent_faint).border_color(p.accent_soft))
            })
            .cursor_pointer().hover(|button| button.bg(p.card_hover).text_color(p.text))
            .on_click(cx.listener(move |this: &mut XoolFiles, _, _, cx| {
                if let Some(edit) = edit {
                    this.save_image_copy(path.clone(), edit, cx);
                } else if id == "preview-convert" {
                    this.toggle_image_conversion(path.clone(), cx);
                } else {
                    this.toggle_image_resize(path.clone(), cx);
                }
                cx.stop_propagation();
            }))
    }));
    let formats = || div().id("preview-image-formats").flex().flex_wrap().gap(px(5.))
        .children([
            ("preview-convert-png", "PNG", image::ImageFormat::Png),
            ("preview-convert-webp", "WebP", image::ImageFormat::WebP),
            ("preview-convert-jpg", "JPG", image::ImageFormat::Jpeg),
        ].into_iter().map(|(id, label, format)| {
            let path = path.to_path_buf();
            let enabled = Some(format) != source_format;
            image_action_button(id, label, None, p).h(px(26.))
                .when(!enabled, |button| button.opacity(0.45))
                .when(enabled, |button| button.cursor_pointer()
                    .hover(|button| button.bg(p.card_hover).text_color(p.text))
                    .on_click(cx.listener(move |this: &mut XoolFiles, _, _, cx| {
                        this.save_image_copy(path.clone(), Edit::Convert(format), cx);
                        cx.stop_propagation();
                    })))
        }));
    div().flex().flex_col().gap(px(6.))
        .child(div().text_size(px(10.5)).font_weight(FontWeight::SEMIBOLD)
            .child(tr("Acciones de imagen", "Image actions")))
        .child(if app.image_editing {
            div().text_size(px(10.5)).text_color(p.muted).child(tr("Guardando copia…", "Saving copy…"))
                .into_any_element()
        } else { actions.into_any_element() })
        .when(expanded && !app.image_editing, |area| area.child(
            formats().with_animation("preview-convert-in", Animation::new(Duration::from_millis(145)).with_easing(ease_out_quint()),
                |row, progress| row.relative().top(px(-5. * (1. - progress))).opacity(progress))
        ))
        .when_some(resize, |area, controls| area.child(image_resize_controls(controls, p, cx)))
}

fn image_action_button(id: &'static str, label: &'static str, icon: Option<&'static str>, p: &Colors) -> gpui::Stateful<Div> {
    div().id(id).h(px(30.)).px(px(9.)).flex().items_center().gap(px(5.))
        .rounded(px(7.)).bg(p.card).border_1().border_color(p.border)
        .text_size(px(10.5)).text_color(p.muted)
        .when_some(icon, |button, icon| button.child(glyph_inherit(icon, 13.)))
        .child(label)
}

fn preview_scroll() -> gpui::Stateful<Div> {
    div().id("preview-scroll").flex_1().min_h(px(0.)).overflow_y_scroll()
        .px(px(13.)).py(px(12.)).flex().flex_col().gap(px(10.))
}

fn preview_text_editor(
    input: &Entity<InputState>, original: &str, width: f32, p: &Colors, cx: &mut Context<XoolFiles>,
) -> Div {
    let columns = ((width.clamp(260., 720.) - 48.) / 6.).floor() as usize;
    let lines: usize = original.lines().take(32)
        .map(|line| line.len().div_ceil(columns).max(1)).sum();
    let height = (lines as f32 * 15. + 24.).clamp(110., 420.);
    div().flex().flex_col().flex_shrink_0().gap(px(10.))
        .child(div().h(px(height)).flex_shrink_0().rounded(px(10.)).border_1()
            .border_color(p.accent).bg(p.input).overflow_hidden()
            .child(Input::new(input).appearance(false).h_full().font_family(MONO_FONT)
                .text_size(px(10.)).px(px(10.)).py(px(8.))))
        .child(div().flex().gap(px(7.))
            .child(primary_button("save-preview-text", tr("Guardar", "Save"), p, cx.listener(
                |this: &mut XoolFiles, _: &ClickEvent, window, cx| this.save_preview_text(window, cx),
            )))
            .child(secondary_button("cancel-preview-text", tr("Cancelar", "Cancel"), p, cx.listener(
                |this: &mut XoolFiles, _: &ClickEvent, window, cx| this.cancel_preview_text(window, cx),
            ))))
}

fn preview_content_view(
    app: &XoolFiles,
    entry: &Entry,
    content: &PreviewContent,
    p: &Colors,
    cx: &mut Context<XoolFiles>,
) -> AnyElement {
    let _ = entry;
    match content {
        PreviewContent::Image(image) => {
            // La imagen ocupa el ancho del inspector y crece con él, igual
            // que el vídeo. La altura se deduce de la proporción real y se
            // limita para dejar sitio a los datos del archivo.
            let available = (app.settings.preview_width.clamp(260.0, 720.0) - 40.0).max(120.0);
            let pixels = image.size(0);
            let ratio = (pixels.width.0.max(1) as f32 / pixels.height.0.max(1) as f32)
                .clamp(0.2, 5.0);
            let height = (available / ratio).clamp(140.0, 420.0);
            div()
                .w_full()
                .rounded(px(10.))
                .overflow_hidden()
                .bg(p.input)
                .border_1()
                .border_color(p.border_soft)
                .flex()
                .items_center()
                .justify_center()
                .child(
                    img(image.clone())
                        .w_full()
                        .h(px(height))
                        .object_fit(ObjectFit::Contain),
                )
                .into_any_element()
        }
        PreviewContent::Text(text) => {
            let shown: String = text.chars().take(5000).collect();
            div()
                .w_full()
                .p(px(10.))
                .rounded(px(10.))
                .bg(p.input)
                .border_1()
                .border_color(p.border_soft)
                .font_family(MONO_FONT)
                .text_size(px(9.5))
                .text_color(p.muted)
                .child(SharedString::from(shown))
                .into_any_element()
        }
        PreviewContent::Folder { count, size, names } => {
            let mut card = div()
                .w_full()
                .p(px(11.))
                .rounded(px(10.))
                .bg(p.card)
                .border_1()
                .border_color(p.border_soft)
                .flex()
                .flex_col()
                .items_center()
                .gap(px(7.))
                .child(
                    div()
                        .w(px(52.))
                        .h(px(52.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(folder_art(52., p)),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.text)
                        .child(crate::localized!(format, "{count} elementos", "{count} items")),
                )
                .child(
                    div()
                        .text_size(px(9.5))
                        .text_color(p.dim)
                        .child(fs_model::format_size(*size)),
                );

            for name in names {
                card = card.child(
                    div()
                        .w_full()
                        .text_size(px(9.))
                        .text_color(p.muted)
                        .text_ellipsis()
                        .child(SharedString::from(name.clone())),
                );
            }
            card.into_any_element()
        }
        PreviewContent::Media { thumb, kind } => {
            let preview_width = app.settings.preview_width.clamp(260.0, 720.0);
            if *kind == FileKind::Audio
                && let Some(audio) = &app.audio
            {
                return audio_card(audio, app.settings.audio_visualization, app.audio_seek_ratio(), p, cx);
            }
            if *kind == FileKind::Video
                && let Some(video) = &app.video
            {
                return video_card(video, p, preview_width, cx);
            }

            let mut card = div()
                .w_full()
                .rounded(px(10.))
                .overflow_hidden()
                .bg(p.input)
                .border_1()
                .border_color(p.border_soft)
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(7.))
                .py(px(16.));

            if let Some(thumb) = thumb {
                card = card.child(
                    img(thumb.clone())
                        .w_full()
                        .max_h(px(220.))
                        .object_fit(ObjectFit::Contain),
                );
            } else {
                card = card.child(glyph(kind_glyph(*kind), 40., rgb(kind.color())));
            }

            card.child(
                div()
                    .text_size(px(9.5))
                    .text_color(p.muted)
                    .child(kind.label()),
            )
            .into_any_element()
        }
        PreviewContent::Message(text) => div()
            .w_full()
            .p(px(11.))
            .rounded(px(10.))
            .bg(p.card)
            .border_1()
            .border_color(p.border_soft)
            .text_size(px(9.5))
            .text_color(p.muted)
            .child(SharedString::from(text.clone()))
            .into_any_element(),
    }
}

fn media_time(seconds: f32) -> String {
    let seconds = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn audio_waveform(samples: Vec<f32>, style: AudioVisualization, progress: f32, p: &Colors) -> impl IntoElement {
    let played = p.accent;
    let pending = p.border;
    canvas(|_, _, _| (), move |bounds, (), window, _| {
        if samples.is_empty() || bounds.size.width <= px(0.) {
            return;
        }
        paint_audio_waveform(&samples, style, bounds, pending, window);
        if progress > 0. {
            let mut played_bounds = bounds;
            played_bounds.size.width *= progress;
            window.with_content_mask(Some(gpui::ContentMask { bounds: played_bounds }), |window| {
                paint_audio_waveform(&samples, style, bounds, played, window);
            });
        }
    }).w_full().h_full()
}

fn paint_audio_waveform(
    samples: &[f32], style: AudioVisualization, bounds: Bounds<Pixels>, color: Rgba, window: &mut Window,
) {
    let height = bounds.size.height;
    let baseline = bounds.origin.y + height * 0.5;
    if style == AudioVisualization::Wave {
        let step = bounds.size.width / samples.len().saturating_sub(1).max(1) as f32;
        let mut shape = gpui::PathBuilder::fill();
        let mut previous = gpui::point(bounds.origin.x, baseline);
        shape.move_to(previous);
        for (index, amplitude) in samples.iter().enumerate() {
            let next = gpui::point(bounds.origin.x + step * index as f32, baseline - height * (*amplitude * 0.45));
            let mid = (previous.x + next.x) * 0.5;
            shape.cubic_bezier_to(next, gpui::point(mid, previous.y), gpui::point(mid, next.y));
            previous = next;
        }
        let right = bounds.origin.x + bounds.size.width;
        shape.line_to(gpui::point(right, baseline));
        previous = gpui::point(right, baseline);
        for (index, amplitude) in samples.iter().enumerate().rev() {
            let next = gpui::point(bounds.origin.x + step * index as f32, baseline + height * (*amplitude * 0.45));
            let mid = (previous.x + next.x) * 0.5;
            shape.cubic_bezier_to(next, gpui::point(mid, previous.y), gpui::point(mid, next.y));
            previous = next;
        }
        shape.close();
        if let Ok(shape) = shape.build() {
            window.paint_path(shape, color);
        }
        return;
    }

    let step = bounds.size.width / samples.len() as f32;
    let centered = style == AudioVisualization::Centered;
    let width = step * if centered { 0.49 } else { 0.76 };
    for (index, amplitude) in samples.iter().enumerate() {
        let bar_height = (height * (*amplitude * 0.94)).max(px(2.));
        let x = bounds.origin.x + step * index as f32 + (step - width) * 0.5;
        let y = if centered { baseline - bar_height * 0.5 } else { bounds.origin.y + height - bar_height };
        let bar = Bounds { origin: gpui::point(x, y), size: gpui::size(width, bar_height) };
        window.paint_quad(gpui::fill(bar, color));
    }
}

fn audio_card(
    audio: &AudioPlayback, style: AudioVisualization, seek_ratio: Option<f32>,
    p: &Colors, cx: &mut Context<XoolFiles>,
) -> AnyElement {
    let mut card = div()
        .w_full()
        .p(px(11.))
        .rounded(px(12.))
        .bg(p.input)
        .border_1()
        .border_color(p.border_soft)
        .flex()
        .flex_col()
        .gap(px(10.));

    let extension = audio
        .path
        .extension()
        .map(|extension| extension.to_string_lossy().to_uppercase())
        .unwrap_or_else(|| "AUDIO".to_string());

    card = card.child(
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(glyph("\u{E8D6}", 11., p.accent))
            .child(
                div()
                    .text_size(px(8.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.dim)
                    .child("AUDIO"),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_size(px(8.5))
                    .text_color(p.dim)
                    .child(SharedString::from(if audio.loading {
                        tr("Analizando…", "Analyzing…").to_string()
                    } else {
                        format!("{} · {}", extension, media_time(audio.duration))
                    })),
            ),
    );

    if audio.loading {
        card = card.child(
            div()
                .h(px(86.))
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .gap(px(7.))
                .child(glyph("\u{E895}", 15., p.accent))
                .child(
                    div()
                        .text_size(px(9.5))
                        .text_color(p.muted)
                        .child(tr("Generando forma de onda…", "Generating waveform…")),
                ),
        );
        return card.into_any_element();
    }

    if let Some(error) = &audio.error {
        card = card.child(
            div()
                .h(px(86.))
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .px(px(10.))
                .text_size(px(9.5))
                .text_color(p.danger)
                .text_center()
                .child(SharedString::from(error.clone())),
        );
        return card.into_any_element();
    }

    let position = seek_ratio.map_or(audio.position, |ratio| audio.duration * ratio);
    let ratio = if audio.duration > 0.0 {
        (position / audio.duration).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let bounds: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::new(Cell::new(None));
    let waveform_bounds = bounds.clone();
    card = card.child(
        div().id("audio-waveform").relative().h(px(86.)).w_full().cursor_pointer()
            .on_mouse_down(MouseButton::Left, cx.listener(
                move |this: &mut XoolFiles, event: &MouseDownEvent, _, cx| {
                    if let Some(bounds) = bounds.get() {
                        this.begin_audio_seek_drag(bounds, event.position.x, cx);
                        cx.stop_propagation();
                    }
                },
            ))
            .child(audio_waveform(audio.waveform.clone(), style, ratio, p))
            .child(canvas(move |bounds, _, _| waveform_bounds.set(Some(bounds)), |_, _, _, _| {})
                .absolute().top_0().right_0().bottom_0().left_0()),
    );

    let playing = audio.playing;
    card = card.child(
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .id("audio-play")
                    .w(px(31.))
                    .h(px(31.))
                    .rounded(px(16.))
                    .bg(p.accent)
                    .text_color(p.accent_fg)
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|style| style.bg(p.accent_hover))
                    .on_click(cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                        this.toggle_audio(cx);
                    }))
                    .child(glyph(
                        if playing { "\u{E769}" } else { "\u{E768}" },
                        11.,
                        p.accent_fg,
                    )),
            )
            .child(
                div()
                    .font_family(MONO_FONT)
                    .text_size(px(9.5))
                    .text_color(p.text)
                    .child(media_time(position)),
            )
            .child(div().flex_1())
            .child(
                div()
                    .font_family(MONO_FONT)
                    .text_size(px(9.5))
                    .text_color(p.dim)
                    .child(media_time(audio.duration)),
            ),
    );

    card = card.child(volume_control(audio.volume, p, cx));
    card.into_any_element()
}

fn volume_control(volume: f32, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    let muted = volume <= 0.01;
    let row = div().flex().items_center().gap(px(8.)).child(
        div()
            .id("audio-mute")
            .w(px(20.))
            .h(px(20.))
            .rounded(px(5.))
            .flex()
            .items_center()
            .justify_center()
            .hover(|style| style.bg(p.card_hover))
            .cursor_pointer()
            .on_click(
                cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.set_media_volume(if muted { 0.8 } else { 0.0 }, cx);
                }),
            )
            .child(glyph(
                if muted { "\u{E74F}" } else { "\u{E767}" },
                10.,
                p.dim,
            )),
    );

    row.child(media_volume_slider("audio-volume", 74., volume, p, cx))
        .child(
            div()
                .font_family(MONO_FONT)
                .text_size(px(8.5))
                .text_color(p.dim)
                .child(format!("{}%", (volume * 100.0).round() as i32)),
        )
        .into_any_element()
}

fn media_volume_slider(
    id: &'static str, width: f32, volume: f32, p: &Colors, cx: &mut Context<XoolFiles>,
) -> AnyElement {
    let bounds = Rc::new(Cell::new(None));
    let slider_bounds = bounds.clone();
    div().id(id).relative().w(px(width)).h(px(18.)).flex().items_center().cursor_pointer()
        .on_mouse_down(MouseButton::Left, cx.listener(
            move |this: &mut XoolFiles, event: &MouseDownEvent, _, cx| {
                if let Some(bounds) = bounds.get() {
                    this.begin_media_volume_drag(bounds, event.position.x, cx);
                    cx.stop_propagation();
                }
            },
        ))
        .child(div().absolute().left_0().right_0().h(px(3.)).rounded(px(1.5)).bg(p.border))
        .child(div().absolute().left_0().w(gpui::relative(volume)).h(px(3.)).rounded(px(1.5)).bg(p.accent))
        .child(div().absolute().left(gpui::relative(volume)).ml(px(-4.)).size(px(8.))
            .rounded_full().bg(p.text))
        .child(canvas(move |bounds, _, _| slider_bounds.set(Some(bounds)), |_, _, _, _| {})
            .absolute().top_0().right_0().bottom_0().left_0())
        .into_any_element()
}

fn video_card(
    video: &VideoPlayer,
    p: &Colors,
    width: f32,
    cx: &mut Context<XoolFiles>,
) -> AnyElement {
    let snapshot = video.snapshot();
    let volume = video.volume();
    let muted = volume <= 0.01;
    let height = (width * 0.56).clamp(148.0, 300.0);

    let mut card = div()
        .w_full()
        .rounded(px(12.))
        .overflow_hidden()
        .bg(rgb(0x0a0a0c))
        .border_1()
        .border_color(p.border_soft)
        .flex()
        .flex_col();

    let mut area = div()
        .id("video-frame")
        .relative()
        .w_full()
        .h(px(height))
        .bg(rgb(0x060608))
        .flex()
        .items_center()
        .justify_center();

    if let Some(frame) = &video.frame {
        area = area.child(
            img(frame.clone())
                .max_w_full()
                .max_h_full()
                .object_fit(ObjectFit::Contain),
        );
        if !snapshot.playing {
            area = area.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .w(px(42.))
                            .h(px(42.))
                            .rounded(px(21.))
                            .bg(rgba(0x000000aa))
                            .border_1()
                            .border_color(rgba(0xffffff2a))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(glyph("\u{E768}", 14., rgb(0xffffff))),
                    ),
            );
        }
    } else if let Some(error) = &snapshot.error {
        area = area.child(
            div()
                .px(px(16.))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(6.))
                .child(glyph("\u{E7BA}", 22., p.danger))
                .child(
                    div()
                        .text_size(px(9.5))
                        .text_color(p.muted)
                        .text_center()
                        .child(SharedString::from(error.clone())),
                ),
        );
    } else {
        area = area.child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(7.))
                .child(glyph("\u{E895}", 18., p.accent))
                .child(
                    div()
                        .text_size(px(9.5))
                        .text_color(p.muted)
                        .child(tr("Cargando vídeo…", "Loading video…")),
                ),
        );
    }

    area = area.cursor_pointer().on_click(cx.listener(
        |this: &mut XoolFiles, _: &ClickEvent, _, cx| {
            this.toggle_video(cx);
        },
    ));
    card = card.child(area);

    let ratio = if snapshot.duration > 0.0 {
        (snapshot.position / snapshot.duration).clamp(0.0, 1.0)
    } else {
        0.0
    };

    // Barra de progreso continua, con carril, relleno y tirador. Se puede
    // arrastrar para fijar cualquier segundo.
    let seek_bounds: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::new(Cell::new(None));
    let seek_prepaint = seek_bounds.clone();
    let seek_down = seek_bounds.clone();

    let mut seek = div()
        .id("video-seek")
        .relative()
        .w_full()
        .h(px(16.))
        .flex()
        .items_center()
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this: &mut XoolFiles, event: &MouseDownEvent, _, cx| {
                this.begin_video_seek_drag(seek_down.get(), event, cx);
                cx.stop_propagation();
            }),
        )
        .child(
            div()
                .absolute()
                .left(px(0.))
                .right(px(0.))
                .h(px(3.))
                .rounded(px(1.5))
                .bg(rgba(0xffffff22)),
        );
    if ratio > 0.0 {
        seek = seek.child(
            div()
                .absolute()
                .left(px(0.))
                .w(gpui::relative(ratio))
                .h(px(3.))
                .rounded(px(1.5))
                .bg(p.accent),
        );
    }
    seek = seek.child(
        div()
            .absolute()
            .left(gpui::relative(ratio))
            .ml(px(-4.))
            .w(px(8.))
            .h(px(8.))
            .rounded(px(4.))
            .bg(rgb(0xffffff)),
    );
    seek = seek.child(
        canvas(
            move |bounds, _, _| seek_prepaint.set(Some(bounds)),
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0(),
    );

    let play_button = div()
        .id("video-play")
        .w(px(24.))
        .h(px(24.))
        .rounded(px(6.))
        .flex()
        .items_center()
        .justify_center()
        .text_color(rgb(0xf4f4f5))
        .hover(|style| style.bg(rgba(0xffffff14)))
        .cursor_pointer()
        .on_click(cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
            this.toggle_video(cx);
        }))
        .child(glyph(
            if snapshot.playing {
                "\u{E769}"
            } else {
                "\u{E768}"
            },
            12.,
            rgb(0xf4f4f5),
        ));

    let time = div()
        .font_family(MONO_FONT)
        .text_size(px(9.5))
        .text_color(p.text)
        .child(format!(
            "{} / {}",
            media_time(snapshot.position),
            media_time(snapshot.duration)
        ));

    let speaker = div()
        .id("video-mute")
        .w(px(22.))
        .h(px(22.))
        .rounded(px(5.))
        .flex()
        .items_center()
        .justify_center()
        .text_color(p.muted)
        .hover(|style| style.bg(rgba(0xffffff14)).text_color(p.text))
        .cursor_pointer()
        .on_click(
            cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                this.set_media_volume(if muted { 0.8 } else { 0.0 }, cx);
            }),
        )
        .child(glyph(
            if muted { "\u{E74F}" } else { "\u{E767}" },
            11.,
            if muted { p.muted } else { p.text },
        ));

    let mut right = div().flex().items_center().gap(px(6.)).child(speaker);
    if width >= 320.0 {
        right = right.child(media_volume_slider("video-volume", 88., volume, p, cx));
    }

    let controls = div()
        .px(px(10.))
        .pt(px(6.))
        .pb(px(8.))
        .flex()
        .flex_col()
        .gap(px(1.))
        .bg(rgb(0x101013))
        .border_t_1()
        .border_color(rgba(0xffffff0f))
        .child(seek)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(play_button)
                .child(time)
                .child(div().flex_1())
                .child(right),
        );

    card.child(controls).into_any_element()
}

fn meta_row(label: &'static str, value: &str, p: &Colors) -> AnyElement {
    div()
        .flex()
        .items_start()
        .gap(px(8.))
        .child(
            div()
                .w(px(64.))
                .flex_shrink_0()
                .text_size(px(8.5))
                .text_color(p.dim)
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_size(px(9.5))
                .text_color(p.muted)
                .text_ellipsis()
                .child(SharedString::from(value.to_string())),
        )
        .into_any_element()
}

fn folder_art(size: f32, p: &Colors) -> AnyElement {
    let body_w = size * 0.82;
    let body_h = size * 0.56;
    let tab_w = size * 0.36;
    let tab_h = size * 0.14;
    div()
        .relative()
        .w(px(size))
        .h(px(size))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .absolute()
                .left(px((size - body_w) / 2.0))
                .top(px((size - body_h) / 2.0 + size * 0.06))
                .w(px(body_w))
                .h(px(body_h))
                .rounded(px((size * 0.11).max(4.)))
                .bg(p.accent),
        )
        .child(
            div()
                .absolute()
                .left(px((size - body_w) / 2.0))
                .top(px((size - body_h) / 2.0 - size * 0.08))
                .w(px(tab_w))
                .h(px(tab_h + size * 0.1))
                .rounded_t(px((size * 0.09).max(3.)))
                .bg(p.accent_hover),
        )
        .child(
            div()
                .absolute()
                .left(px((size - body_w) / 2.0 + size * 0.08))
                .top(px((size - body_h) / 2.0 + size * 0.14))
                .w(px(body_w * 0.6))
                .h(px((size * 0.035).max(1.5)))
                .rounded(px(2.))
                .bg(rgba(0xffffff44)),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Barra de estado
// ---------------------------------------------------------------------------

fn status_bar(app: &XoolFiles, p: &Colors, width: f32, cx: &mut Context<XoolFiles>) -> AnyElement {
    let tab = app.active_tab();
    let selected = tab.selected_count();
    let selected_size = tab.selected_size();
    let total = tab.root_entries().len();
    let visible = if total == tab.entries.len() {
        tab.visible.len()
    } else {
        tab.visible.iter().filter(|&&index| index < total).count()
    };
    let query_active = !app.active_tab().search_query.trim().is_empty();

    let medium = width >= 700.0;
    let tiny = width < 520.0;

    let selection_text = if tiny {
        format!("{visible}/{total}")
    } else if query_active || !app.active_tab().show_folders || !app.active_tab().show_files {
        crate::localized!(format, "{visible} de {total} elementos", "{visible} of {total} items")
    } else {
        crate::localized!(format, "{total} elementos", "{total} items")
    };
    let selection_active = selected > 1;

    let recursive = app.active_tab().recursive_search;
    let search_w = if width < 380.0 {
        92.0
    } else if width < 520.0 {
        120.0
    } else if width < 760.0 {
        190.0
    } else {
        236.0
    };
    let mut search = div()
        .w(px(search_w))
        .h(px(26.))
        .flex_shrink()
        .flex()
        .items_center()
        .gap(px(5.))
        .px(px(8.))
        .rounded(px(7.))
        .bg(p.input)
        .border_1()
        .border_color(if recursive { p.accent_soft } else { p.border })
        .child(glyph("\u{E721}", 10., p.dim))
        .child(
            div().flex_1().min_w(px(0.)).child(
                Input::new(&app.search)
                    .appearance(false)
                    .bordered(false)
                    .cleanable(true)
                    .h(px(22.)),
            ),
        );
    if !tiny {
        search = search.child(
            div()
                .id("search-recursive")
                .h(px(19.))
                .px(px(4.))
                .rounded(px(5.))
                .flex()
                .items_center()
                .gap(px(2.))
                .bg(if recursive {
                    p.accent_soft
                } else {
                    p.panel_soft
                })
                .text_color(if recursive { p.accent } else { p.dim })
                .text_size(px(7.5))
                .font_weight(FontWeight::SEMIBOLD)
                .hover(|style| style.bg(p.card_hover).text_color(p.text))
                .on_click(cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                    this.toggle_recursive_search(window, cx);
                }))
                .child(glyph(
                    "\u{E838}",
                    10.,
                    if recursive { p.accent } else { p.dim },
                ))
                .child("SUB"),
        );
    }

    let mut elements = div()
        .h(px(26.))
        .px(px(9.))
        .rounded(px(7.))
        .bg(if selection_active {
            p.accent_soft
        } else {
            p.card
        })
        .border_1()
        .border_color(if selection_active {
            p.accent_soft
        } else {
            p.border_soft
        })
        .flex()
        .items_center()
        .gap(px(6.))
        .text_size(px(10.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(if selection_active { p.accent } else { p.text });
    if selection_active {
        elements = elements
            .child(glyph("\u{E81E}", 11., p.accent))
            .child(SharedString::from(selected.to_string()));
        if selected_size > 0 {
            elements = elements.child(
                div()
                    .font_family(MONO_FONT)
                    .text_size(px(8.5))
                    .text_color(p.accent)
                    .child(fs_model::format_size(selected_size)),
            );
        }
    } else {
        elements = elements.child(selection_text);
    }

    let left = div()
        .flex()
        .items_center()
        .gap(px(8.))
        .min_w(px(0.))
        .flex_shrink()
        .child(search)
        .child(elements);

    let zoom = app.settings.view.zoom_percent(app.settings.icon_scale);
    let zoom_fill = app.settings.view.zoom_percent(app.icon_scale_display) as f32 / 100.;
    let folder_count = tab.root_entries().iter().filter(|entry| entry.is_dir).count();
    let file_count = total.saturating_sub(folder_count);

    let mut right = div().flex().items_center().gap(px(4.));

    if !tiny {
        let folder_toggle = div()
            .id("status-folders")
            .h(px(26.))
            .px(px(7.))
            .rounded(px(6.))
            .flex()
            .items_center()
            .gap(px(4.))
            .font_family(MONO_FONT)
            .text_size(px(9.))
            .text_color(if app.active_tab().show_folders { p.muted } else { p.dim })
            .when(!app.active_tab().show_folders, |style| style.opacity(0.4))
            .hover(|style| style.bg(p.card_hover).opacity(1.0))
            .cursor_pointer()
            .on_click(cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                this.toggle_folders(cx);
            }))
            .child(glyph(
                "\u{E8B7}",
                12.,
                if app.active_tab().show_folders { p.accent } else { p.dim },
            ))
            .child(SharedString::from(folder_count.to_string()));

        let file_toggle = div()
            .id("status-files")
            .h(px(26.))
            .px(px(7.))
            .rounded(px(6.))
            .flex()
            .items_center()
            .gap(px(4.))
            .font_family(MONO_FONT)
            .text_size(px(9.))
            .text_color(if app.active_tab().show_files { p.muted } else { p.dim })
            .when(!app.active_tab().show_files, |style| style.opacity(0.4))
            .hover(|style| style.bg(p.card_hover).opacity(1.0))
            .cursor_pointer()
            .on_click(cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                this.toggle_files(cx);
            }))
            .child(glyph(
                "\u{E8A5}",
                12.,
                if app.active_tab().show_files { p.accent } else { p.dim },
            ))
            .child(SharedString::from(file_count.to_string()));

        right = right.child(folder_toggle).child(file_toggle);
    }

    right = right.child(
        div()
            .w(px(1.))
            .h(px(18.))
            .mx(px(5.))
            .flex_shrink_0()
            .bg(p.border),
    );

    let (view_icon, view_label) = match app.settings.view {
        ViewMode::Grid => ("\u{E8FD}", tr("Iconos", "Icons")),
        ViewMode::List => ("\u{E80A}", tr("Lista", "List")),
        ViewMode::Tree => ("\u{E8FD}", tr("Árbol", "Tree")),
        ViewMode::Details => ("\u{E80A}", tr("Detalles", "Details")),
    };
    right = right.child(
        div()
            .id("status-view")
            .h(px(26.))
            .pl(px(5.)).pr(px(2.))
            .rounded(px(5.))
            .flex().flex_shrink_0().items_center().gap(px(7.))
            .when(app.view_menu, |style| style.bg(p.card_hover))
            .hover(|style| style.bg(p.card_hover))
            .cursor_pointer()
            .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new(tr("Cambiar vista y tamaño", "Change view and size")).build(window, cx))
            .on_click(cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                this.toggle_view_menu(cx);
            }))
            .child(glyph(view_icon, 12., p.muted))
            .when(medium, |style| style.child(
                div().text_size(px(10.5)).text_color(p.muted).whitespace_nowrap().child(view_label)
            ))
            .child(
                div().relative().h(px(22.)).min_w(px(34.)).px(px(7.))
                    .rounded(px(3.)).border_1().border_color(p.accent_soft).bg(p.input).overflow_hidden()
                    .flex().items_center().justify_center()
                    .text_size(px(10.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.text)
                    .when(zoom_fill > 0., |badge| badge.child(div().absolute().left(px(0.)).right(px(0.)).bottom(px(0.))
                        .h(gpui::relative(zoom_fill)).bg(Rgba { a: 0.24, ..p.accent })
                        .child(div().absolute().left(px(0.)).right(px(0.)).top(px(0.)).h(px(1.)).bg(p.accent))))
                    .child(format!("{zoom}%")),
            ),
    );

    div()
        .h(px(STATUSBAR_HEIGHT))
        .w_full()
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .pl(px(4.))
        .pr(px(12.))
        .bg(p.titlebar)
        .when(app.settings.status_bar_above, |bar| bar.border_b_1())
        .when(!app.settings.status_bar_above, |bar| bar.border_t_1())
        .border_color(p.border_soft)
        .child(left)
        .child(right)
        .into_any_element()
}

fn view_menu_popup(
    app: &XoolFiles,
    p: &Colors,
    viewport_w: f32,
    viewport_h: f32,
    title_h: f32,
    cx: &mut Context<XoolFiles>,
) -> AnyElement {
    let below_bar = app.settings.status_bar_above;
    let anchor = STATUSBAR_HEIGHT + 8. + if below_bar { title_h } else { 0. };
    let zoom = app.settings.view.zoom_percent(app.settings.icon_scale);
    let width = 238.0;
    let left = (viewport_w - width - 14.0).max(14.0);

    let mut menu = div()
        .id("view-menu-popup")
        .absolute()
        .left(px(left))
        .w(px(width))
        .max_h(px((viewport_h - anchor - 8.).max(0.)))
        .overflow_y_scroll()
        .p(px(6.))
        .rounded(px(12.))
        .bg(p.panel)
        .border_1()
        .border_color(p.border)
        .flex()
        .flex_col()
        .gap(px(1.))
        .occlude();

    menu = menu.child(
        div()
            .px(px(8.))
            .pt(px(4.))
            .pb(px(5.))
            .flex_shrink_0()
            .text_size(px(8.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(p.dim)
            .child(tr("VISTA", "VIEW")),
    );

    let presets = [
        (tr("Iconos muy grandes", "Extra large icons"), 100),
        (tr("Iconos grandes", "Large icons"), 75),
        (tr("Iconos medianos", "Medium icons"), 50),
        (tr("Iconos pequeños", "Small icons"), 25),
        (tr("Lista", "List"), 15),
        (tr("Árbol", "Tree"), 10),
        (tr("Detalles", "Details"), 0),
    ];
    for (label, preset) in presets {
        if preset == 15 {
            menu = menu.child(div().h(px(1.)).flex_shrink_0().mx(px(6.)).my(px(5.)).bg(p.border_soft));
        }
        menu = menu.child(view_menu_item(
            ("view-preset", preset as usize),
            view_preset_icon(preset, if zoom == preset { p.accent } else { p.muted }),
            label,
            zoom == preset,
            p,
            cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                this.set_view_preset(preset, cx);
            }),
        ));
    }

    popup_in(menu, "view-menu-enter", move |panel, slide| {
        if below_bar { panel.top(px(anchor - slide)) } else { panel.bottom(px(anchor - slide)) }
    })
}

fn view_preset_icon(preset: u32, color: Rgba) -> Div {
    let cells = match preset {
        100 => 1,
        75 => 2,
        50 => 3,
        25 => 4,
        15 => return glyph("\u{E80A}", 12., color),
        10 => return glyph("\u{E8FD}", 12., color),
        _ => return glyph("\u{E8A5}", 12., color),
    };
    let size = 12. / cells as f32 - 1.;
    div().size(px(12.)).flex_shrink_0().flex().flex_col().items_center().justify_center().gap(px(1.))
        .children((0..cells).map(|_| div().flex().gap(px(1.))
            .children((0..cells).map(|_| div().size(px(size)).rounded(px(size / 5.)).bg(color)))))
}

fn view_menu_item(
    id: impl Into<ElementId>,
    icon: Div,
    label: &'static str,
    active: bool,
    p: &Colors,
    handler: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .h(px(30.))
        .flex_shrink_0()
        .px(px(8.))
        .rounded(px(7.))
        .flex()
        .items_center()
        .gap(px(8.))
        .text_size(px(10.5))
        .text_color(if active { p.accent } else { p.muted })
        .when(active, |style| style.bg(p.accent_soft))
        .hover(|style| style.bg(p.card_hover).text_color(p.text))
        .cursor_pointer()
        .on_click(handler)
        .child(icon)
        .child(div().flex_1().child(label))
        .when(active, |style| {
            style.child(glyph("\u{E73E}", 10., p.accent))
        })
        .into_any_element()
}

fn address_history_backdrop(top: f32, cx: &mut Context<XoolFiles>) -> AnyElement {
    div()
        .id("address-history-backdrop")
        .absolute()
        .top(px(top))
        .left(px(0.))
        .right(px(0.))
        .bottom(px(0.))
        .occlude()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this: &mut XoolFiles, _: &MouseDownEvent, window, cx| {
                this.close_address_history(cx);
                this.focus_root(window);
            }),
        )
        .into_any_element()
}

fn address_history_popup(
    app: &XoolFiles,
    p: &Colors,
    left: f32,
    width: f32,
    top: f32,
    cx: &mut Context<XoolFiles>,
) -> AnyElement {
    let current = app.active_tab().path.clone();
    let recent = app.settings.recent_paths.clone();

    let mut header = div()
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(7.))
        .py(px(5.))
        .child(glyph("\u{E81C}", 10., p.accent))
        .child(
            div()
                .text_size(px(8.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.dim)
                .child(tr("RUTAS RECIENTES", "RECENT PATHS")),
        )
        .child(div().flex_1());

    if !recent.is_empty() {
        header = header.child(
            div()
                .id("recent-clear")
                .px(px(6.))
                .py(px(2.))
                .rounded(px(4.))
                .text_size(px(8.5))
                .text_color(p.dim)
                .hover(|style| style.bg(p.danger_soft).text_color(p.danger))
                .cursor_pointer()
                .on_click(cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.clear_recent(cx);
                    cx.stop_propagation();
                }))
                .child(tr("Limpiar", "Clear")),
        );
    }

    let mut list = div()
        .id("recent-list")
        .track_scroll(&app.address_history_scroll)
        .flex()
        .flex_col()
        .gap(px(1.))
        .max_h(px(300.))
        .overflow_y_scroll();

    if recent.is_empty() {
        list = list.child(
            div()
                .px(px(9.))
                .py(px(14.))
                .text_size(px(9.5))
                .text_color(p.dim)
                .child(tr("Aún no hay rutas visitadas.", "No paths visited yet.")),
        );
    } else {
        for (index, path) in recent.iter().enumerate() {
            let is_current = app.address_history_selection.map_or(path == &current, |selected| selected == index);
            let name = folder_name(path);
            let full = fs_model::normalize_path(path);
            let target = path.clone();
            list = list.child(
                div()
                    .id(("recent", index))
                    .h(px(30.)).flex_shrink_0()
                    .px(px(7.))
                    .rounded(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .when(is_current, |style| style.bg(p.accent_soft))
                    .hover(|style| style.bg(p.card_hover))
                    .cursor_pointer()
                    .on_click(cx.listener(
                        move |this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                            this.navigate_recent(target.clone(), window, cx);
                            cx.stop_propagation();
                        },
                    ))
                    .child(glyph(
                        "\u{E8B7}",
                        12.,
                        if is_current { p.accent } else { p.dim },
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(if is_current { p.accent } else { p.text })
                                    .text_ellipsis()
                                    .child(SharedString::from(name)),
                            )
                            .child(
                                div()
                                    .text_size(px(8.5))
                                    .text_color(p.dim)
                                    .text_ellipsis()
                                    .child(SharedString::from(full)),
                            ),
                    ),
            );
        }
    }

    div()
        .id("address-history")
        .absolute()
        .left(px(left))
        .top(px(top - 2.))
        .w(px(width))
        .p(px(5.))
        .rounded(px(10.))
        .bg(p.panel)
        .border_1()
        .border_color(p.border)
        .occlude()
        .flex()
        .flex_col()
        .child(header)
        .child(list)
        .with_animation(
            "address-history-in",
            Animation::new(Duration::from_millis(135)).with_easing(ease_out_quint()),
            move |element, progress| element
                .top(px(top - 2. - (1. - progress) * 5.))
                .opacity(progress),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Menús
// ---------------------------------------------------------------------------

fn menu_backdrop(p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    let _ = p;
    div()
        .id("menu-backdrop")
        .absolute()
        .top(px(0.))
        .left(px(0.))
        .right(px(0.))
        .bottom(px(0.))
        .occlude()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this: &mut XoolFiles, _: &MouseDownEvent, _, cx| {
                this.close_context_menu(cx);
            }),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(|this: &mut XoolFiles, _: &MouseDownEvent, _, cx| {
                this.close_context_menu(cx);
            }),
        )
        .into_any_element()
}

const MENU_ITEM_HEIGHT: f32 = 26.0;
const MENU_SEPARATOR_MARGIN: f32 = 3.0;
const MENU_SEPARATOR_LINE: f32 = 1.0;
const CONTEXT_MENU_PADDING: f32 = 4.0;
const CONTEXT_MENU_BORDER: f32 = 1.0;
const CONTEXT_MENU_INSET: f32 = 8.0;

fn context_menu_axis(anchor: f32, viewport: f32, preferred_size: f32) -> (f32, f32) {
    let available = (viewport - 2.0 * CONTEXT_MENU_INSET).max(0.0);
    let size = preferred_size.min(available);
    let position = anchor.clamp(CONTEXT_MENU_INSET, CONTEXT_MENU_INSET + (available - size));
    (position, size)
}

const CONTEXT_HEADER_HEIGHT: f32 = 38.0;
const CONTEXT_MENU_WIDTH: f32 = 312.0;

fn context_menu(
    app: &XoolFiles,
    p: &Colors,
    menu: &app::ContextMenu,
    system_rows: &[ShellRow],
    vw: f32,
    vh: f32,
    window: &Window,
    cx: &mut Context<XoolFiles>,
) -> AnyElement {
    let local_actions = matches!(&menu.target, MenuTarget::SidebarFavorite(_) | MenuTarget::SidebarRecentFile(_) | MenuTarget::Tab(_) | MenuTarget::Address(_) | MenuTarget::AddressAncestors(_));
    let style = if local_actions || matches!(&menu.target, MenuTarget::Sidebar(_)) {
        ContextMenuStyle::Standard
    } else {
        app.settings.context_menu_style
    };
    let (tools, rows) = ordered_context_rows(app, &menu.target, system_rows);
    let ContextMenuLayout { tools, rows, more } = context_menu_layout(style, tools, rows);
    let preferred_width = if local_actions || style == ContextMenuStyle::Minimal { 244. } else { CONTEXT_MENU_WIDTH };
    let header_height = if tools.is_empty() { 0.0 } else { CONTEXT_HEADER_HEIGHT };
    let frame_height = header_height + 2.0 * (CONTEXT_MENU_PADDING + CONTEXT_MENU_BORDER);
    let (x, width) = context_menu_axis(menu.x, vw, preferred_width);
    let (y, _) = context_menu_axis(menu.y, vh, frame_height + rows.iter().map(menu_row_height).sum::<f32>());
    let mut visible = Vec::new();
    let transition = menu.submenu_transition.as_ref().filter(|motion| {
        motion.started.elapsed() < Duration::from_millis(125)
    });
    let (previous, progress) = if let Some(motion) = transition {
        window.request_animation_frame();
        let elapsed = motion.started.elapsed().as_secs_f32() / 0.125;
        (motion.previous.as_slice(), 1.0 - (1.0 - elapsed.clamp(0.0, 1.0)).powi(3))
    } else {
        (menu.pages.as_slice(), 1.0)
    };
    append_context_rows(app, menu, rows, &more, 0, Some(&menu.pages), Some(previous), progress, &mut visible);
    let natural_height = frame_height + visible.iter().map(ContextLine::height).sum::<f32>();
    let height = natural_height.min((vh - CONTEXT_MENU_INSET - y).max(0.0));

    if transition.is_none() && menu.reveal_expanded.replace(false) && height < natural_height
        && let Some(parent) = visible.iter().rposition(|line| line.expanded)
    {
        menu.scroll.scroll_to_item(parent + 1);
    }

    let mut popup = div()
        .id(("context-menu", menu.id))
        .on_mouse_down_out(cx.listener(|this: &mut XoolFiles, _: &MouseDownEvent, _, cx| {
            this.close_context_menu(cx);
        }))
        .absolute().left(px(x)).top(px(y)).w(px(width)).h(px(height))
        .py(px(CONTEXT_MENU_PADDING))
        .flex().flex_col()
        .bg(p.panel).border(px(CONTEXT_MENU_BORDER)).border_color(p.border)
        .rounded(px(10.)).shadow_xl().occlude();

    if !tools.is_empty() {
        popup = popup.child(div().h(px(CONTEXT_HEADER_HEIGHT)).flex_shrink_0().flex()
            .items_center().gap(px(2.)).mx(px(5.)).px(px(1.))
            .border_b_1().border_color(p.border_soft)
            .children(tools.into_iter().filter_map(|row| match row {
                MenuRow::Item(entry) => Some(context_tool(
                    entry.icon, &entry.label, entry.hint, entry.action, entry.danger, p, cx,
                )),
                MenuRow::Separator => None,
            })));
    }

    popup.child(
        div().id("context-menu-scroll").min_h(px(0.)).flex_1()
            .flex().flex_col().overflow_y_scroll().track_scroll(&menu.scroll)
            .children(visible.into_iter().enumerate().map(|(index, line)| {
                let height = line.height();
                let content = match line.row {
                    Some(mut row) => {
                        if line.retiring && let MenuRow::Item(entry) = &mut row {
                            entry.enabled = false;
                        }
                        menu_row_element(row, index, line.depth, line.expanded, p, cx)
                    }
                    None => div().h(px(MENU_ITEM_HEIGHT)).flex_shrink_0()
                        .pl(px(13. + 21. * line.depth as f32)).flex().items_center()
                        .text_size(px(11.)).text_color(p.muted).child(tr("No hay acciones disponibles", "No actions available"))
                        .into_any_element(),
                };
                if line.visibility < 1.0 {
                    div().h(px(height)).min_h(px(0.)).flex_shrink_0().flex().flex_col()
                        .overflow_hidden().opacity(line.visibility).child(content).into_any_element()
                } else {
                    content
                }
            })),
    ).with_animation(
        ("context-menu-in", menu.id),
        Animation::new(Duration::from_millis(105)).with_easing(ease_out_quint()),
        move |element, progress| element
            .top(px(y - 3.0 * (1.0 - progress)))
            .opacity(0.3 + 0.7 * progress),
    ).into_any_element()
}

struct ContextLine {
    row: Option<MenuRow>,
    depth: usize,
    expanded: bool,
    visibility: f32,
    retiring: bool,
}

impl ContextLine {
    fn height(&self) -> f32 {
        self.row.as_ref().map_or(MENU_ITEM_HEIGHT, menu_row_height) * self.visibility
    }
}

fn menu_row_height(row: &MenuRow) -> f32 {
    match row {
        MenuRow::Item(_) => MENU_ITEM_HEIGHT,
        MenuRow::Separator => MENU_SEPARATOR_LINE + 2.0 * MENU_SEPARATOR_MARGIN,
    }
}

fn append_context_rows(
    app: &XoolFiles, menu: &app::ContextMenu, rows: Vec<MenuRow>, more: &[MenuRow], depth: usize,
    current: Option<&[ContextPage]>, previous: Option<&[ContextPage]>, progress: f32,
    visible: &mut Vec<ContextLine>,
) {
    let visibility = match (current.is_some(), previous.is_some()) {
        (true, false) => progress,
        (false, true) => 1.0 - progress,
        _ => 1.0,
    };
    let retiring = current.is_none();
    for mut row in rows {
        let submenu = match &mut row {
            MenuRow::Item(entry) => match &mut entry.action {
                MenuAction::ContextSubmenu(page, action_depth) => {
                    *action_depth = depth;
                    Some(*page)
                }
                _ => None,
            },
            MenuRow::Separator => None,
        };
        let current_branch = submenu.and_then(|page| submenu_tail(current, page));
        let old_branch = submenu.and_then(|page| submenu_tail(previous, page));
        visible.push(ContextLine {
            row: Some(row), depth, expanded: current_branch.is_some(), visibility, retiring,
        });
        if let Some(page) = submenu
            && (current_branch.is_some() || old_branch.is_some())
        {
            let rows = match page {
                ContextPage::More => more.to_vec(),
                ContextPage::View => context_view_rows(app),
                ContextPage::Sort => context_sort_rows(app),
                ContextPage::FavoriteColor => favorite_color_rows(app, &menu.target),
                ContextPage::Shell(key) => menu.submenus.get(&key)
                    .map(|rows| rows.iter().map(|row| native_menu_row(row, depth + 1)).collect())
                    .unwrap_or_default(),
            };
            if rows.is_empty() {
                let visibility = match (current_branch.is_some(), old_branch.is_some()) {
                    (true, false) => progress,
                    (false, true) => 1.0 - progress,
                    _ => 1.0,
                };
                visible.push(ContextLine {
                    row: None, depth: depth + 1, expanded: false, visibility,
                    retiring: current_branch.is_none(),
                });
            } else {
                append_context_rows(app, menu, rows, more, depth + 1, current_branch, old_branch, progress, visible);
            }
        }
    }
}

fn submenu_tail(pages: Option<&[ContextPage]>, page: ContextPage) -> Option<&[ContextPage]> {
    let (&first, rest) = pages?.split_first()?;
    (first == page).then_some(rest)
}

fn context_tool(
    icon: &'static str, label: &str, hint: &str, action: MenuAction, danger: bool,
    p: &Colors, cx: &mut Context<XoolFiles>,
) -> AnyElement {
    let tooltip = if hint.is_empty() { label.to_owned() } else { format!("{label} ({hint})") };
    let hover_color = if danger { p.danger_soft } else { p.accent_faint };
    div().id(SharedString::from(label.to_owned()))
        .w(px(31.)).h(px(30.)).flex_shrink_0().flex().items_center().justify_center()
        .rounded(px(6.)).cursor_pointer()
        .hover(move |button| button.bg(hover_color))
        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx))
        .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, window, cx| {
            this.perform_menu_action(action.clone(), window, cx);
        }))
        .child(glyph(icon, 15., if danger { p.danger } else { p.text }))
        .into_any_element()
}

fn native_menu_row(row: &ShellRow, depth: usize) -> MenuRow {
    match row {
        ShellRow::Separator => MenuRow::Separator,
        ShellRow::Item { label, icon, action, enabled, checked } => {
            let action = match action {
                ShellAction::Invoke(offset) if menu_rotation(label).is_some() => MenuAction::RotateImage(*offset),
                ShellAction::Invoke(offset) => MenuAction::ShellCommand(*offset),
                ShellAction::Expand(key) => MenuAction::ContextSubmenu(ContextPage::Shell(*key), depth),
            };
            MenuRow::Item(MenuEntry {
                icon: shell_action_glyph(label, matches!(&action, MenuAction::ContextSubmenu(..))),
                label: label.clone(), hint: "", action, danger: false, checked: *checked,
                enabled: *enabled, image: icon.clone(),
            })
        }
    }
}

fn shell_action_glyph(label: &str, submenu: bool) -> &'static str {
    let label = label.to_lowercase();
    let groups: &[(&[&str], &str)] = &[
        (&["defender", "antivirus", "seguridad", "bitlocker"], "\u{E729}"),
        (&["winrar", "7-zip", "extraer", "comprimir", "extract", tr("archivo comprimido", "archive")], "\u{F012}"),
        (&["eliminar", "destruir", "delete", "shred"], "\u{E74D}"),
        (&["terminal", "powershell"], "\u{E756}"),
        (&["imprimir", "print"], "\u{E749}"),
        (&["girar", "rotate"], "\u{E7AD}"),
        (&["fondo de escritorio", "background", "wallpaper", "fotos", "paint"], "\u{E91B}"),
        (&["compartir", "share", "enviar", "send to", "acceso a"], "\u{E72D}"),
        (&["favoritos", "favorite"], "\u{E734}"),
        (&["anclar", "unpin", "pin to"], "\u{E718}"),
        (&["versiones anteriores", "previous versions"], "\u{E81C}"),
        (&["acceso directo", "shortcut"], "\u{E71B}"),
        (&["abrir con", "open with", "abrir en", "open in"], "\u{E8A7}"),
        (&["editar", "edit"], "\u{E70F}"),
    ];
    groups.iter().find(|(terms, _)| terms.iter().any(|term| label.contains(term)))
        .map_or(if submenu { "\u{E8B7}" } else { "\u{E7C3}" }, |(_, glyph)| *glyph)
}

fn sort_menu(app: &XoolFiles, p: &Colors, top: f32, cx: &mut Context<XoolFiles>) -> AnyElement {
    let tab = app.active_tab();
    let mut rows = Vec::new();
    for key in [
        SortKey::Name,
        SortKey::Size,
        SortKey::Kind,
        SortKey::Modified,
    ] {
        rows.push(MenuRow::checked(
            sort_icon(key),
            key.label(),
            MenuAction::SetSort(key),
            tab.sort == key,
        ));
    }
    rows.push(MenuRow::checked(
        "\u{E74B}",
        tr("Orden descendente", "Descending order"),
        MenuAction::ToggleSortDesc,
        tab.sort_desc,
    ));
    rows.push(MenuRow::Separator);
    rows.push(MenuRow::checked(
        "\u{E80A}",
        tr("Cuadrícula", "Grid"),
        MenuAction::SetView(ViewMode::Grid),
        app.settings.view == ViewMode::Grid,
    ));
    rows.push(MenuRow::checked(
        "\u{E8FD}",
        tr("Lista", "List"),
        MenuAction::SetView(ViewMode::List),
        app.settings.view == ViewMode::List,
    ));
    rows.push(MenuRow::Separator);
    rows.push(MenuRow::checked(
        "\u{E8F5}",
        tr("Archivos ocultos", "Hidden files"),
        MenuAction::ToggleHidden,
        app.settings.show_hidden,
    ));
    rows.push(MenuRow::checked(
        "\u{E8A1}",
        tr("Vista previa", "Preview"),
        MenuAction::TogglePreview,
        app.settings.show_preview,
    ));

    let panel = div()
        .id("sort-menu")
        .absolute()
        .top(px(top))
        .right(px(88.))
        .w(px(230.))
        .py(px(4.))
        .flex()
        .flex_col()
        .bg(p.panel)
        .border_1()
        .border_color(p.border)
        .rounded(px(10.))
        .shadow_xl()
        .occlude()
        .child(
            div()
                .px(px(12.))
                .pt(px(5.))
                .pb(px(3.))
                .text_size(px(8.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.dim)
                .child(tr("ORDENAR Y VER", "SORT AND VIEW")),
        )
        .children(menu_row_elements(rows, p, cx));
    popup_in(panel, "sort-menu-enter", move |panel, slide| panel.top(px(top - slide)))
}

fn sort_icon(key: SortKey) -> &'static str {
    match key {
        SortKey::Name => "\u{E8CB}",
        SortKey::Size => "\u{E7C3}",
        SortKey::Kind => "\u{E8A5}",
        SortKey::Modified => "\u{E787}",
    }
}

fn favorite_menu_row(app: &XoolFiles, path: &std::path::Path) -> MenuRow {
    if app.is_favorite(path) {
        MenuRow::item("\u{E735}", tr("Quitar de favoritos", "Remove from Favorites"), "", MenuAction::RemoveFavorite(path.to_path_buf()))
    } else {
        MenuRow::item("\u{E734}", tr("Añadir a favoritos", "Add to Favorites"), "", MenuAction::AddFavorite(path.to_path_buf()))
    }
}

fn context_label(label: &str) -> String {
    label.trim_end_matches(['.', '…']).trim_end().to_lowercase()
}

fn context_priority(label: &str) -> Option<usize> {
    match label {
        "abrir" | "open" => Some(0),
        "abrir con" | "open with" => Some(1),
        "winrar" => Some(2),
        "abrir terminal aquí" | "abrir en terminal" | "open in terminal" | "open terminal here" => Some(3),
        "editar en el bloc de notas" | "editar con el bloc de notas" | "editar con bloc de notas"
        | "edit with notepad" | "edit in notepad" => Some(4),
        "editar con photos" | "editar con fotos" | "edit with photos" => Some(5),
        "editar con paint" | "edit with paint" => Some(6),
        "establecer como fondo de escritorio" | "set as desktop background" => Some(7),
        "imprimir" | "print" => Some(8),
        _ => None,
    }
}

struct ContextMenuLayout {
    tools: Vec<MenuRow>,
    rows: Vec<MenuRow>,
    more: Vec<MenuRow>,
}

fn context_menu_layout(style: ContextMenuStyle, tools: Vec<MenuRow>, rows: Vec<MenuRow>) -> ContextMenuLayout {
    match style {
        ContextMenuStyle::Standard => ContextMenuLayout { tools, rows, more: Vec::new() },
        ContextMenuStyle::Minimal => {
            let mut essentials = Vec::new();
            let mut more = Vec::new();
            for row in rows.into_iter().chain(tools) {
                match &row {
                    MenuRow::Item(entry) if is_open_action(&entry.action) || matches!(&entry.action,
                        MenuAction::CopySelection | MenuAction::Paste | MenuAction::Rename(_)
                        | MenuAction::DeleteSelection | MenuAction::NewFolder | MenuAction::Refresh
                        | MenuAction::RestoreRecycle
                    ) => essentials.push(row),
                    MenuRow::Separator if !matches!(more.last(), Some(MenuRow::Item(_))) => {}
                    _ => more.push(row),
                }
            }
            if matches!(more.last(), Some(MenuRow::Separator)) { more.pop(); }
            if !more.is_empty() {
                essentials.push(MenuRow::Separator);
                essentials.push(MenuRow::item("\u{E712}", tr("Más acciones", "More actions"), "", MenuAction::ContextSubmenu(ContextPage::More, 0)));
            }
            ContextMenuLayout { tools: Vec::new(), rows: essentials, more }
        }
    }
}

fn is_open_action(action: &MenuAction) -> bool {
    matches!(action, MenuAction::Open(_) | MenuAction::OpenLocation(_))
}

fn ordered_context_rows(
    app: &XoolFiles, target: &MenuTarget, system: &[ShellRow],
) -> (Vec<MenuRow>, Vec<MenuRow>) {
    let (tools, local) = menu_rows(app, target);
    let file_item = matches!(target, MenuTarget::Item(_)) && !app.in_recycle_bin();
    let sidebar_location = matches!(target, MenuTarget::Sidebar(_));
    let has_local_terminal = local.iter().any(|row| matches!(row,
        MenuRow::Item(entry) if matches!(&entry.action, MenuAction::OpenTerminal(_))
    ));
    let mut preferred = [const { None }; 9];
    let mut pin_to_start = None;
    let mut remaining = Vec::new();
    let native = system.iter().map(|row| (true, native_menu_row(row, 0)));
    let combined = local.into_iter().map(|row| (false, row))
        .chain(std::iter::once((true, MenuRow::Separator)))
        .chain(native);

    for (is_native, mut row) in combined {
        let MenuRow::Item(entry) = &mut row else {
            if matches!(remaining.last(), Some(MenuRow::Item(_))) {
                remaining.push(MenuRow::Separator);
            }
            continue;
        };
        let label = context_label(&entry.label);
        if matches!(&entry.action, MenuAction::OpenTerminal(_)) {
            entry.image = platform::terminal_menu_icon();
        }
        if is_native {
            if sidebar_location {
                match label.as_str() {
                    "anclar a inicio" | "anclar al inicio" | "pin to start" => {
                        pin_to_start = Some(row);
                        continue;
                    }
                    "abrir en pestaña nueva" | "abrir en una pestaña nueva" | "abrir en nueva pestaña"
                    | "open in new tab" | "open in a new tab"
                    | "abrir en el otro panel" | "open in other pane" | "open in the other pane"
                    | "abrir en el terminal" | "abrir en windows terminal" | "open in windows terminal" => continue,
                    _ if context_priority(&label) == Some(3) => continue,
                    _ => {}
                }
            }
            if !sidebar_location && matches!(label.as_str(), "crear acceso directo" | "create shortcut") {
                continue;
            }
            if label.contains("favorito") || label.contains("favorite")
                || matches!(label.as_str(), "mostrar en el explorador" | "show in explorer" | "show in file explorer")
                || has_local_terminal && context_priority(&label) == Some(3)
            {
                continue;
            }
        }
        if file_item && let Some(rank) = context_priority(&label) {
            if preferred[rank].is_none() || (rank == 1 && is_native && entry.enabled) {
                preferred[rank] = Some(row);
            }
        } else {
            remaining.push(row);
        }
    }

    if matches!(remaining.last(), Some(MenuRow::Separator)) { remaining.pop(); }
    if let Some(row) = pin_to_start { remaining.insert(1, row); }
    let mut rows: Vec<_> = preferred.into_iter().flatten().collect();
    if !rows.is_empty() && !remaining.is_empty() { rows.push(MenuRow::Separator); }
    rows.extend(remaining);
    (tools, rows)
}

fn menu_rows(app: &XoolFiles, target: &MenuTarget) -> (Vec<MenuRow>, Vec<MenuRow>) {
    match target {
        MenuTarget::Item(path) => {
            if app.in_recycle_bin() {
                return (vec![
                    MenuRow::item("\u{E71B}", tr("Copiar ruta original", "Copy original path"), "", MenuAction::CopyPath(path.clone())),
                    MenuRow::danger("\u{E74D}", tr("Eliminar definitivamente", "Delete permanently"), tr("Supr", "Del"), MenuAction::DeleteSelection),
                ], vec![
                    MenuRow::item("\u{E7A7}", tr("Restaurar", "Restore"), "Enter", MenuAction::RestoreRecycle),
                    MenuRow::danger("\u{E74D}", tr("Vaciar Papelera", "Empty Recycle Bin"), "", MenuAction::EmptyRecycleBin),
                ]);
            }

            let directory = app.active_tab().entry_for(path).is_some_and(|entry| entry.is_dir);
            let mut tools = vec![
                MenuRow::item("\u{E8C6}", tr("Cortar", "Cut"), "Ctrl+X", MenuAction::CutSelection),
                MenuRow::item("\u{E8C8}", tr("Copiar", "Copy"), "Ctrl+C", MenuAction::CopySelection),
                MenuRow::item("\u{E77F}", tr("Pegar", "Paste"), "Ctrl+V", MenuAction::Paste),
                MenuRow::item("\u{E71B}", tr("Copiar ruta", "Copy path"), "", MenuAction::CopyPath(path.clone())),
            ];
            if app.active_tab().selection.len() == 1 {
                tools.push(MenuRow::item("\u{E70F}", tr("Renombrar", "Rename"), "F2", MenuAction::Rename(path.clone())));
            }
            tools.push(MenuRow::danger("\u{E74D}", tr("Eliminar", "Delete"), tr("Supr", "Del"), MenuAction::DeleteSelection));

            let mut rows = vec![MenuRow::item("\u{E8E5}", tr("Abrir", "Open"), "Enter", MenuAction::Open(path.clone()))];
            if directory {
                rows.extend(location_open_rows(path));
            } else if app.active_tab().selection.len() == 1 {
                rows.push(MenuRow::item("\u{E8A7}", tr("Abrir con…", "Open with…"), "", MenuAction::OpenWith(path.clone())));
            }
            tools.push(favorite_menu_row(app, path));
            tools.push(MenuRow::item("\u{E946}", tr("Propiedades", "Properties"), "", MenuAction::Properties(path.clone())));
            rows.push(MenuRow::item("\u{E756}", tr("Abrir terminal aquí", "Open terminal here"), "", MenuAction::OpenTerminal(
                if directory { path.clone() } else { path.parent().unwrap_or(path).to_path_buf() },
            )));
            let order = context_tool_order(&app.settings);
            tools.sort_by_key(|row| context_tool_key(row).and_then(|key|
                order.iter().position(|item| item == key)).unwrap_or(order.len()));
            let (tools, extra): (Vec<_>, Vec<_>) = tools.into_iter().partition(|row|
                context_tool_key(row).is_none_or(|key| !app.settings.hidden_menu_tools.iter().any(|hidden| hidden == key)));
            if !extra.is_empty() {
                rows.push(MenuRow::Separator);
                rows.extend(extra);
            }
            (tools, rows)
        }
        MenuTarget::Background if app.in_recycle_bin() => (
            vec![MenuRow::item("\u{E72C}", tr("Actualizar", "Refresh"), "F5", MenuAction::Refresh)],
            vec![
                MenuRow::danger("\u{E74D}", tr("Vaciar Papelera", "Empty Recycle Bin"), "", MenuAction::EmptyRecycleBin),
                MenuRow::item("\u{E8B3}", tr("Seleccionar todo", "Select all"), "Ctrl+A", MenuAction::SelectAll),
                MenuRow::item("\u{E80A}", tr("Ver", "View"), "", MenuAction::ContextSubmenu(ContextPage::View, 0)),
                MenuRow::item("\u{E8CB}", tr("Ordenar por", "Sort by"), "", MenuAction::ContextSubmenu(ContextPage::Sort, 0)),
            ],
        ),
        MenuTarget::Background => (
            vec![
                MenuRow::item("\u{E77F}", tr("Pegar", "Paste"), "Ctrl+V", MenuAction::Paste),
                MenuRow::item("\u{E72C}", tr("Actualizar", "Refresh"), "F5", MenuAction::Refresh),
                MenuRow::item("\u{E946}", tr("Propiedades", "Properties"), "", MenuAction::Properties(app.active_tab().path.clone())),
            ],
            vec![
                MenuRow::item("\u{E80A}", tr("Ver", "View"), "", MenuAction::ContextSubmenu(ContextPage::View, 0)),
                MenuRow::item("\u{E8CB}", tr("Ordenar por", "Sort by"), "", MenuAction::ContextSubmenu(ContextPage::Sort, 0)),
                MenuRow::item("\u{E8F4}", tr("Nueva carpeta", "New folder"), "Ctrl+Shift+N", MenuAction::NewFolder),
                MenuRow::item("\u{E8B3}", tr("Seleccionar todo", "Select all"), "Ctrl+A", MenuAction::SelectAll),
                MenuRow::Separator,
                MenuRow::item("\u{E756}", tr("Abrir terminal aquí", "Open terminal here"), "", MenuAction::OpenTerminal(app.active_tab().path.clone())),
                MenuRow::item(SETTINGS_ICON, tr("Ajustes", "Settings"), "Ctrl+,", MenuAction::Settings),
                MenuRow::item("\u{E8BB}", tr("Cerrar pestaña", "Close tab"), "Ctrl+W", MenuAction::CloseTab),
            ],
        ),
        MenuTarget::SidebarRecentFile(path) => (Vec::new(), vec![
            MenuRow::item("\u{E710}", tr("Abrir en una pestaña nueva", "Open in a new tab"), "", MenuAction::RevealRecentFile(path.clone())),
            MenuRow::Separator,
            MenuRow::item("\u{E74D}", tr("Borrar historial", "Clear history"), "", MenuAction::ClearRecentFiles),
        ]),
        MenuTarget::SidebarFavorite(path) => (Vec::new(), vec![
            MenuRow::item("\u{E790}", tr("Cambiar color", "Change color"), "", MenuAction::ContextSubmenu(ContextPage::FavoriteColor, 0)),
            MenuRow::Separator,
            MenuRow::item("\u{E74D}", tr("Quitar de favoritos", "Remove from Favorites"), "", MenuAction::RemoveFavorite(path.clone())),
        ]),
        MenuTarget::Address(path) => {
            let mut rows = vec![
                MenuRow::item("\u{E8B7}", tr("Abrir", "Open"), "", MenuAction::OpenLocation(path.clone())),
                MenuRow::Separator,
                MenuRow::item("\u{E710}", tr("Abrir en una pestaña nueva", "Open in a new tab"), "", MenuAction::OpenInNewTab(path.clone())),
                MenuRow::item("\u{E8A7}", tr("Abrir en una ventana nueva", "Open in a new window"), "", MenuAction::OpenInNewWindow(path.clone())),
                MenuRow::Separator,
                MenuRow::item("\u{E70F}", tr("Editar ruta", "Edit path"), "", MenuAction::EditAddress(path.clone())),
                MenuRow::item("\u{E8C8}", tr("Copiar ruta", "Copy path"), "", MenuAction::CopyPath(path.clone())),
            ];
            if !fs_model::is_recycle_bin(path) {
                rows.push(MenuRow::item("icons/settings-workspaces.svg",
                    tr("Agregar al espacio de trabajo", "Add to workspace"), "", MenuAction::AddToWorkspace(path.clone())));
            }
            rows.extend([
                MenuRow::Separator,
                MenuRow::checked("\u{E8FD}", tr("Compactar rutas", "Compact paths"), MenuAction::ToggleCompactPaths, app.settings.compact_paths),
                MenuRow::checked("\u{E76C}", tr("Ruta por segmentos", "Path breadcrumbs"), MenuAction::TogglePathBreadcrumbs, app.settings.path_breadcrumbs),
            ]);
            (Vec::new(), rows)
        }
        MenuTarget::AddressAncestors(folders) => (Vec::new(), folders.iter().map(|(label, path)|
            MenuRow::item("\u{E8B7}", label.clone(), "", MenuAction::OpenLocation(path.clone()))
        ).collect()),
        MenuTarget::Tab(id) => (Vec::new(), tab_context_rows(app, *id)),
        MenuTarget::Sidebar(path) => {
            let recycle = fs_model::is_recycle_bin(path);
            let mut tools = vec![MenuRow::item("\u{E71B}", tr("Copiar ruta", "Copy path"), "", MenuAction::CopyPath(path.clone()))];
            let mut rows = vec![MenuRow::item("\u{E8E5}", tr("Abrir", "Open"), "", MenuAction::OpenLocation(path.clone()))];
            if recycle {
                rows.push(MenuRow::Separator);
                rows.push(MenuRow::danger("\u{E74D}", tr("Vaciar Papelera", "Empty Recycle Bin"), "", MenuAction::EmptyRecycleBin));
            } else {
                tools.push(MenuRow::item("\u{E946}", tr("Propiedades", "Properties"), "", MenuAction::Properties(path.clone())));
            }
            (tools, rows)
        }
    }
}

fn tab_context_rows(app: &XoolFiles, id: u64) -> Vec<MenuRow> {
    let Some((pane, index)) = app.tab_position(id) else { return Vec::new() };
    let count = app.panes[pane].tabs.len();
    let item = |icon, label, action, enabled| {
        let mut row = MenuRow::item(icon, label, "", action);
        if let MenuRow::Item(entry) = &mut row { entry.enabled = enabled; }
        row
    };
    vec![
        item("\u{E72B}", tr("Mover pestaña a la izquierda", "Move tab left"), MenuAction::MoveTab(id, -1), index > 0),
        item("\u{E72A}", tr("Mover pestaña a la derecha", "Move tab right"), MenuAction::MoveTab(id, 1), index + 1 < count),
        MenuRow::Separator,
        item("\u{E8BB}", tr("Cerrar las otras pestañas", "Close other tabs"), MenuAction::CloseOtherTabs(id), count > 1),
        item("\u{E8BB}", tr("Cerrar pestañas de la derecha", "Close tabs to the right"), MenuAction::CloseTabsToRight(id), index + 1 < count),
        item("\u{E8BB}", tr("Cerrar pestaña", "Close tab"), MenuAction::CloseTabFromMenu(id), true),
    ]
}

fn favorite_color_rows(app: &XoolFiles, target: &MenuTarget) -> Vec<MenuRow> {
    let MenuTarget::SidebarFavorite(path) = target else { return Vec::new() };
    let selected = app.settings.favorites.iter().find(|favorite| favorite.path == *path).map(|favorite| favorite.color);
    crate::settings::favorite_colors().into_iter().map(|(id, label, _)| {
        let mut row = MenuRow::item("\u{E735}", label, "", MenuAction::SetFavoriteColor(path.clone(), id));
        if let MenuRow::Item(entry) = &mut row {
            entry.checked = selected == Some(id);
        }
        row
    }).collect()
}

fn location_open_rows(path: &std::path::Path) -> [MenuRow; 2] {
    [
        MenuRow::item("\u{E8A7}", tr("Abrir en pestaña nueva", "Open in new tab"), tr("Botón central", "Middle click"), MenuAction::OpenInNewTab(path.to_path_buf())),
        MenuRow::item("\u{E9F9}", tr("Abrir en el otro panel", "Open in the other pane"), "", MenuAction::OpenInOtherPane(path.to_path_buf())),
    ]
}

fn context_view_rows(app: &XoolFiles) -> Vec<MenuRow> {
    let mut rows: Vec<_> = [
        ("\u{E80A}", tr("Iconos", "Icons"), ViewMode::Grid),
        ("\u{E8FD}", tr("Lista", "List"), ViewMode::List),
        ("\u{E8A5}", tr("Árbol", "Tree"), ViewMode::Tree),
        ("\u{E8A5}", tr("Detalles", "Details"), ViewMode::Details),
    ].into_iter().map(|(icon, label, view)| MenuRow::checked(
        icon, label, MenuAction::SetView(view), app.settings.view == view,
    )).collect();
    rows.extend([
        MenuRow::Separator,
        MenuRow::checked("\u{E8F5}", tr("Archivos ocultos", "Hidden files"), MenuAction::ToggleHidden, app.settings.show_hidden),
        MenuRow::checked("\u{E9F9}", tr("Panel dividido", "Split pane"), MenuAction::ToggleSplit, app.panes.len() > 1),
        MenuRow::checked("\u{E8A1}", tr("Vista previa", "Preview"), MenuAction::TogglePreview, app.settings.show_preview),
    ]);
    rows
}

fn context_sort_rows(app: &XoolFiles) -> Vec<MenuRow> {
    let tab = app.active_tab();
    let mut rows: Vec<_> = [SortKey::Name, SortKey::Size, SortKey::Kind, SortKey::Modified]
        .into_iter().map(|key| MenuRow::checked(sort_icon(key), key.label(), MenuAction::SetSort(key), tab.sort == key))
        .collect();
    rows.push(MenuRow::Separator);
    rows.push(MenuRow::checked("\u{E74B}", tr("Orden descendente", "Descending order"), MenuAction::ToggleSortDesc, tab.sort_desc));
    rows
}

fn menu_rotation(label: &str) -> Option<bool> {
    match context_label(label).as_str() {
        "girar a la derecha" | "rotate right" | "rotate clockwise" => Some(true),
        "girar a la izquierda" | "rotate left" | "rotate counterclockwise" => Some(false),
        _ => None,
    }
}

fn rotation_icon(clockwise: bool, color: Rgba) -> AnyElement {
    canvas(|_, _, _| {}, move |bounds, _, window, _| {
        let at = |x: f32, y: f32| gpui::point(
            bounds.origin.x + px(if clockwise { x } else { 14.0 - x }),
            bounds.origin.y + px(y),
        );
        let mut arc = gpui::PathBuilder::stroke(px(1.15));
        arc.move_to(at(7.1, 11.8));
        arc.cubic_bezier_to(at(2.1, 6.3), at(4.0, 11.8), at(1.8, 9.2));
        arc.cubic_bezier_to(at(11.4, 5.9), at(2.5, 1.0), at(9.5, 1.0));
        arc.move_to(at(11.7, 2.7));
        arc.line_to(at(11.4, 5.9));
        arc.line_to(at(8.2, 5.6));
        if let Ok(path) = arc.build() { window.paint_path(path, color); }
    }).w(px(14.)).h(px(14.)).into_any_element()
}

fn menu_row_elements(
    rows: Vec<MenuRow>,
    p: &Colors,
    cx: &mut Context<XoolFiles>,
) -> Vec<AnyElement> {
    rows.into_iter().enumerate()
        .map(|(index, row)| menu_row_element(row, index, 0, false, p, cx)).collect()
}

fn menu_row_element(
    row: MenuRow, index: usize, depth: usize, expanded: bool,
    p: &Colors, cx: &mut Context<XoolFiles>,
) -> AnyElement {
    let inset = 21. * depth as f32;
    match row {
        MenuRow::Separator => div()
            .h(px(MENU_SEPARATOR_LINE)).ml(px(8. + inset)).mr(px(8.))
            .my(px(MENU_SEPARATOR_MARGIN)).flex_shrink_0().bg(p.border_soft).into_any_element(),
        MenuRow::Item(entry) => {
            let action = entry.action;
            let expandable = matches!(&action, MenuAction::ContextSubmenu(..));
            let rotate = menu_rotation(&entry.label);
            let glyph_color = match &action {
                MenuAction::SetFavoriteColor(_, color) => rgb(crate::settings::favorite_color(*color)),
                _ if entry.danger => p.danger,
                _ if expanded => p.accent,
                _ => p.muted,
            };
            let mut item = div()
                .id(("menu-row", index))
                .h(px(MENU_ITEM_HEIGHT)).flex_shrink_0().ml(px(4. + inset)).mr(px(4.)).px(px(9.))
                .flex().items_center().gap(px(8.)).rounded(px(6.)).text_size(px(11.))
                .text_color(if entry.danger { p.danger } else { p.text })
                .when(expanded, |style| style.bg(p.accent_faint))
                .when(entry.enabled, |style| {
                    style.hover(|style| style.bg(p.card_hover)).cursor_pointer()
                        .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                            this.perform_menu_action(action.clone(), window, cx);
                        }))
                })
                .when(!entry.enabled, |style| style.opacity(0.4))
                .child(div().w(px(17.)).h(px(17.)).flex_shrink_0()
                    .flex().items_center().justify_center()
                    .child(match entry.image {
                        _ if rotate.is_some() => rotation_icon(rotate == Some(true), glyph_color),
                        Some(image) => img(image).size_full().object_fit(ObjectFit::Contain).into_any_element(),
                        None => glyph(entry.icon, 12., glyph_color).into_any_element(),
                    }))
                .child(div().flex_1().min_w(px(0.)).text_ellipsis().child(SharedString::from(entry.label)));

            if expandable {
                let arrow = if expanded { "\u{E70E}" } else { "\u{E70D}" };
                item = item.child(glyph(arrow, 9., glyph_color));
            } else if entry.checked {
                item = item.child(glyph("\u{E73E}", 10., p.accent));
            } else if !entry.hint.is_empty() {
                item = item.child(div().text_size(px(8.5)).text_color(p.dim).child(entry.hint));
            }
            item.into_any_element()
        }
    }
}

// ---------------------------------------------------------------------------
// Diálogos
// ---------------------------------------------------------------------------

fn dialog_overlay(app: &XoolFiles, p: &Colors, viewport_w: f32, viewport_h: f32, cx: &mut Context<XoolFiles>) -> AnyElement {
    let Some(dialog) = app.dialog.clone() else {
        return div().into_any_element();
    };

    let settings = matches!(dialog, Dialog::Settings);
    let card = match dialog {
        Dialog::Settings => settings_dialog(app, p, viewport_w, viewport_h, cx),
        Dialog::PermanentDelete(items) => permanent_delete_dialog(&items, p, cx),
        Dialog::OpenMultiple(paths) => {
            let body = div().px(px(18.)).py(px(16.)).child(crate::localized!(format, "Se abrirán {} archivos.", "This will open {} files.", paths.len())).into_any_element();
            let footer = div().flex().justify_end().gap(px(8.))
                .child(secondary_button("cancel-open-many", tr("Cancelar", "Cancel"), p, cx.listener(
                    |this: &mut XoolFiles, _: &ClickEvent, window, cx| this.close_dialog(window, cx),
                )))
                .child(primary_button("confirm-open-many", tr("Abrir", "Open"), p, cx.listener(
                    |this: &mut XoolFiles, _: &ClickEvent, window, cx| this.submit_dialog(window, cx),
                )))
                .into_any_element();
            dialog_shell("\u{E8E5}", tr("Abrir varios archivos", "Open multiple files"), tr("Confirma la apertura de la selección.", "Confirm that you want to open the selected files."),
                (420., 400.), p, body, footer, false, cx)
        }
        Dialog::NewFolder(parent) => input_dialog(
            "\u{E8F4}",
            tr("Nueva carpeta", "New folder"),
            tr("La carpeta se creará dentro de la ubicación actual.", "The folder will be created in the current location."),
            Some(&parent),
            tr("Crear carpeta", "Create folder"),
            &app.dialog_input,
            p,
            cx,
            false,
        ),
        Dialog::SidebarGroupName(_) => input_dialog(
            "\u{E8B7}",
            tr("Cambiar nombre del grupo", "Rename group"),
            tr("Elige un nombre para identificar el grupo en la barra lateral.", "Choose a name for the group in the sidebar."),
            None,
            tr("Guardar nombre", "Save name"),
            &app.dialog_input, p, cx, false,
        ),
        Dialog::Message { title, body } => message_dialog(&title, &body, p, cx),
    };

    let overlay = div()
        .id("dialog-backdrop")
        .absolute()
        .top(px(0.))
        .left(px(0.))
        .right(px(0.))
        .bottom(px(0.))
        .flex()
        .items_center()
        .justify_center()
        .bg(p.overlay)
        .occlude()
        .when(settings, |overlay| overlay.child(
            div().id("dialog-dismiss").absolute().top(px(0.)).left(px(0.)).size_full()
                .on_mouse_down(MouseButton::Left, cx.listener(|this: &mut XoolFiles, _: &MouseDownEvent, window, cx| {
                    this.close_dialog(window, cx);
                    cx.stop_propagation();
                })),
        ))
        .child(card);
    if settings {
        let closing = app.settings_closing;
        overlay.with_animation(
            ("settings-transition", app.settings_animation),
            Animation::new(Duration::from_millis(if closing { 155 } else { 190 }))
                .with_easing(ease_out_quint()),
            move |element, progress| {
                let visibility = if closing { 1.0 - progress } else { progress };
                element.pt(px((1.0 - visibility) * 20.0)).opacity(visibility)
            },
        ).into_any_element()
    } else {
        overlay.into_any_element()
    }
}

fn image_resize_controls(controls: Option<&crate::image_edits::ResizeControls>, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    let mut can_save = false;
    let body = if let Some(controls) = controls {
        let [width, height] = controls.dimensions;
        let allowed = crate::image_edits::resize_allowed(controls.original[0], width, height);
        can_save = allowed && controls.original != controls.dimensions;
        div().flex().flex_col().gap(px(9.))
            .child(div().flex().gap(px(8.)).children([
                (tr("Ancho", "Width"), width), (tr("Alto", "Height"), height),
            ].into_iter().enumerate().map(|(axis, (label, value))| {
                div().flex_1().min_w(px(0.)).flex().flex_col().gap(px(5.))
                    .child(div().text_size(px(11.)).text_color(p.muted).child(label))
                    .child(div().flex().h(px(34.)).items_center().gap(px(3.)).px(px(4.))
                        .rounded(px(7.)).border_1().border_color(p.border).bg(p.input)
                        .child(resize_step_button(axis, -1, p, cx))
                        .child(div().id(("resize-number", axis)).h_full().flex_1().min_w(px(0.))
                            .flex().items_center().justify_center().gap(px(4.)).rounded(px(4.))
                            .cursor(CursorStyle::ResizeLeftRight).hover(|value| value.bg(p.card_hover))
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(value.to_string()))
                            .child(div().text_size(px(9.)).text_color(p.muted).child("px"))
                            .on_mouse_down(MouseButton::Left, cx.listener(move |this: &mut XoolFiles, event: &MouseDownEvent, _, cx| {
                                if let Some(controls) = this.image_resize_controls_mut() {
                                    controls.drag = Some((axis, f32::from(event.position.x), controls.dimensions[axis]));
                                }
                                cx.stop_propagation();
                            })))
                        .child(resize_step_button(axis, 1, p, cx)))
            })))
            .child(div().id("resize-lock-ratio").flex().items_center().justify_between().gap(px(8.))
                .cursor_pointer().text_size(px(12.)).child(tr("Mantener proporción", "Keep aspect ratio"))
                .child(switch(controls.locked, p))
                .on_click(cx.listener(|this: &mut XoolFiles, _, _, cx| {
                    if let Some(controls) = this.image_resize_controls_mut() {
                        controls.locked = !controls.locked;
                        if controls.locked {
                            controls.set_dimension(0, controls.dimensions[0] as i32);
                        }
                        cx.notify();
                    }
                })))
            .when(!allowed && controls.original != controls.dimensions, |body| {
                body.child(div().text_size(px(11.)).text_color(p.danger)
                    .child(tr("Reduce las dimensiones para guardar la copia.", "Reduce the dimensions to save the copy.")))
            })
            .into_any_element()
    } else {
        div().text_size(px(11.)).text_color(p.muted).child(tr("Leyendo dimensiones…", "Reading dimensions…")).into_any_element()
    };

    let footer = div().flex().flex_wrap().justify_end().gap(px(6.))
        .child(secondary_button("cancel-image-resize", tr("Cancelar", "Cancel"), p, cx.listener(
            |this: &mut XoolFiles, _, _, cx| this.close_image_tool(cx),
        )))
        .child(image_action_button("save-image-resize", tr("Guardar copia", "Save copy"), None, p)
            .when(!can_save, |button| button.opacity(0.45))
            .when(can_save, |button| {
                button.bg(p.accent).text_color(p.accent_fg).cursor_pointer()
                    .hover(|button| button.bg(p.accent_hover))
                    .on_click(cx.listener(|this: &mut XoolFiles, _, _, cx| this.save_image_resize(cx)))
            }))
        .into_any_element();
    div().id("preview-image-resize-controls").flex().flex_col().gap(px(9.)).p(px(9.))
        .rounded(px(8.)).bg(p.card).border_1().border_color(p.border_soft)
        .child(body).child(footer)
        .with_animation("preview-resize-in", Animation::new(Duration::from_millis(145)).with_easing(ease_out_quint()),
            |panel, progress| panel.relative().top(px(-5. * (1. - progress))).opacity(progress))
        .into_any_element()
}

fn resize_step_button(axis: usize, step: i32, p: &Colors, cx: &mut Context<XoolFiles>) -> gpui::Stateful<Div> {
    let (id, label) = if step < 0 { ("resize-decrease", "−") } else { ("resize-increase", "+") };
    div().id((id, axis)).size(px(22.)).rounded(px(4.)).flex().items_center().justify_center()
        .cursor_pointer().text_color(p.muted).hover(|button| button.bg(p.card_hover).text_color(p.text))
        .child(label)
        .on_click(cx.listener(move |this: &mut XoolFiles, _, _, cx| {
            if let Some(controls) = this.image_resize_controls_mut() {
                controls.set_dimension(axis, controls.dimensions[axis] as i32 + step);
                cx.notify();
            }
        }))
}

fn permanent_delete_dialog(items: &[fs_model::RecycleItem], p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    let subject = if items.len() == 1 {
        items[0].origin.file_name().unwrap_or(items[0].origin.as_os_str())
            .to_string_lossy().into_owned()
    } else {
        crate::localized!(format, "{} elementos seleccionados", "{} items selected", items.len())
    };
    let body = div().px(px(18.)).py(px(18.)).text_size(px(13.)).text_ellipsis()
        .child(SharedString::from(subject)).into_any_element();
    let footer = div().flex().justify_end().gap(px(8.))
        .child(secondary_button("cancel-delete", tr("Cancelar", "Cancel"), p, cx.listener(
            |this: &mut XoolFiles, _: &ClickEvent, window, cx| this.close_dialog(window, cx),
        )))
        .child(primary_button("confirm-delete", tr("Eliminar", "Delete"), p, cx.listener(
            |this: &mut XoolFiles, _: &ClickEvent, window, cx| this.submit_dialog(window, cx),
        )))
        .into_any_element();
    dialog_shell(
        "\u{E74D}", tr("Eliminar definitivamente", "Delete permanently"), tr("Esta acción no se puede deshacer.", "This action cannot be undone."),
        (440., 660.), p, body, footer, false, cx,
    )
}

fn dialog_shell(
    icon: &'static str,
    title: &str,
    subtitle: &str,
    dimensions: (f32, f32),
    p: &Colors,
    body: AnyElement,
    footer: AnyElement,
    compact_footer: bool,
    cx: &mut Context<XoolFiles>,
) -> AnyElement {
    div()
        .id("dialog-card")
        .w(px(dimensions.0))
        .max_h(px(dimensions.1))
        .flex()
        .flex_col()
        .bg(p.panel)
        .border_1()
        .border_color(p.border)
        .rounded(px(14.))
        .shadow_2xl()
        .occlude()
        .on_click(cx.listener(|_: &mut XoolFiles, _: &ClickEvent, _, cx| {
            cx.stop_propagation();
        }))
        .child(
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(11.))
                .px(px(18.))
                .py(px(if subtitle.is_empty() { 10. } else { 14. }))
                .border_b_1()
                .border_color(p.border_soft)
                .child(
                    div()
                        .w(px(34.))
                        .h(px(34.))
                        .rounded(px(10.))
                        .bg(p.accent_soft)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(glyph(icon, if icon == SETTINGS_ICON { 19. } else { 15. }, p.accent)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .text_size(px(if icon == SETTINGS_ICON { 16. } else { 14. }))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(SharedString::from(title.to_string())),
                        )
                        .when(!subtitle.is_empty(), |header| header.child(
                            div()
                                .text_size(px(if icon == SETTINGS_ICON { 11. } else { 9.5 }))
                                .text_color(p.muted)
                                .child(SharedString::from(subtitle.to_string())),
                        )),
                )
                .child(icon_button(
                    "dialog-close",
                    "\u{E8BB}",
                    10.,
                    p,
                    false,
                    true,
                    cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                        this.close_dialog(window, cx);
                    }),
                )),
        )
        .child(body)
        .child(
            div()
                .flex_shrink_0()
                .px(px(18.))
                .py(px(if compact_footer { 5. } else { 12. }))
                .border_t_1()
                .border_color(p.border_soft)
                .child(footer),
        )
        .into_any_element()
}

fn input_dialog(
    icon: &'static str,
    title: &str,
    subtitle: &str,
    path: Option<&std::path::Path>,
    confirm: &'static str,
    input: &Entity<InputState>,
    p: &Colors,
    cx: &mut Context<XoolFiles>,
    danger: bool,
) -> AnyElement {
    let body = div()
        .flex()
        .flex_col()
        .gap(px(9.))
        .px(px(18.))
        .py(px(16.))
        .when_some(path, |body, path| body
            .child(div().text_size(px(9.5)).text_color(p.dim).child(tr("Ubicación", "Location")))
            .child(div().px(px(9.)).py(px(6.)).rounded(px(7.)).bg(p.input)
                .border_1().border_color(p.border_soft).text_size(px(9.5)).text_color(p.muted)
                .text_ellipsis().child(fs_model::normalize_path(path))))
        .child(
            div()
                .mt(px(3.))
                .h(px(36.))
                .flex()
                .items_center()
                .px(px(10.))
                .rounded(px(8.))
                .bg(p.input)
                .border_1()
                .border_color(p.accent)
                .child(
                    Input::new(input)
                        .appearance(false)
                        .bordered(false)
                        .h(px(32.)),
                ),
        )
        .into_any_element();

    let footer = div()
        .flex()
        .justify_end()
        .gap(px(8.))
        .child(secondary_button(
            "dialog-cancel",
            tr("Cancelar", "Cancel"),
            p,
            cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                this.close_dialog(window, cx);
            }),
        ))
        .child(if danger {
            danger_button(
                "dialog-confirm",
                confirm,
                p,
                cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                    this.submit_dialog(window, cx);
                }),
            )
        } else {
            primary_button(
                "dialog-confirm",
                confirm,
                p,
                cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                    this.submit_dialog(window, cx);
                }),
            )
        })
        .into_any_element();

    dialog_shell(icon, title, subtitle, (500., 660.), p, body, footer, false, cx)
}

fn message_dialog(
    title: &str,
    message: &str,
    p: &Colors,
    cx: &mut Context<XoolFiles>,
) -> AnyElement {
    let body = div()
        .px(px(18.))
        .py(px(16.))
        .text_size(px(10.5))
        .text_color(p.muted)
        .child(SharedString::from(message.to_string()))
        .into_any_element();
    let footer = div()
        .flex()
        .justify_end()
        .child(primary_button(
            "message-ok",
            tr("Entendido", "OK"),
            p,
            cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                this.close_dialog(window, cx);
            }),
        ))
        .into_any_element();
    dialog_shell("\u{E946}", title, "Xool Files", (460., 660.), p, body, footer, false, cx)
}

fn settings_dialog_width(viewport_width: f32) -> f32 {
    (viewport_width * 0.78).max(690.).min(viewport_width - 24.).min(1060.)
}

pub(crate) fn settings_navigation_axis(viewport_width: f32) -> gpui::Axis {
    if settings_dialog_width(viewport_width) >= 670. {
        gpui::Axis::Vertical
    } else {
        gpui::Axis::Horizontal
    }
}

fn settings_dialog(app: &XoolFiles, p: &Colors, viewport_w: f32, viewport_h: f32, cx: &mut Context<XoolFiles>) -> AnyElement {
    let width = settings_dialog_width(viewport_w);
    let height = (viewport_h * 0.87).min(920.).min(viewport_h - 24.);
    let body = div()
        .id(("settings-scroll", app.preferences_page))
        .track_scroll(&app.settings_page_scroll)
        .flex_1()
        .min_h(px(0.))
        .overflow_y_scroll()
        .px(px(18.))
        .py(px(14.))
        .flex()
        .flex_col()
        .gap(px(0.));

    let wide = settings_navigation_axis(viewport_w) == gpui::Axis::Vertical;
    let nav_width = if wide {
        app.settings.settings_nav_width.clamp(118., 256.)
    } else {
        (width - 24.).max(0.)
    };
    let nav_progress = app.settings_nav_progress;
    let query = search_shortcut_text(app.settings_search.read(cx).value().as_ref());
    let query = if !app.settings_nav_collapsed && query.chars().count() >= 3 { query.as_str() } else { "" };
    let input = Input::new(&app.settings_search).text_size(px(13.));
    let mut navigation = div().id("settings-navigation").track_scroll(&app.settings_nav_scroll).flex().gap(px(2.)).p(px(10.)).bg(p.panel_soft);
    navigation = navigation.w(px(nav_width)).h_full();
    navigation = if wide {
        navigation.flex_col().overflow_y_scroll()
            .child(div().flex_shrink_0().pb(px(6.)).child(input))
    } else {
        navigation.items_center().overflow_x_scroll()
            .child(div().w(px(164.)).flex_shrink_0().child(input))
    };
    let mut current_page = None;
    let mut matches = 0;
    for (index, label, icon, description) in [
        (1, "General", SETTINGS_ICON, tr("Configura el inicio, los paneles y las vistas de carpeta.", "Configure startup, panes, and folder views.")),
        (0, tr("Apariencia", "Appearance"), "\u{E790}", tr("Elige el tema, los colores y el tamaño de la interfaz.", "Choose the theme, colors, and interface size.")),
        (7, tr("Barra lateral", "Sidebar"), "icons/settings-sidebar.svg", tr("Elige qué grupos aparecen y cómo se muestran.", "Choose which groups appear and how they are displayed.")),
        (12, tr("Barra de pestañas", "Tab bar"), "\u{E8A7}", tr("Personaliza los iconos, el cierre y el historial de pestañas.", "Customize tab icons, close buttons, and history.")),
        (13, tr("Espacios de trabajo", "Workspaces"), "icons/settings-workspaces.svg", tr("Organiza tus carpetas fijadas y los grupos de cada espacio.", "Organize pinned folders and groups in each workspace.")),
        (8, tr("Archivos y menús", "Files and menus"), "\u{E8B7}", tr("Ajusta la apertura de archivos y las opciones del menú contextual.", "Configure how files open and how context menus work.")),
        (2, tr("Multimedia", "Media"), "\u{E768}", tr("Personaliza la vista previa de audio y los sonidos de acciones.", "Customize audio previews and action sounds.")),
        (3, tr("Búsqueda", "Search"), "\u{E721}", tr("Configura el buscador global, la indexación y los filtros.", "Configure global search, indexing, and filters.")),
        (6, tr("Atajos", "Shortcuts"), "\u{E765}", tr("Consulta y personaliza las combinaciones de teclado.", "View and customize keyboard shortcuts.")),
        (11, tr("Integración", "Integration"), "icons/settings-integration.svg", tr("Configura el inicio con Windows y el comportamiento en segundo plano.", "Configure Windows startup and background behavior.")),
        (app::ABOUT_SETTINGS_PAGE, tr("Acerca de", "About"), "\u{E946}", tr("Consulta la versión, el uso de espacio y el archivo de configuración.", "View the version, storage usage, and settings file.")),
    ] {
        let selected = app.preferences_page == index;
        let search = SettingsSearch { query, matched: Cell::new(false) };
        let title_match = search.matches(&[label]);
        if selected || !query.is_empty() {
            let content = settings_page(app, index, &search, p, cx);
            let heading = settings_heading(&search, label, description, index == app::ABOUT_SETTINGS_PAGE, p);
            if selected {
                current_page = Some(div().flex().flex_col().flex_shrink_0()
                    .child(heading).child(content));
            }
        }
        if !query.is_empty() && !search.matched.get() { continue; }
        matches += 1;
        navigation = navigation.child(
            div()
                .id(("xool-preferences", index))
                .flex_shrink_0()
                .flex().items_center().gap(px(8.))
                .px(px(10.))
                .py(px(6.))
                .rounded(px(8.))
                .hover(|style| style.bg(p.card_hover))
                .when(selected, |row| row.bg(p.accent_faint))
                .text_color(if selected { p.accent } else { p.text })
                .text_size(px(13.))
                .cursor_pointer()
                .child(glyph(icon, 12., if selected { p.accent } else { p.muted }).font_weight(FontWeight::NORMAL))
                .child(div().min_w(px(0.)).truncate()
                    .when(title_match, |label| label.bg(rgba(SETTINGS_HIGHLIGHT))).child(label))
                .on_click(
                    cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                        if this.settings_closing { return; }
                        if this.preferences_page != index {
                            if this.workspace_ui.edit == Some(crate::workspaces::NameEdit::SettingsPreset) {
                                this.workspace_ui.edit = None;
                            }
                            this.creating_sidebar_group = false;
                            this.recording_shortcut = None;
                            this.settings_theme_open = false;
                            this.settings_page_scroll = gpui::ScrollHandle::new();
                        }
                        this.preferences_page = index;
                        if index == app::ABOUT_SETTINGS_PAGE {
                            this.refresh_storage_info(cx);
                        }
                        cx.notify();
                    }),
                ),
        );
    }
    if matches == 0 {
        navigation = navigation.child(div().text_size(px(12.)).text_color(p.muted).child(tr("No se encontraron secciones.", "No matching sections.")));
    }
    let clip = div().relative().overflow_hidden().flex_shrink_0().h_full()
        .w(px(nav_width * nav_progress))
        .when(nav_progress > 0., |clip| clip.child(
            div().absolute().right(px(0.)).top(px(0.)).h_full().w(px(nav_width)).child(navigation),
        ));
    let collapsed = app.settings_nav_collapsed;
    let tooltip = if collapsed { tr("Mostrar secciones de Ajustes", "Show settings sections") } else { tr("Ocultar secciones de Ajustes", "Hide settings sections") };
    let toggle = div().id("settings-nav-toggle").flex_shrink_0().w(px(18.)).h(px(30.))
        .flex().items_center().justify_center().rounded(px(5.)).bg(p.panel_soft)
        .text_color(p.muted).cursor_pointer()
        .hover(|button| button.bg(p.card_hover).text_color(p.text))
        .child(glyph_inherit(if collapsed { "\u{E76C}" } else { "\u{E76B}" }, 9.))
        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tooltip).build(window, cx))
        .on_click(cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| {
            this.toggle_settings_navigation(window, cx);
        }));
    let nav = div().flex().items_center().flex_shrink_0()
        .when(wide, |strip| strip.h_full())
        .when(!wide, |strip| strip.h(px(30. + 24. * nav_progress)))
        .child(clip)
        .when(wide && nav_progress > 0., |strip| strip.child(
            div().id("settings-nav-divider").w(px(5. * nav_progress)).h_full().flex().justify_center()
                .cursor(CursorStyle::ResizeLeftRight)
                .child(div().w(px(1.)).h_full().bg(p.border_soft))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this: &mut XoolFiles, event: &MouseDownEvent, _, cx| {
                    if !this.settings_nav_collapsed && this.settings_nav_progress == 1. {
                        this.settings_nav_drag = Some((f32::from(event.position.x), nav_width));
                    }
                    cx.stop_propagation();
                })),
        ))
        .child(toggle);

    let body = body.text_size(px(13.)).child(current_page.expect("settings page is listed"));
    let footer = div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_size(px(11.5))
                .text_color(p.dim)
                .child(crate::localized!(format, "Xool Files {} · GPUI nativo", "Xool Files {} · Native GPUI", env!("CARGO_PKG_VERSION"))),
        )
        .into_any_element();

    dialog_shell(
        SETTINGS_ICON,
        tr("Ajustes", "Settings"),
        "",
        (width, height),
        p,
        div().flex().h(px((height - 84.).max(140.))).min_h(px(0.)).overflow_hidden()
            .when(!wide, |layout| layout.flex_col())
            .child(nav)
            .child(body.min_w(px(0.)).when(wide, |page| page.pl(px(8. + 10. * nav_progress))))
            .into_any_element(),
        footer,
        true,
        cx,
    )
}

const SETTINGS_HIGHLIGHT: u32 = 0xe3b84438;

struct SettingsSearch<'a> {
    query: &'a str,
    matched: Cell<bool>,
}

impl SettingsSearch<'_> {
    fn matches(&self, labels: &[&str]) -> bool {
        let matched = !self.query.is_empty()
            && labels.iter().any(|label| search_shortcut_text(label).contains(self.query));
        self.matched.set(self.matched.get() || matched);
        matched
    }

    fn text<E: Styled + ParentElement>(&self, element: E, label: &'static str) -> E {
        self.mark(element, &[label]).child(label)
    }

    fn mark<E: Styled>(&self, element: E, labels: &[&str]) -> E {
        if self.matches(labels) { element.bg(rgba(SETTINGS_HIGHLIGHT)).rounded(px(5.)) } else { element }
    }
}

fn settings_page(
    app: &XoolFiles, page: usize, search: &SettingsSearch<'_>, p: &Colors, cx: &mut Context<XoolFiles>,
) -> Div {
    let mut body = div().flex().flex_col().flex_shrink_0().min_w(px(0.));
    if page == 13 {
        body = body.child(workspace_panel::preferences(search, app, p, cx));
    }
    if page == 0 {
        body = body.child(appearance_settings(search, app, p, cx));
    }
    if page == 7 {
        body = body.child(sidebar_preferences(search, app, p, cx));
    }
    if page == 8 {
        body = body
            .child(settings_section(search, tr("ARCHIVOS Y CARPETAS", "FILES AND FOLDERS"), p))
            .child(settings_surface(p)
            .child(toggle_row(search, "single-click-files", tr("Abrir archivos con un clic", "Open files with a single click"),
                tr("Usa un clic para abrir archivos. Ctrl y Mayús permiten seleccionar.", "Use one click to open files. Use Ctrl or Shift to select."), app.settings.single_click_files, p,
                cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.settings.single_click_files = !this.settings.single_click_files;
                    this.settings.save();
                    cx.notify();
                })))
            .child(settings_divider(p))
            .child(toggle_row(search, 
                "show-extensions",
                tr("Mostrar extensiones", "Show file extensions"),
                tr("Muestra el sufijo de los archivos en las vistas.", "Show file extensions in folder views."),
                app.settings.show_extensions,
                p,
                cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.settings.show_extensions = !this.settings.show_extensions;
                    this.settings.save();
                    cx.notify();
                }),
            ))
            .child(settings_divider(p))
            .child(toggle_row(search, 
                "hidden-visible",
                tr("Mostrar elementos ocultos", "Show hidden items"),
                tr("Incluye archivos y carpetas marcados como ocultos.", "Include files and folders marked as hidden."),
                app.settings.show_hidden,
                p,
                cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.toggle_hidden(cx);
                }),
            ))
            .child(settings_divider(p))
            .child(toggle_row(search, "hover-details", tr("Detalles al pasar el ratón", "Show details on hover"),
                tr("Elige los datos que aparecen al dejar el puntero sobre un elemento.", "Choose what appears when you hover over an item."),
                app.settings.hover_details, p,
                cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.settings.hover_details = !this.settings.hover_details;
                    this.settings.save();
                    cx.notify();
                })))
            .child(settings_divider(p))
            .when(app.settings.hover_details || !search.query.is_empty(), |panel| panel.child(
                div().flex().flex_col().flex_shrink_0().overflow_hidden()
                    .child(hover_field_options(search, app, p, cx))
                    .child(settings_divider(p))
                    .with_animation("hover-details-fields-in",
                        Animation::new(Duration::from_millis(180)).with_easing(ease_out_quint()),
                        |body, progress| body.max_h(px(208. * progress)).opacity(progress)),
            ))
                .child(toggle_row(search, "confirm-open-many", tr("Confirmar antes de abrir varios archivos", "Confirm before opening multiple files"),
                    tr("Pregunta antes de abrir una selección grande.", "Ask before opening a large selection."), app.settings.confirm_open_multiple, p,
                    cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                        this.settings.confirm_open_multiple = !this.settings.confirm_open_multiple;
                        this.settings.save();
                        cx.notify();
                    })))
                .when(app.settings.confirm_open_multiple, |card| card
                    .child(settings_divider(p))
                    .child(div().px(px(14.)).py(px(8.)).flex().items_center().justify_between()
                        .child(tr("Pedir confirmación a partir de", "Ask for confirmation starting at"))
                        .child(div().flex().gap(px(4.)).children([2, 5, 10, 20].map(|count| {
                            div().id(("open-count", count)).px(px(8.)).py(px(6.)).rounded(px(7.))
                                .cursor_pointer().when(app.settings.confirm_open_count == count, |button| button.bg(p.card_hover))
                                .child(format!("{count}"))
                                .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                                    this.settings.confirm_open_count = count;
                                    this.settings.save();
                                    cx.notify();
                                }))
                        })))))
                .child(settings_divider(p))
            .child(toggle_row(search, 
                "confirm-delete",
                tr("Confirmar antes de eliminar", "Confirm before deleting"),
                tr("Pide confirmación antes de mover a la papelera.", "Ask for confirmation before moving items to the Recycle Bin."),
                app.settings.confirm_delete,
                p,
                cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.settings.confirm_delete = !this.settings.confirm_delete;
                    this.settings.save();
                    cx.notify();
                }),
            ))
                .child(settings_divider(p))
                .child(toggle_row(search, "undo-file-operations", tr("Deshacer operaciones", "Undo operations"),
                    tr("Usa Ctrl+Z para revertir la última operación compatible.", "Use Ctrl+Z to undo the last supported operation."), app.settings.undo_file_operations, p,
                    cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| this.toggle_file_undo(cx)))))
            .child(settings_section(search, tr("VISTA PREVIA", "PREVIEW"), p))
            .child(settings_surface(p).child(toggle_row(search, "edit-text-preview", tr("Permitir editar archivos de texto", "Allow editing text files"),
                tr("Activa la edición de TXT, MD, LOG, JSON y CSV en el panel de vista previa.", "Edit TXT, MD, LOG, JSON, and CSV files in the preview pane."), app.settings.edit_text_preview, p,
                cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.settings.edit_text_preview = !this.settings.edit_text_preview;
                    this.settings.save();
                    cx.notify();
                }))))
            .child(div().mt(px(12.)).child(context_menu_settings(search, app, p, cx)));
    }
    if page == 2 {
        body = body.child(multimedia_settings(search, app, p, cx));
    }
    if page == 1 {
        body = body.child(general_settings(search, app, p, cx));
    }
    if page == 12 {
        let active_limit = match app.settings.closed_tab_limit { 10 => 1, 15 => 2, _ => 0 };
        body = body
            .child(settings_section(search, tr("VISTA PREVIA", "PREVIEW"), p))
            .child(tab_settings_preview(app, p))
            .child(div().h(px(10.)))
            .child(settings_surface(p)
                .child(wheel_tabs_setting(search, app, p, cx))
                .child(settings_divider(p))
                .child(toggle_row(search, "active-tab-gradient",
                    tr("Degradado en la pestaña activa", "Active tab gradient"),
                    tr("Destaca la pestaña seleccionada con el color de acento.", "Highlight the selected tab with the theme accent."),
                    app.settings.active_tab_gradient, p,
                    cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                        this.settings.active_tab_gradient = !this.settings.active_tab_gradient;
                        this.settings.save();
                        cx.notify();
                    })))
                .when(app.settings.active_tab_gradient || app.tab_gradient_reveal.progress() > 0., |card| card
                    .child(tab_settings_suboptions(&app.tab_gradient_reveal, tab_gradient_settings(search, app, p, cx))))
                .child(settings_divider(p))
                .child(toggle_row(search, "show-tab-close", tr("Mostrar botón Cerrar", "Show close button"),
                    tr("Muestra la X al pasar el cursor sobre una pestaña.", "Show the X when hovering over a tab."), app.settings.show_tab_close, p,
                    cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                        this.settings.show_tab_close = !this.settings.show_tab_close;
                        this.settings.save();
                        cx.notify();
                    })))
                .when(app.settings.show_tab_close || app.tab_close_reveal.progress() > 0., |card| card.child(
                    tab_settings_suboptions(&app.tab_close_reveal, div().flex().flex_col()
                    .child(settings_divider(p))
                    .child(toggle_row(search, "tab-close-left", tr("En el lado izquierdo", "On the left side"),
                        tr("La X aparece sobre el icono de la pestaña.", "The X appears over the tab icon."), app.settings.tab_close_left, p,
                        cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                            this.settings.tab_close_left = !this.settings.tab_close_left;
                            this.settings.save();
                            cx.notify();
                        })).pl(px(24.)))
                    .child(settings_divider(p))
                    .child(toggle_row(search, "tab-close-always", tr("Siempre en las pestañas", "Always on tabs"),
                        tr("Muestra la X aunque el cursor esté fuera.", "Show the X even when the pointer is away."), app.settings.tab_close_always, p,
                        cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                            this.settings.tab_close_always = !this.settings.tab_close_always;
                            this.settings.save();
                            cx.notify();
                        })).pl(px(24.))))))
            .child(settings_divider(p))
            .child(toggle_row(search, "compact-flush-tabs", tr("Quitar espacio sobre las pestañas al maximizar", "Remove space above tabs when maximized"),
                tr("Ajusta la barra de título a la altura de las pestañas y elimina el hueco entre ellas.", "Align the title bar with the tabs and remove the gap between tabs."),
                app.settings.flush_tabs_when_maximized, p, cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.settings.flush_tabs_when_maximized = !this.settings.flush_tabs_when_maximized;
                    this.settings.save();
                    cx.notify();
                })))
                .child(settings_divider(p))
                .child(settings_segments(search, "closed-tab-limit", tr("Pestañas recientes en el menú", "Recent tabs in menu"),
                    ["5", "10", "15"], active_limit, p, cx, |this, index, _, cx| {
                        this.settings.closed_tab_limit = [5, 10, 15][index];
                        this.settings.save();
                        cx.notify();
                    }))
                .child(settings_divider(p))
                .child(toggle_row(search, "system-tab-icons", tr("Usar iconos de Windows", "Use Windows icons"),
                    tr("Muestra la carpeta amarilla y el icono propio de ubicaciones como Descargas.", "Show the yellow folder icon and native icons for locations like Downloads."),
                    app.settings.use_system_tab_icons, p,
                    cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                        this.settings.use_system_tab_icons = !this.settings.use_system_tab_icons;
                        this.settings.save();
                        cx.notify();
                    })))
                .child(settings_divider(p))
                .child(toggle_row(search, "hide-tab-icons", tr("Ocultar iconos de las pestañas", "Hide tab icons"),
                    tr("Muestra solo el nombre en la barra de pestañas.", "Show only the name in the tab bar."), app.settings.hide_tab_icons, p,
                    cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                        this.settings.hide_tab_icons = !this.settings.hide_tab_icons;
                        this.settings.save();
                        cx.notify();
                    }))));
    }
    if page == 11 {
        body = body.child(integration_settings(search, app, p, cx));
    }
    if page == 3 {
        body = body
            .child(quick_search_settings(search, app, p, cx))
            .child(div().h(px(10.)).flex_shrink_0())
            .child(search_index_settings(search, app, p, cx))
            .child(div().h(px(10.)).flex_shrink_0())
            .child(search_filter_settings(search, app, p, cx));
    }
    if page == app::ABOUT_SETTINGS_PAGE {
        let clear_cache = tr("Borrar caché de miniaturas", "Clear thumbnail cache");
        let info = app.storage_info.clone().unwrap_or_default();
        let total = info.total.max(1);
        let used = info.total.saturating_sub(info.free);
        let fraction = (used as f32 / total as f32).clamp(0.0, 1.0);
        let xool_total = info.app + info.settings + info.cache;
        let cache_location = info
            .cache_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| tr("No disponible", "Unavailable").to_string());

        body = body
            .child(settings_section(search, tr("USO DE ALMACENAMIENTO", "STORAGE USAGE"), p))
            .child(
                settings_surface(p).p(px(14.)).mb(px(8.))
                    .gap(px(8.))
                    .child(
                        search.text(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD), tr("Almacenamiento del dispositivo", "Device storage")),
                    )
                    .child(
                        div()
                            .h(px(8.))
                            .w_full()
                            .rounded(px(4.))
                            .bg(p.input)
                            .child(
                                div()
                                    .h_full()
                                    .w(gpui::relative(fraction))
                                    .rounded(px(4.))
                                    .bg(p.accent),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .text_size(px(11.))
                            .text_color(p.dim)
                            .child(crate::localized!(format,
                                "{} usados de {}", "{} used of {}",
                                format_bytes(used),
                                format_bytes(info.total)
                            ))
                            .child(crate::localized!(format, "{} libres", "{} free", format_bytes(info.free))),
                    ),
            )
            .child(
                settings_surface(p).p(px(14.)).mb(px(8.))
                    .gap(px(7.))
                    .child(
                        search.text(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD), tr("Usado por Xool Files", "Used by Xool Files")),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(p.accent)
                            .child(crate::localized!(format, "Espacio total: {}", "Total space: {}", format_bytes(xool_total))),
                    )
                    .child(storage_row(tr("Aplicación y motor", "Application and engine"), info.app, p))
                    .child(storage_row(tr("Configuración", "Settings"), info.settings, p))
                    .child(storage_row(
                        &crate::localized!(format, "Caché de miniaturas ({} archivos)", "Thumbnail cache ({} files)", info.cache_files),
                        info.cache,
                        p,
                    ))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(p.dim)
                            .child(cache_location),
                    )
                    .child(
                        search.mark(div().pt(px(3.)), &[clear_cache]).child(danger_button(
                            "clear-thumb-cache",
                            clear_cache,
                            p,
                            cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                                this.clear_thumbnail_cache(cx);
                            }),
                        )),
                    )
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(p.dim)
                    .child(tr("Las miniaturas se guardan para abrir las carpetas al instante. Borrar la caché no elimina tus archivos.", "Cached thumbnails help folders open faster. Clearing the cache leaves your files intact.")),
            )
            .child(settings_file_storage(search, p, cx));
    }
    if page == 6 {
        let customized = !app.settings.shortcuts.is_empty();
        let shortcut_query = search_shortcut_text(app.shortcut_search.read(cx).value().as_ref());
        let matches: Vec<_> = crate::shortcuts::SHORTCUTS.iter().enumerate().filter(|(_, def)| {
            let name = search_shortcut_text(def.label());
            let keys = search_shortcut_text(&crate::shortcuts::display(&app.settings.shortcut(def.id)));
            let found = search.matches(&[def.label(), &keys]);
            format!("{name} {keys}").contains(&shortcut_query) || found
        }).collect();
        body = body
            .child(div().mb(px(7.)).px(px(8.)).rounded(px(7.)).bg(p.panel_soft)
                .border_1().border_color(p.border).flex().items_center().gap(px(5.))
                .child(glyph("\u{E721}", 12., p.muted))
                .child(div().flex_1().child(Input::new(&app.shortcut_search).appearance(false).bordered(false).text_size(px(13.)))))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(10.))
                    .child(settings_section(search, tr("ATAJOS DE TECLADO", "KEYBOARD SHORTCUTS"), p))
                    .when(customized, |style| {
                        style.child(
                            div()
                                .id("shortcuts-reset-all")
                                .px(px(9.))
                                .py(px(4.))
                                .rounded(px(6.))
                                .bg(p.card)
                                .border_1()
                                .border_color(p.border)
                                .text_size(px(10.5))
                                .text_color(p.muted)
                                .cursor_pointer()
                                .hover(|style| style.bg(p.card_hover).text_color(p.text))
                                .on_click(cx.listener(
                                    |this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                                        this.reset_all_shortcuts(cx);
                                    },
                                ))
                                .child(tr("Restablecer todo", "Reset all")),
                        )
                    }),
            )
            .child(
                settings_surface(p).p(px(10.))
                    .when(matches.is_empty(), |list| list.child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(p.muted)
                        .child(tr("No se encontraron atajos.", "No shortcuts found."))))
                    .children(
                        matches.iter().copied().enumerate().map(|(position, (index, def))| {
                            div().flex().flex_col().flex_shrink_0()
                                .when(position > 0, |row| row.child(settings_divider(p)))
                                .child(shortcut_editor_row(search, app, def, index, p, cx))
                        }),
                    ),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(p.dim)
                    .child(tr("Pulsa un atajo para cambiarlo. La × (o Supr mientras grabas) lo deja sin asignar. Los atajos se activan cuando la ventana de Xool Files tiene el foco.", "Click a shortcut to change it. Click × or press Delete while recording to clear it. Shortcuts work when Xool Files has focus.")),
            );
    }
    body
}

fn context_tools() -> [(&'static str, &'static str, &'static str); 8] {
    [
    ("cut", tr("Cortar", "Cut"), "\u{E8C6}"),
    ("copy", tr("Copiar", "Copy"), "\u{E8C8}"),
    ("paste", tr("Pegar", "Paste"), "\u{E77F}"),
    ("path", tr("Copiar ruta", "Copy path"), "\u{E71B}"),
    ("rename", tr("Renombrar", "Rename"), "\u{E70F}"),
    ("delete", tr("Eliminar", "Delete"), "\u{E74D}"),
    ("favorite", tr("Favoritos", "Favorites"), "\u{E734}"),
    ("properties", tr("Información", "Information"), "\u{E946}"),
    ]
}

fn context_tool_key(row: &MenuRow) -> Option<&'static str> {
    let MenuRow::Item(entry) = row else { return None; };
    Some(match &entry.action {
        MenuAction::CutSelection => "cut",
        MenuAction::CopySelection => "copy",
        MenuAction::Paste => "paste",
        MenuAction::CopyPath(_) => "path",
        MenuAction::Rename(_) => "rename",
        MenuAction::DeleteSelection => "delete",
        MenuAction::AddFavorite(_) | MenuAction::RemoveFavorite(_) => "favorite",
        MenuAction::Properties(_) => "properties",
        _ => return None,
    })
}

pub(crate) fn context_tool_order(settings: &Settings) -> Vec<String> {
    let mut order = Vec::new();
    let tools = context_tools();
    for key in settings.menu_tools_order.iter().map(String::as_str).chain(tools.map(|tool| tool.0)) {
        if tools.iter().any(|tool| tool.0 == key) && !order.iter().any(|item| item == key) {
            order.push(key.to_owned());
        }
    }
    order
}

pub(crate) const MENU_TOOL_ROW_HEIGHT: f32 = 33.;
struct MenuToolDrag(&'static str);

fn context_toolbar_settings(search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    let order = context_tool_order(&app.settings);
    let enabled = |key: &str| !app.settings.hidden_menu_tools.iter().any(|hidden| hidden == key);
    let preview = div().flex().items_center().flex_wrap().gap(px(4.)).px(px(7.)).py(px(4.))
        .min_h(px(33.)).rounded(px(8.)).border_1().border_color(p.border_soft).bg(p.input)
        .when(!order.iter().any(|key| enabled(key)), |preview|
            preview.child(div().px(px(4.)).text_size(px(11.)).text_color(p.muted)
                .child(tr("No hay iconos visibles", "No visible icons"))))
        .children(order.iter().filter(|key| enabled(key)).filter_map(|key| {
            let &(_, label, icon) = context_tools().iter().find(|tool| tool.0 == key)?;
            Some(div().id((ElementId::from("tool-preview"), label)).size(px(24.)).rounded(px(5.)).bg(p.card)
                .flex().items_center().justify_center().child(glyph(icon, 13., p.text))
                .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(label).build(window, cx)))
        }));
    let rows = div().flex().flex_col().rounded(px(8.)).border_1().border_color(p.border_soft)
        .overflow_hidden().bg(p.input)
        .children(order.iter().enumerate().filter_map(|(index, key)| {
            let &(key, label, icon) = context_tools().iter().find(|tool| tool.0 == key)?;
            let active = enabled(key);
            let move_button = |direction: isize, cx: &mut Context<XoolFiles>| {
                let target = index.checked_add_signed(direction).filter(|target| *target < order.len());
                let (id, icon, hint) = if direction < 0 {
                    ("tool-up", "\u{E74A}", tr("Subir", "Move up"))
                } else {
                    ("tool-down", "\u{E74B}", tr("Bajar", "Move down"))
                };
                div().id((ElementId::from(id), key)).size(px(24.)).flex().items_center().justify_center()
                    .rounded(px(5.)).opacity(if target.is_some() { 1. } else { 0.35 })
                    .child(glyph(icon, 11., p.muted))
                    .when_some(target, |button, target| button.cursor_pointer()
                        .hover(|style| style.bg(p.card_hover))
                        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(hint).build(window, cx))
                        .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                            this.reorder_menu_tool(key, target, cx);
                        })))
            };
            let colors = *p;
            let offset = app.menu_tool_offset(key, index);
            let dragging = app.menu_tool_target.is_some_and(|(from, _)| from == index);
            let row = div().id((ElementId::from("menu-tool-row"), key))
                .relative().top(px(offset)).size_full().px(px(8.))
                .flex().items_center().gap(px(6.)).bg(p.input)
                .when(index > 0, |row| row.border_t_1().border_color(p.border_soft))
                .when(dragging, |row| row.opacity(0.38))
                .hover(|row| row.bg(p.card))
                .child(div().id((ElementId::from("tool-grip"), key)).size(px(22.)).rounded(px(5.))
                    .flex().items_center().justify_center().cursor(CursorStyle::OpenHand)
                    .hover(|grip| grip.bg(p.card_hover))
                    .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new(tr("Arrastrar para reordenar", "Drag to reorder")).build(window, cx))
                    .child(div().flex().flex_col().gap(px(2.)).children((0..3).map(|_| {
                        div().flex().gap(px(2.)).children((0..2).map(|_| {
                            div().size(px(2.)).rounded_full().bg(p.muted)
                        }))
                    })))
                    .on_drag(MenuToolDrag(key), move |_, _, _, cx| {
                        cx.new(|_| DragGhost { label: label.into(), count: 1, colors, icon })
                    }))
                .child(div().size(px(23.)).flex_shrink_0().rounded(px(6.))
                    .bg(if active { p.accent_faint } else { p.card })
                    .flex().items_center().justify_center()
                    .child(glyph(icon, 13., if active { p.accent } else { p.muted })))
                .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(12.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if active { p.text } else { p.muted }).child(label))
                .child(div().flex().flex_shrink_0().p(px(1.)).gap(px(1.)).rounded(px(7.))
                    .border_1().border_color(p.border_soft).bg(p.card)
                    .child(move_button(-1, cx)).child(move_button(1, cx)))
                .child(div().id((ElementId::from("tool-visibility"), key)).size(px(33.)).flex_shrink_0()
                    .flex().items_center().justify_center().cursor_pointer()
                    .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(
                        if active { tr("Ocultar icono", "Hide icon") } else { tr("Mostrar icono", "Show icon") },
                    ).build(window, cx))
                    .child(switch(active, p))
                    .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                        let hidden = &mut this.settings.hidden_menu_tools;
                        if active { hidden.push(key.to_owned()); } else { hidden.retain(|item| item != key); }
                        this.settings.save();
                        cx.notify();
                    })));
            Some(div().id((ElementId::from("menu-tool-slot"), key))
                .h(px(MENU_TOOL_ROW_HEIGHT)).flex_shrink_0()
                .on_drag_move::<MenuToolDrag>(cx.listener(
                    move |this: &mut XoolFiles, event: &gpui::DragMoveEvent<MenuToolDrag>, _, cx| {
                        if event.bounds.contains(&event.event.position) {
                            let source = event.drag(cx).0;
                            this.hover_menu_tool(source, index, cx);
                        }
                    }))
                .on_drop(cx.listener(move |this: &mut XoolFiles, drag: &MenuToolDrag, _, cx| {
                    this.reorder_menu_tool(drag.0, index, cx);
                    cx.stop_propagation();
                }))
                .child(row))
        }));
    settings_surface(p).w_full().max_w(px(350.)).p(px(9.)).gap(px(6.))
        .child(search.text(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD), tr("Personalizar iconos de acceso rápido", "Customize quick access icons")))
        .child(div().text_size(px(11.)).text_color(p.muted)
            .child(tr("Arrastra desde el tirador para ordenar los iconos.", "Drag the handle to reorder the icons.")))
        .child(preview)
        .child(rows)
        .into_any_element()
}

fn context_menu_settings(
    search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>,
) -> AnyElement {
    div().flex().flex_col().gap(px(8.))
        .child(settings_section(search, tr("MENÚ CONTEXTUAL", "CONTEXT MENU"), p))
        .child(settings_surface(p).p(px(12.)).gap(px(8.))
            .child(search.text(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD), tr("Estilo del menú", "Menu style")))
            .child(div().flex().flex_wrap().gap(px(8.)).children(
                ContextMenuStyle::ALL.into_iter().enumerate().map(|(index, style)| {
                    let active = app.settings.context_menu_style == style;
                    let matched = search.matches(&[style.label()]);
                    div().id(("context-menu-style", index)).flex_1().min_w(px(136.))
                        .p(px(8.)).gap(px(7.)).flex().flex_col().rounded(px(8.))
                        .border_1().border_color(if active { p.accent } else { p.border_soft })
                        .bg(if matched { rgba(SETTINGS_HIGHLIGHT) } else if active { p.accent_faint } else { p.input })
                        .cursor_pointer().hover(|card| card.border_color(p.accent))
                        .child(div().text_size(px(11.5)).font_weight(FontWeight::SEMIBOLD)
                            .text_color(if active { p.accent } else { p.text }).child(style.label()))
                        .child(context_menu_preview(style, p))
                        .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                            this.settings.context_menu_style = style;
                            this.settings.save();
                            cx.notify();
                        }))
                }),
            )))
        .child(context_toolbar_settings(search, app, p, cx))
        .child(settings_surface(p).child(toggle_row(search,
            "shell-actions", tr("Acciones de Windows y de aplicaciones instaladas", "Windows and installed app actions"),
            tr("Incluye las opciones de Windows y de las aplicaciones instaladas.", "Include commands from Windows and installed apps."),
            app.settings.shell_menu_actions, p,
            cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                this.settings.shell_menu_actions = !this.settings.shell_menu_actions;
                this.settings.save();
                cx.notify();
            }),
        )))
        .into_any_element()
}

fn context_menu_preview(style: ContextMenuStyle, p: &Colors) -> AnyElement {
    let path = PathBuf::new();
    let tools = vec![
        MenuRow::item("\u{E8C6}", tr("Cortar", "Cut"), "", MenuAction::CutSelection),
        MenuRow::item("\u{E8C8}", tr("Copiar", "Copy"), "", MenuAction::CopySelection),
        MenuRow::item("\u{E77F}", tr("Pegar", "Paste"), "", MenuAction::Paste),
        MenuRow::item("\u{E71B}", tr("Copiar ruta", "Copy path"), "", MenuAction::CopyPath(path.clone())),
        MenuRow::item("\u{E70F}", tr("Renombrar", "Rename"), "", MenuAction::Rename(path.clone())),
        MenuRow::danger("\u{E74D}", tr("Eliminar", "Delete"), "", MenuAction::DeleteSelection),
        MenuRow::item("\u{E734}", tr("Añadir a favoritos", "Add to Favorites"), "", MenuAction::AddFavorite(path.clone())),
        MenuRow::item("\u{E946}", tr("Propiedades", "Properties"), "", MenuAction::Properties(path.clone())),
    ];
    let rows = vec![
        MenuRow::item("\u{E8E5}", tr("Abrir", "Open"), "", MenuAction::Open(path.clone())),
        MenuRow::item("\u{E8A7}", tr("Abrir con", "Open with"), "", MenuAction::OpenWith(path.clone())),
        MenuRow::item("\u{E756}", tr("Abrir terminal aquí", "Open terminal here"), "", MenuAction::OpenTerminal(path)),
        MenuRow::Separator,
        MenuRow::item("\u{E72D}", tr("Compartir", "Share"), "", MenuAction::ShellCommand(0)),
    ];
    let ContextMenuLayout { tools, rows, .. } = context_menu_layout(style, tools, rows);
    div().h(px(150.)).rounded(px(6.)).overflow_hidden().bg(p.panel)
        .border_1().border_color(p.border_soft).p(px(4.)).flex().flex_col()
        .when(!tools.is_empty(), |preview| {
            preview.child(div().h(px(22.)).flex_shrink_0().flex().items_center().gap(px(4.)).px(px(4.))
                .border_b_1().border_color(p.border_soft)
                .children(tools.into_iter().filter_map(|row| match row {
                    MenuRow::Item(entry) => Some(glyph(entry.icon, 10., if entry.danger { p.danger } else { p.muted })),
                    MenuRow::Separator => None,
                })))
        })
        .children(rows.into_iter().take(7).map(|row| match row {
            MenuRow::Item(entry) => div().flex().items_center().gap(px(6.)).px(px(4.)).h(px(18.)).flex_shrink_0()
                .child(glyph(entry.icon, 9., if entry.danger { p.danger } else { p.muted }))
                .child(div().text_size(px(10.5)).min_w(px(0.)).truncate().child(SharedString::from(entry.label)))
                .into_any_element(),
            MenuRow::Separator => div().h(px(1.)).mx(px(3.)).my(px(3.)).flex_shrink_0().bg(p.border_soft).into_any_element(),
        }))
        .into_any_element()
}

fn multimedia_settings(search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    const EXAMPLE: [f32; 32] = [
        0.32, 0.41, 0.48, 0.31, 0.21, 0.53, 0.82, 0.95, 0.73, 0.51, 0.64, 0.43, 0.31, 0.24, 0.54, 0.88,
        0.64, 0.72, 0.45, 0.32, 0.38, 0.72, 0.89, 0.97, 0.76, 0.61, 0.48, 0.27, 0.41, 0.60, 0.37, 0.21,
    ];
    div().flex().flex_col()
        .child(action_sound_settings(search, app, p, cx))
        .child(settings_section(search, tr("REPRODUCCIÓN", "PLAYBACK"), p))
        .child(settings_surface(p)
            .child(toggle_row(search, "media-autoplay", tr("Reproducir audio al seleccionar", "Play audio when selected"),
                tr("Inicia el audio al seleccionarlo con la vista previa abierta.", "Start playback when an audio file is selected with the preview open."), app.settings.preview_autoplay,
                p, cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.settings.preview_autoplay = !this.settings.preview_autoplay;
                    this.settings.save();
                    cx.notify();
                })))
            .child(settings_divider(p))
            .child(toggle_row(search, "video-autoplay", tr("Reproducir vídeo al seleccionar", "Play video when selected"),
                tr("Inicia el vídeo al seleccionarlo con la vista previa abierta.", "Start playback when a video file is selected with the preview open."), app.settings.video_autoplay,
                p, cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.settings.video_autoplay = !this.settings.video_autoplay;
                    this.settings.save();
                    cx.notify();
                }))))
        .child(settings_section(search, tr("VISUALIZACIÓN", "VISUALIZATION"), p))
        .child(settings_surface(p).p(px(12.)).gap(px(8.))
            .child(search.text(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD), tr("Forma de onda en la vista previa de audio", "Waveform in audio preview")))
            .child(div().flex().flex_wrap().gap(px(8.)).children(
                AudioVisualization::ALL.into_iter().enumerate().map(|(index, style)| {
                    let active = style == app.settings.audio_visualization;
                    let matched = search.matches(&[style.label()]);
                    div().id(("audio-visualization", index)).flex_1().min_w(px(116.))
                        .rounded(px(8.)).border_1()
                        .border_color(if active { p.accent } else { p.border_soft })
                        .bg(if matched { rgba(SETTINGS_HIGHLIGHT) } else if active { p.accent_faint } else { p.input })
                        .p(px(8.)).flex().flex_col().gap(px(7.)).cursor_pointer()
                        .hover(|card| card.border_color(p.accent))
                        .child(div().w_full().h(px(64.)).rounded(px(5.)).p(px(6.)).bg(p.panel)
                            .child(audio_waveform(EXAMPLE.to_vec(), style, 0.42, p)))
                        .child(div().text_size(px(12.)).text_color(if active { p.accent } else { p.text }).child(style.label()))
                        .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                            this.settings.audio_visualization = style;
                            this.settings.save();
                            cx.notify();
                        }))
                }),
            )))
        .into_any_element()
}

fn action_sound_settings(
    search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>,
) -> AnyElement {
    div().flex().flex_col().mb(px(10.))
        .child(settings_section(search, tr("SONIDOS DE ACCIONES", "ACTION SOUNDS"), p))
        .child(settings_surface(p)
            .child(toggle_row(search, "action-sounds", tr("Sonidos al hacer clic", "Click sounds"),
                tr("Reproduce el sonido asignado al seleccionar un archivo o abrir o cambiar de pestaña.", "Play the assigned sound when selecting a file or opening or switching tabs."),
                app.settings.action_sounds_enabled, p,
                cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.settings.action_sounds_enabled = !this.settings.action_sounds_enabled;
                    this.settings.save();
                    cx.notify();
                })))
            .child(settings_divider(p))
            .child(div().px(px(12.)).py(px(11.)).flex().flex_col().gap(px(8.))
                .child(search.text(div().text_size(px(12.)).text_color(p.muted), tr("Probar sonidos", "Preview sounds")))
                .child(div().flex().flex_wrap().gap(px(8.)).children(
                    ActionSound::PRESETS.into_iter().enumerate().map(|(index, sound)| {
                        let name = SharedString::from(sound.label().to_owned());
                        let matched = search.matches(&[sound.label()]);
                        div().id(("sound-preview", index)).flex_1().min_w(px(120.)).h(px(44.))
                            .px(px(8.)).rounded(px(7.)).border_1().border_color(p.border_soft)
                            .flex().items_center().gap(px(8.)).bg(p.input)
                            .when(matched, |card| card.bg(rgba(SETTINGS_HIGHLIGHT)))
                            .cursor_pointer().hover(|card| card.bg(p.card_hover).border_color(p.accent_soft))
                            .child(div().size(px(27.)).flex_shrink_0().rounded_full().bg(p.accent_faint)
                                .flex().items_center().justify_center()
                                .child(glyph("\u{E768}", 11., p.accent)))
                            .child(div().min_w(px(0.)).truncate().text_size(px(11.5))
                                .font_weight(FontWeight::MEDIUM).child(name))
                            .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                                this.preview_action_sound(&sound, cx);
                            }))
                    }),
                )))
            .child(settings_divider(p))
            .child(action_sound_row(SoundAction::SelectFile, search, app, p, cx))
            .child(settings_divider(p))
            .child(action_sound_row(SoundAction::SwitchTab, search, app, p, cx)))
        .into_any_element()
}

fn action_sound_row(
    action: SoundAction, search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>,
) -> Div {
    let label = action.label();
    let selected = app.action_sound(action);
    let [first, second, third] = ActionSound::PRESETS;
    let choices = [None, Some(first), Some(second), Some(third)];
    let labels = choices.map(|sound| sound.as_ref().map_or(tr("Sin sonido", "No sound"), ActionSound::label));
    let active = choices.iter().position(|sound| sound.as_ref() == selected).unwrap_or(usize::MAX);
    search.mark(div().px(px(12.)).py(px(9.)).flex().flex_wrap().items_center().justify_between().gap(px(8.)), &labels)
        .child(search.text(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD), label))
        .child(settings_segment_group(label, labels, active, None, p, cx,
            move |this, index, _, cx| this.set_action_sound(action, choices[index], cx)))
}

fn settings_file_storage(search: &SettingsSearch<'_>, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    let export = tr("Exportar configuración", "Export settings");
    let import = tr("Importar configuración", "Import settings");
    let open_folder = tr("Abrir carpeta", "Open folder");
    let restore = tr("Restaurar valores predeterminados", "Restore defaults");
    let restore_button = tr("Restaurar ajustes", "Reset settings");
    let backup = tr("Guarda una copia del archivo actual en la misma carpeta.", "Save a backup of the current settings file in the same folder.");
    let location = crate::settings::settings_path()
        .map(|path| path.display().to_string()).unwrap_or_else(|| tr("No disponible", "Unavailable").into());
    div().flex().flex_col().mt(px(10.))
        .child(settings_section(search, tr("ARCHIVO DE CONFIGURACIÓN", "SETTINGS FILE"), p))
        .child(settings_surface(p)
            .child(search.mark(div().px(px(12.)).py(px(10.)).flex().flex_col().gap(px(7.)), &["settings.json", export, import, open_folder])
                .child(div().flex().items_center().gap(px(7.))
                    .child(glyph("\u{E8A5}", 13., p.accent))
                    .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child("settings.json")))
                .child(div().text_size(px(11.)).text_color(p.muted).whitespace_nowrap().overflow_hidden().child(location))
                .child(div().flex().flex_wrap().gap(px(7.))
                    .child(secondary_button("storage-open-settings-folder", open_folder, p,
                        cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| this.open_settings_folder(window, cx))))
                    .child(secondary_button("storage-export-settings", export, p,
                        cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| this.export_preferences(window, cx))))
                    .child(secondary_button("storage-import-settings", import, p,
                        cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| this.import_preferences(window, cx))))))
            .child(settings_divider(p))
            .child(search.mark(div().px(px(12.)).py(px(10.)).flex().flex_wrap().items_center().justify_between().gap(px(7.)), &[restore, restore_button, backup])
                .child(div().flex().flex_col().gap(px(3.))
                    .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(restore))
                    .child(div().text_size(px(11.5)).text_color(p.muted).child(backup)))
                .child(secondary_button("storage-reset-settings", restore_button, p,
                    cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| this.reset_preferences(window, cx))))))
        .into_any_element()
}

fn integration_settings(search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    div().flex().flex_col().gap(px(9.))
        .child(settings_section(search, tr("INICIO Y EJECUCIÓN", "STARTUP AND BACKGROUND"), p))
        .child(settings_surface(p)
            .child(toggle_row(search, "windows-startup", tr("Abrir Xool Files al iniciar Windows", "Open Xool Files when Windows starts"),
                tr("Inicia la aplicación al entrar en tu sesión.", "Launch the app when you sign in."), app.windows_startup, p,
                cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| this.toggle_windows_startup(cx))))
            .child(settings_divider(p))
            .child(toggle_row(search, "background-on-close", tr("Seguir en segundo plano al cerrar la ventana", "Keep running in the background when the window closes"),
                tr("Mantiene Xool Files disponible en la bandeja o minimizado en la barra de tareas.", "Keep Xool Files available in the system tray or minimized on the taskbar."),
                app.settings.background_on_close, p, cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.settings.background_on_close = !this.settings.background_on_close;
                    this.settings.save();
                    cx.notify();
                })))
            .child(settings_divider(p))
            .child(toggle_row(search, "tray-icon", tr("Mostrar icono en la bandeja del sistema", "Show system tray icon"),
                tr("Desde el icono puedes volver a abrir Xool Files o salir de la aplicación.", "Use the icon to reopen or quit Xool Files."),
                app.settings.show_tray_icon, p, cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                    this.toggle_tray_icon(window, cx);
                }))))
        .child(settings_section(search, tr("CARPETAS Y UNIDADES", "FOLDERS AND DRIVES"), p))
        .child(settings_surface(p).child(toggle_row(search, "windows-folder-default", tr("Usar Xool Files como gestor de archivos predeterminado", "Use Xool Files as the default file manager"),
            tr("Abre carpetas y unidades con Xool Files para tu usuario de Windows.", "Open folders and drives with Xool Files for your Windows user account."),
            app.folder_default, p, cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| this.toggle_folder_default(cx)))))
        .when_some(app.integration_message.clone(), |panel, message| panel.child(
            div().text_size(px(12.)).text_color(p.danger).child(message)))
        .into_any_element()
}

fn search_index_settings(search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    let status = app.search_index.status();
    let section_title = tr("BÚSQUEDA EN EL EQUIPO", "SEARCH THIS PC");
    let global_title = tr("Buscar en todo el PC", "Search the entire PC");
    let rebuild_label = tr("Reindexar ahora", "Reindex now");
    let watch_title = tr("Indexación instantánea", "Instant indexing");
    let description = tr("Usa el atajo global o «Probar búsqueda». Puedes combinar palabras, usar * y ? o escribir parte de una ruta con \\.", "Use the global shortcut or “Try search”. You can combine words, use * and ?, or enter part of a path with \\.");
    let watch_description = tr("Detecta archivos nuevos, eliminados y renombrados mientras Xool Files está abierto. La primera indexación recorre las unidades.", "Detects new, deleted, and renamed files while Xool Files is open. The first indexing scan checks the drives.");
    let status_text = if status.building {
        tr("Escaneando unidades…", "Scanning drives…")
    } else if status.ready {
        tr("Índice listo", "Index ready")
    } else {
        tr("Se crea con la primera búsqueda", "Built with the first search")
    };
    let live_status = match (app.settings.search_live_index, status.watched_drives) {
        (false, _) => tr("Desactivada", "Off").to_string(),
        (true, 0) => tr("Sin unidades bajo seguimiento", "No drives are being monitored").to_string(),
        (true, count) => crate::localized!(format, "En seguimiento: {count} unidad(es)", "Tracking {count} drive(s)"),
    };
    let live_button = if app.settings.search_live_index { tr("Desactivar", "Disable") } else { tr("Activar", "Enable") };
    div().flex().flex_col()
        .child(settings_section(search, section_title, p))
        .child(settings_surface(p)
            .child(search.mark(div().px(px(12.)).py(px(10.)).flex().flex_wrap().gap(px(10.))
                    .items_center().justify_between(),
                    &[global_title, rebuild_label, description, tr("Índice", "Index")])
                .child(search_settings_description(global_title, description, p))
                .child(div().flex().flex_col().items_end().gap(px(5.))
                    .child(div().text_size(px(11.5)).text_color(p.muted).child(status_text))
                    .when(!status.building, |actions| actions.child(secondary_button("index-rebuild", rebuild_label, p,
                        cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| this.reindex_search(cx)))))
                    .when(status.building, |actions| actions.child(div().px(px(9.)).py(px(6.)).text_size(px(12.))
                        .text_color(p.dim).child(rebuild_label)))))
            .child(settings_divider(p))
            .child(search.mark(div().px(px(12.)).py(px(10.)).flex().flex_wrap().gap(px(10.))
                    .items_center().justify_between(),
                    &[watch_title, live_button, watch_description])
                .child(search_settings_description(watch_title, watch_description, p))
                .child(div().flex().flex_col().items_end().gap(px(5.))
                    .child(div().text_size(px(11.5)).text_color(p.muted).child(live_status))
                    .child(secondary_button("index-live-toggle", live_button, p,
                        cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| this.toggle_live_search_index(cx))))))
            .when_some(status.error, |panel, error| panel.child(
                div().px(px(12.)).pb(px(8.)).text_size(px(11.5)).text_color(p.danger).child(error))))
        .into_any_element()
}

fn search_settings_description(title: &'static str, description: &'static str, p: &Colors) -> Div {
    div().flex_1().min_w(px(170.)).flex().flex_col().gap(px(4.))
        .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(title))
        .child(div().text_size(px(12.)).text_color(p.muted).child(description))
}

fn search_filter_settings(search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    div().w_full().flex().flex_col()
        .child(settings_section(search, tr("COINCIDENCIA DE TEXTO", "TEXT MATCHING"), p))
        .child(settings_surface(p)
            .child(toggle_row(search, "search-ignore-accents", tr("Ignorar tildes", "Ignore accents"),
                tr("Al buscar por nombre, musica también encuentra Música. Funciona en la carpeta y en el buscador global.", "A search for musica also finds Música. Works in folder and global searches."),
                app.settings.search_ignore_accents, p,
                cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| this.toggle_search_ignore_accents(cx)))))
        .child(div().h(px(10.)).flex_shrink_0())
        .child(settings_section(search, tr("FILTROS DE CARPETA", "FOLDER FILTERS"), p))
        .child(settings_surface(p).w_full().p(px(12.)).gap(px(5.))
            .child(search.text(div().whitespace_nowrap().text_size(px(12.)).text_color(p.muted),
                tr("Combina filtros. SUB incluye subcarpetas.", "Combine filters. SUB includes subfolders.")))
            .child(search.text(div().whitespace_nowrap().font_family(MONO_FONT)
                .text_size(px(11.5)).text_color(p.accent),
                "ext:pdf,txt · tipo:imagen · -borrador · ~informe")))
        .into_any_element()
}

fn quick_search_settings(search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    let try_search = tr("Probar búsqueda", "Try search");
    let availability = tr("Disponible mientras Xool Files está abierto.", "Available while Xool Files is open.");
    let keys = app.settings.quick_search_shortcut.as_deref();
    let preset = crate::shortcuts::QUICK_SEARCH_PRESETS.iter().position(|candidate| *candidate == keys);
    let recording = app.recording_shortcut == Some(crate::shortcuts::QUICK_SEARCH_RECORDING);
    let custom_keys = keys.filter(|_| preset.is_none());
    let manual_label = if recording {
        tr("Pulsa las teclas…", "Press the keys…").to_string()
    } else {
        custom_keys.map(crate::shortcuts::display).unwrap_or_else(|| tr("Personalizar…", "Customize…").to_string())
    };
    let active = recording || custom_keys.is_some();
    let hint = if recording { tr("Esc para cancelar", "Esc to cancel") } else { tr("Personalizar atajo global", "Customize global shortcut") };
    let presets = [tr("Desactivado", "Off"), tr("Ctrl+Espacio", "Ctrl+Space"), tr("Mayús+Espacio", "Shift+Space"), "Ctrl+X"];
    let manual_index = presets.len();
    let selector = settings_segment_group("quick-search-hotkey", presets,
        if active { manual_index } else { preset.unwrap_or(usize::MAX) },
        Some((manual_label, hint)), p, cx, move |this, index, window, cx| {
            if index == manual_index {
                this.recording_shortcut = (!recording).then_some(crate::shortcuts::QUICK_SEARCH_RECORDING);
                this.quick_search_message = None;
                this.focus_root(window);
                cx.notify();
            } else {
                this.set_quick_key(crate::shortcuts::QUICK_SEARCH_PRESETS[index], window, cx);
            }
        });
    div().flex().flex_col()
        .child(settings_section(search, tr("BÚSQUEDA RÁPIDA", "QUICK SEARCH"), p))
        .child(settings_surface(p)
            .child(search.mark(div().px(px(14.)).py(px(10.)).flex().flex_wrap().gap(px(8.))
                    .items_center().justify_between(),
                    &[tr("Atajo global", "Global shortcut"), tr("Personalizar atajo global", "Customize global shortcut"), tr("Atajo manual", "Custom shortcut")])
                .when(search.matches(&presets), |row| row.bg(rgba(SETTINGS_HIGHLIGHT)))
                .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(tr("Atajo global", "Global shortcut")))
                .child(selector))
            .when(keys == Some("ctrl-x"), |panel| panel.child(
                div().px(px(12.)).pb(px(8.)).text_size(px(11.)).text_color(p.muted)
                    .child(tr("Ctrl+X abrirá la búsqueda y dejará de cortar mientras Xool Files esté abierto.", "Ctrl+X will open search and stop cutting while Xool Files is open."))))
            .child(settings_divider(p))
            .child(search.mark(div().px(px(12.)).py(px(8.)).flex().flex_wrap().items_center().gap(px(10.)), &[try_search, availability])
                .child(secondary_button("try-quick-search", try_search, p,
                    cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| this.open_quick_search(window, cx))))
                .child(div().text_size(px(12.)).text_color(p.muted).child(availability))))
        .when_some(app.quick_search_message.clone(), |panel, message| panel.child(
            div().mt(px(7.)).text_size(px(11.)).text_color(p.danger).child(message)))
        .into_any_element()
}

pub(crate) fn quick_search_view(app: &QuickSearch, p: &Colors, cx: &mut Context<QuickSearch>) -> AnyElement {
    use crate::quick_search::{CORNER_RADIUS, FOOTER_HEIGHT, FRAME_INSET, HEADER_HEIGHT, LIST_PADDING, ROW_GAP, ROW_HEIGHT, SHADOW_INSET};

    let input = app.input.read(cx).value();
    let mut list = div().id("quick-results").min_h(px(0.)).flex_1().track_scroll(&app.scroll)
        .overflow_y_scroll().p(px(LIST_PADDING)).flex().flex_col().gap(px(ROW_GAP));
    for (index, result) in app.results.iter().enumerate() {
        let path = &result.path;
        let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned();
        let selected = index == app.selection;
        list = list.child(div().id(("quick-result", index)).h(px(ROW_HEIGHT)).flex_shrink_0()
            .pr(px(5.)).rounded(px(6.)).flex().items_center().gap(px(5.))
            .when(selected, |row| row.bg(p.selection))
            .when(!selected, |row| row.hover(|style| style.bg(p.card_hover)))
            .child(div().id("quick-open").h_full().min_w(px(0.)).flex_1().pl(px(9.))
                .flex().items_center().gap(px(10.)).cursor_pointer()
                .child(div().size(px(28.)).flex_shrink_0().rounded(px(4.)).overflow_hidden()
                    .flex().items_center().justify_center()
                    .child(match &result.visual {
                        Some(image) => img(image.clone()).w_full().h_full().object_fit(ObjectFit::Contain).into_any_element(),
                        None => glyph(if result.is_dir { "\u{E8B7}" } else { "\u{E8A5}" }, 15., p.accent).into_any_element(),
                    }))
                .child(div().min_w(px(0.)).flex_1().flex().flex_col()
                    .child(div().text_size(px(12.)).truncate().child(name))
                    .child(div().text_size(px(10.5)).text_color(p.muted).truncate().child(path.display().to_string())))
                .on_click(cx.listener(move |this: &mut QuickSearch, _: &ClickEvent, window, cx| {
                    this.open_result(index, window, cx);
                })))
            .child(quick_result_actions(result, &app.owner, p, !app.searching)));
    }
    let status = if app.searching {
        tr("Buscando…", "Searching…")
    } else if app.indexing {
        tr("Creando el índice del equipo…", "Building the computer index…")
    } else if !input.trim().is_empty() && app.results.is_empty() {
        tr("No se encontraron resultados.", "No results found.")
    } else {
        tr("↑ ↓ Elegir     Enter Abrir     Esc Cerrar", "↑ ↓ Select     Enter Open     Esc Close")
    };
    let toast = app.toast(cx);
    let status_color = match toast.as_ref().map(|toast| toast.tone) {
        Some(ToastTone::Success) => p.success,
        Some(ToastTone::Error) => p.danger,
        Some(ToastTone::Info) => p.accent,
        None => p.muted,
    };
    let panel = div().id("quick-search").size_full().rounded(px(CORNER_RADIUS - FRAME_INSET))
        .text_color(p.text).font_family(UI_FONT)
        .border_1().border_color(p.border).overflow_hidden().flex().flex_col()
        .capture_key_down(cx.listener(QuickSearch::on_key_down))
        .child(div().px(px(16.)).h(px(HEADER_HEIGHT)).flex_shrink_0().flex().items_center().gap(px(10.))
            .child(glyph("\u{E721}", 18., p.accent))
            .child(div().min_w(px(0.)).flex_1()
                .child(Input::new(&app.input).appearance(false).bordered(false).text_size(px(18.))))
            .child(icon_button("close-quick-search", "\u{E711}", 12., p, false, true,
                cx.listener(|this: &mut QuickSearch, _: &ClickEvent, window, _| this.close(window)))
                .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new(tr("Cerrar búsqueda", "Close search")).build(window, cx))))
        .when(!app.results.is_empty(), |panel| panel.child(list))
        .child(div().h(px(FOOTER_HEIGHT)).flex_shrink_0().flex().items_center().px(px(12.))
            .border_t_1().border_color(p.border_soft)
            .text_size(px(10.5)).text_color(status_color).truncate()
            .child(toast.map_or_else(|| status.to_owned(), |toast| toast.text)));
    div().size_full().p(px(SHADOW_INSET)).child(
        div().id("quick-search-card").size_full().rounded(px(CORNER_RADIUS)).bg(p.panel).p(px(FRAME_INSET))
            .shadow(vec![gpui::BoxShadow {
                color: rgba(0x00000046).into(),
                offset: gpui::point(px(0.), px(0.)),
                blur_radius: px(8.),
                spread_radius: px(0.),
            }])
            .on_mouse_down_out(cx.listener(|this: &mut QuickSearch, _: &MouseDownEvent, window, _| {
                this.close(window);
            }))
            .child(panel)
    ).into_any_element()
}

fn quick_result_actions(result: &QuickResult, owner: &gpui::WeakEntity<XoolFiles>, p: &Colors, enabled: bool) -> Div {
    #[derive(Clone, Copy)]
    enum Action { Copy, Paste, Delete, CopyPath, Properties }

    let paste_hint = if result.is_dir { tr("Pegar en esta carpeta", "Paste into this folder") } else { tr("Pegar en la carpeta del archivo", "Paste into the file’s folder") };
    let actions = [
        ("quick-copy", "\u{E8C8}", tr("Copiar", "Copy"), Action::Copy),
        ("quick-paste", "\u{E77F}", paste_hint, Action::Paste),
        ("quick-delete", "\u{E74D}", tr("Eliminar", "Delete"), Action::Delete),
        ("quick-path", "\u{E71B}", tr("Copiar ruta", "Copy path"), Action::CopyPath),
        ("quick-properties", "\u{E946}", tr("Propiedades", "Properties"), Action::Properties),
    ];
    div().flex().flex_shrink_0().items_center().children(actions.into_iter().map(|(id, icon, hint, action)| {
        let path = result.path.clone();
        let is_dir = result.is_dir;
        let owner = owner.clone();
        icon_button(id, icon, 13., p, false, enabled,
            move |_: &ClickEvent, window, cx| {
                cx.stop_propagation();
                let _ = owner.update(cx, |this, cx| match action {
                    Action::Copy => this.copy_paths(vec![path.clone()], false, window, cx),
                    Action::Paste => {
                        let folder = if is_dir { Some(path.as_path()) } else { path.parent() };
                        if let Some(folder) = folder {
                            this.paste_into(folder.to_path_buf(), window, cx);
                        }
                    }
                    Action::Delete => this.perform_delete(vec![path.clone()], window, cx),
                    Action::CopyPath => this.copy_path(&path, window, cx),
                    Action::Properties => platform::properties(window, &path),
                });
            })
            .size(px(26.)).rounded(px(5.))
            .when(enabled && matches!(action, Action::Delete), |button| button.hover(|style| {
                style.bg(p.card_hover).text_color(p.danger)
            }))
            .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(hint).build(window, cx))
    }))
}

struct FileDetailsTooltip {
    name: Option<String>,
    rows: Vec<(&'static str, String)>,
    colors: Colors,
    _metadata: gpui::Task<()>,
}

impl gpui::Render for FileDetailsTooltip {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let p = &self.colors;
        let mut name_lines = Vec::new();
        if let Some(name) = &self.name {
            let mut remaining = name.clone();
            while !remaining.is_empty() {
                let (line, tail) = split_grid_name(&remaining, 260., 12., window);
                name_lines.push(line);
                remaining = tail;
            }
        }
        div().w(px(288.)).px(px(11.)).py(px(8.)).bg(p.panel).text_color(p.text).rounded(px(7.))
            .border_1().border_color(p.border).shadow_lg().flex().flex_col().gap(px(4.))
            .when(!name_lines.is_empty(), |tip| tip.child(div().flex().flex_col().pb(px(2.))
                .children(name_lines.into_iter().map(|line| {
                    div().h(px(17.)).flex_shrink_0().text_size(px(12.)).line_height(px(17.))
                        .font_weight(FontWeight::SEMIBOLD).truncate().child(line)
                }))))
            .children(self.rows.iter().map(|(label, value)| {
                div().h(px(17.)).flex_shrink_0().flex().items_center().gap(px(7.))
                    .text_size(px(11.)).line_height(px(17.))
                    .child(div().w(px(94.)).flex_shrink_0().text_color(p.muted).child(*label))
                    .child(div().flex_1().min_w(px(0.)).truncate().child(value.clone()))
            }))
    }
}

fn file_details(entry: &Entry, colors: Colors, fields: crate::settings::HoverFields)
    -> impl Fn(&mut Window, &mut gpui::App) -> gpui::AnyView + 'static
{
    let path = entry.path.clone();
    let name = fields.name.then(|| entry.name.clone());
    let kind = entry.kind;
    let mut rows = Vec::new();
    if fields.kind { rows.push((tr("Tipo", "Type"), entry.type_label())); }
    if fields.dimensions {
        rows.push((tr("Dimensiones", "Dimensions"), "…".into()));
    }
    if fields.created { rows.push((tr("Creación", "Created"), "…".into())); }
    if fields.modified { rows.push((tr("Modificación", "Modified"), entry.modified_label())); }
    if fields.accessed { rows.push((tr("Último acceso", "Last accessed"), "…".into())); }
    if fields.size { rows.push((tr("Tamaño", "Size"), if entry.is_dir { "—".into() } else { entry.size_label() })); }

    move |_, cx| {
        let path = path.clone();
        cx.new(|cx: &mut Context<FileDetailsTooltip>| {
            let task = cx.spawn(async move |this, cx| {
                let (dimensions, created, accessed) = cx.background_executor().spawn(async move {
                    let metadata = (fields.created || fields.accessed).then(|| std::fs::metadata(&path).ok()).flatten();
                    let created = metadata.as_ref().and_then(|m| m.created().ok());
                    let accessed = metadata.as_ref().and_then(|m| m.accessed().ok());
                    let dimensions = if fields.dimensions {
                        match kind {
                            FileKind::Image => image::image_dimensions(&path).ok(),
                            FileKind::Video => crate::media::video_dimensions(&path),
                            _ => None,
                        }
                    } else { None };
                    (dimensions, created, accessed)
                }).await;
                let _ = this.update(cx, |tip, cx| {
                    for (label, value) in &mut tip.rows {
                        let updated = match *label {
                            label if label == tr("Dimensiones", "Dimensions") => dimensions.map(|(w, h)| crate::localized!(format, "{w} × {h} píxeles", "{w} × {h} pixels")),
                            label if label == tr("Creación", "Created") => created.map(fs_model::format_date),
                            label if label == tr("Último acceso", "Last accessed") => accessed.map(fs_model::format_date),
                            _ => continue,
                        };
                        *value = updated.unwrap_or_else(|| "—".into());
                    }
                    cx.notify();
                });
            });
            FileDetailsTooltip { name: name.clone(), rows: rows.clone(), colors, _metadata: task }
        }).into()
    }
}

fn hover_field_options(search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    type Field = fn(&mut crate::settings::HoverFields) -> &mut bool;
    let values = app.settings.hover_fields;
    let options: [(&str, bool, Field); 7] = [
        (tr("Nombre", "Name"), values.name, |f| &mut f.name),
        (tr("Tipo de elemento", "Item type"), values.kind, |f| &mut f.kind),
        (tr("Dimensiones", "Dimensions"), values.dimensions, |f| &mut f.dimensions),
        (tr("Fecha de creación", "Date created"), values.created, |f| &mut f.created),
        (tr("Fecha de modificación", "Date modified"), values.modified, |f| &mut f.modified),
        (tr("Último acceso", "Last accessed"), values.accessed, |f| &mut f.accessed),
        (tr("Tamaño", "Size"), values.size, |f| &mut f.size),
    ];
    div().pl(px(24.)).pr(px(10.)).py(px(5.)).flex().flex_col()
        .children(options.into_iter().enumerate().map(|(index, (label, active, field))| {
            search.mark(div().id(("hover-field", index)), &[label]).h(px(28.)).px(px(3.)).rounded(px(4.))
                .flex().items_center().justify_between().cursor_pointer().hover(|row| row.bg(p.card_hover))
                .child(div().text_size(px(12.)).text_color(p.muted).child(label))
                .child(switch(active, p))
                .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    let value = field(&mut this.settings.hover_fields);
                    *value = !*value;
                    this.settings.save();
                    cx.notify();
                }))
        }))
        .into_any_element()
}

fn sidebar_drive_branch(path: &std::path::Path, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    let expanded = app.sidebar_drive_nodes.contains_key(path);
    let path = path.to_path_buf();
    div().id(SharedString::from(path.display().to_string())).w(px(17.)).h(px(26.)).flex_shrink_0()
        .flex().items_center().justify_center().cursor_pointer().rounded(px(4.))
        .hover(|row| row.bg(p.card_hover))
        .child(glyph(if expanded { "\u{E70D}" } else { "\u{E76C}" }, 9., p.muted))
        .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| this.toggle_sidebar_drive_node(path.clone(), cx)))
        .into_any_element()
}

fn sidebar_settings_preview(app: &XoolFiles, p: &Colors) -> AnyElement {
    let only = app.settings.sidebar_icons_only;
    div().flex().flex_col().gap(px(5.))
        .child(settings_section_label(div(), tr("Vista previa", "Preview"), p))
        .child(settings_surface(p).w(px(if only { 51. } else { 212. })).p(px(5.))
            .children(app.settings.sidebar_groups.iter().map(|group| {
                let icon = match group {
                    SidebarGroup::Favorites => "\u{E734}", SidebarGroup::Quick => "\u{E8B7}",
                    SidebarGroup::Drives => "\u{E7F1}", SidebarGroup::Recent => "\u{E81C}",
                    SidebarGroup::Custom(_) => "\u{E8B7}",
                };
                div().h(px(app.settings.sidebar_row_height())).px(px(6.)).flex().items_center().gap(px(6.))
                    .when(only, |row| row.justify_center())
                    .child(glyph(icon, 12., p.accent))
                    .when(!only, |row| row.child(div().text_size(px(12.)).child(app.settings.sidebar_group_name(*group).to_owned())))
            })))
        .into_any_element()
}

fn custom_sidebar_group_settings(
    group: &crate::settings::CustomSidebarGroup, search: &SettingsSearch<'_>, p: &Colors,
    cx: &mut Context<XoolFiles>,
) -> gpui::Stateful<Div> {
    let id = group.id;
    div().id(("manage-sidebar-group", id)).flex().flex_col().gap(px(5.))
        .px(px(8.)).py(px(6.)).rounded(px(7.)).bg(p.panel)
        .child(div().flex().flex_wrap().items_center().justify_between().gap(px(6.))
            .child(search.mark(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD), &[&group.name])
                .child(group.name.clone()))
            .child(div().flex().flex_wrap().gap(px(5.))
                .child(secondary_button("rename-group", tr("Cambiar nombre", "Rename"), p,
                    cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, window, cx| this.name_sidebar_group(id, window, cx))))
                .child(secondary_button("add-group-folders", tr("Añadir carpetas", "Add folders"), p,
                    cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| this.add_sidebar_group_folders(id, cx))))))
        .when(group.folders.is_empty(), |card| card.child(sidebar_group_hint(tr("Añade carpetas aquí o arrástralas al grupo en la barra lateral.", "Add folders here or drag them into the group in the sidebar."), p)))
        .children(group.folders.iter().enumerate().map(|(index, path)| {
            let folder = path.clone();
            let label = path.display().to_string();
            div().flex().items_center().gap(px(7.))
                .child(glyph("\u{E8B7}", 12., p.accent))
                .child(search.mark(div().flex_1().min_w(px(0.)).text_size(px(11.5)).text_color(p.muted)
                    .whitespace_nowrap().overflow_hidden(), &[&label]).child(label))
                .child(icon_button(("remove-group-folder", index), "icons/tab-close.svg", 12., p, false, true,
                    cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                        if let Some(group) = this.settings.custom_sidebar_groups.iter_mut().find(|group| group.id == id) {
                            group.folders.retain(|path| path != &folder);
                            this.settings.save();
                            cx.notify();
                        }
                    }))
                    .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new(tr("Quitar acceso del grupo", "Remove shortcut from group")).build(window, cx)))
        }))
}

fn sidebar_preferences(search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    let settings = &app.settings;
    let mut groups = settings_surface(p).p(px(10.)).gap(px(4.))
        .child(search.text(div().pb(px(8.)).text_size(px(14.)).font_weight(FontWeight::SEMIBOLD), tr("Grupos de la barra lateral", "Sidebar groups")));
    for (index, group) in app.sidebar_group_order().into_iter().enumerate() {
        let label = settings.sidebar_group_name(group);
        let drag_label = label.to_owned();
        let remove_label = crate::localized!(format, "Quitar {label}", "Remove {label}");
        let drag_colors = *p;
        let dragged = app.sidebar_group_target.is_some_and(|(item, _)| item == group);
        let offset = app.sidebar_group_offset(group);
        groups = groups.child(div().id(("sidebar-group-slot", index)).h(px(32.)).flex_shrink_0()
            .on_drag_move::<SidebarGroup>(cx.listener(
                move |this: &mut XoolFiles, event: &gpui::DragMoveEvent<SidebarGroup>, _, cx| {
                    if event.bounds.contains(&event.event.position) {
                        let target = Some((*event.drag(cx), index));
                        if this.sidebar_group_target != target {
                            this.preview_sidebar_group(target);
                            cx.notify();
                        }
                    }
                }))
            .on_drop(cx.listener(move |this: &mut XoolFiles, group: &SidebarGroup, _, cx| {
                this.commit_sidebar_group(*group, index, cx);
            }))
            .child(div().id(group.heading_id()).h_full().px(px(8.))
                .relative().top(px(offset)).flex().items_center().gap(px(8.)).rounded(px(5.))
                .bg(if dragged { p.selection } else { p.input })
                .opacity(if dragged { 0.50 } else { 1.0 })
                .on_drag(group, move |_, _, _, cx| {
                    cx.new(|_| DragGhost {
                        label: drag_label.clone().into(), count: 1, colors: drag_colors, icon: "\u{E700}",
                    })
                })
            .child(glyph("\u{E700}", 11., p.muted))
            .child(search.mark(div().flex_1().min_w(px(0.)).whitespace_nowrap().overflow_hidden()
                .text_size(px(13.)).font_weight(FontWeight::SEMIBOLD), &[label]).child(label.to_owned()))
            .child(div().id(("remove-sidebar-group", index)).size(px(27.)).rounded(px(5.))
                .flex().items_center().justify_center().cursor_pointer()
                .hover(|button| button.bg(p.danger_soft))
                .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(remove_label.clone()).build(window, cx))
                .child(glyph("\u{E74D}", 12., p.muted))
                .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.settings.sidebar_groups.retain(|item| *item != group);
                    if let SidebarGroup::Custom(id) = group {
                        this.settings.custom_sidebar_groups.retain(|custom| custom.id != id);
                    }
                    this.settings.save();
                    cx.notify();
                    cx.stop_propagation();
                })))));
    }
    let missing = SidebarGroup::ALL.into_iter().filter(|group| !settings.sidebar_groups.contains(group));
    let missing: Vec<_> = missing.collect();
    if !missing.is_empty() {
        groups = groups.child(settings_divider(p).mt(px(5.)))
            .child(div().pt(px(4.))
            .flex().flex_wrap().gap(px(5.)).children(missing.into_iter().map(|group| {
        search.mark(div(), &[group.label()]).child(secondary_button(group.label(), group.label(), p,
            cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                if !this.settings.sidebar_groups.contains(&group) {
                    this.settings.sidebar_groups.push(group);
                    if group == SidebarGroup::Recent { this.load_recent_icons(cx); }
                    this.settings.save();
                    cx.notify();
                }
            })))
        })));
    }
    groups = groups.child(settings_divider(p).mt(px(5.)))
        .child(search.mark(div().flex().py(px(4.)), &[tr("Crear grupo", "Create group"), tr("Grupo personalizado", "Custom group")])
            .map(|row| {
                if app.creating_sidebar_group {
                    row.flex_wrap().items_center().gap(px(6.))
                        .child(div().flex_1().min_w(px(140.)).rounded(px(8.))
                            .bg(p.input).border_1().border_color(p.border)
                            .child(Input::new(&app.dialog_input).appearance(false).bordered(false)
                                .h(px(31.)).text_size(px(13.))))
                        .child(secondary_button("cancel-custom-sidebar-group", tr("Cancelar", "Cancel"), p,
                            cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| this.cancel_sidebar_group_creation(window, cx))))
                        .child(primary_button("save-custom-sidebar-group", tr("Crear grupo", "Create group"), p,
                            cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| this.create_sidebar_group(window, cx))))
                } else {
                    row.child(secondary_button("create-custom-sidebar-group", tr("Crear grupo", "Create group"), p,
                        cx.listener(|this: &mut XoolFiles, _: &ClickEvent, window, cx| this.begin_sidebar_group_creation(window, cx))))
                }
            }))
        .children(settings.sidebar_groups.iter().filter_map(|group| match group {
            SidebarGroup::Custom(id) => settings.custom_sidebar_group(*id),
            _ => None,
        }).map(|group| custom_sidebar_group_settings(group, search, p, cx)));

    div().flex().flex_col().gap(px(10.))
        .child(sidebar_settings_preview(app, p))
        .child(settings_section(search, "GRUPOS", p))
        .child(groups
            .child(settings_divider(p).mt(px(5.)))
            .child(toggle_row(search, "reorder-sidebar-favorites", tr("Reordenar favoritos al arrastrar", "Reorder favorites by dragging"),
                tr("Arrastra un favorito hacia arriba o abajo en la barra lateral.", "Drag a favorite up or down in the sidebar."), settings.reorder_sidebar_favorites, p,
                cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.settings.reorder_sidebar_favorites = !this.settings.reorder_sidebar_favorites;
                    this.settings.save();
                    cx.notify();
                }))))
        .child(settings_section(search, tr("DISEÑO", "LAYOUT"), p))
        .child(settings_surface(p)
            .child(toggle_row(search, "sidebar-visible", tr("Mostrar barra lateral", "Show sidebar"), tr("Muestra los grupos seleccionados.", "Show the selected groups."),
                settings.sidebar_visible, p, cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| this.toggle_sidebar(cx))))
            .child(settings_divider(p))
            .child(settings_choices(search, "sidebar-side", (tr("Posición", "Position"), None), [tr("Izquierda", "Left"), tr("Derecha", "Right")], settings.sidebar_right,
                p, cx, |this, right| this.settings.sidebar_right = right))
            .child(settings_divider(p))
            .child(toggle_row(search, "sidebar-icons-only", tr("Solo iconos", "Icons only"), tr("Muestra los nombres al pasar el puntero.", "Show names when hovering over icons."),
                settings.sidebar_icons_only, p, cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.settings.sidebar_icons_only = !this.settings.sidebar_icons_only; this.settings.save(); cx.notify();
                })))
            .child(settings_divider(p))
            .child(settings_segments(search, "sidebar-row-size", tr("Altura de las filas", "Row height"), [tr("Compacta", "Compact"), "Normal", tr("Cómoda", "Comfortable")],
                usize::from(settings.sidebar_row_size.min(2)), p, cx, |this, n, _, cx| {
                    this.settings.sidebar_row_size = n as u8; this.settings.save(); cx.notify();
                })))
        .child(settings_section(search, tr("UNIDADES Y RECIENTES", "DRIVES AND RECENT FILES"), p))
        .child(settings_surface(p)
            .child(toggle_row(search, "sidebar-drive-tree", tr("Discos en árbol", "Drive tree"), tr("Permite desplegar las carpetas dentro de una unidad.", "Expand folders within a drive."),
                settings.sidebar_drive_tree, p, cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.settings.sidebar_drive_tree = !this.settings.sidebar_drive_tree;
                    this.sidebar_drive_nodes.clear(); this.settings.save(); cx.notify();
                })))
            .child(settings_divider(p))
            .child(settings_choices(search, "sidebar-drive-usage", (tr("Uso de los discos", "Drive usage"), None), [tr("Ninguno", "None"), tr("Abajo", "Bottom")], !settings.hide_drive_usage,
                p, cx, |this, below| this.settings.hide_drive_usage = !below))
            .child(settings_divider(p))
            .child(settings_segments(search, "sidebar-drive-names", tr("Nombre de los discos", "Drive names"), [tr("Disco (C:)", "Drive (C:)"), tr("C: Disco", "C: Drive"), "C:"],
                usize::from(settings.drive_name_style.min(2)), p, cx, |this, n, _, cx| {
                    this.settings.drive_name_style = n as u8; this.settings.save(); cx.notify();
                }))
            .child(settings_divider(p))
            .child(settings_segments(search, "sidebar-recent-count", tr("Límite de archivos recientes", "Recent files limit"), ["5", "10", "20", "50"],
                match settings.sidebar_recent_limit { 5 => 0, 20 => 2, 50 => 3, _ => 1 }, p, cx, |this, n, _, cx| {
                    this.settings.sidebar_recent_limit = [5, 10, 20, 50][n]; this.settings.save(); cx.notify();
                })))
        .into_any_element()
}

fn settings_heading(
    search: &SettingsSearch<'_>, title: &'static str, description: &'static str, about: bool, p: &Colors,
) -> Div {
    div().flex().items_center().gap(px(12.)).pb(px(12.))
        .when(about, |heading| heading.child(
            img("icons/xool-files.png").size(px(54.)).flex_shrink_0().object_fit(ObjectFit::Contain)))
        .child(div().min_w(px(0.)).flex().flex_col().gap(px(3.))
            .child(search.mark(div(), &[title]).text_size(px(24.))
                .font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(title))
            .when(about, |heading| heading.child(
                search.text(div().text_size(px(12.)).text_color(p.muted),
                    tr(concat!("Xool Files · Versión ", env!("CARGO_PKG_VERSION")), concat!("Xool Files · Version ", env!("CARGO_PKG_VERSION"))))))
            .child(search.mark(div(), &[description]).min_w(px(0.)).overflow_hidden()
                .whitespace_nowrap().text_size(px(12.)).text_color(p.muted).child(description)))
}

fn settings_row_text(title: &'static str, description: Option<&'static str>, p: &Colors) -> Div {
    div().min_w(px(0.)).flex().flex_col().gap(px(2.))
        .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(title))
        .when_some(description, |row, description| row.child(
            div().text_size(px(11.5)).text_color(p.muted).child(description)))
}

fn settings_choices(search: &SettingsSearch<'_>, 
    id: &'static str, text: (&'static str, Option<&'static str>), options: [&'static str; 2], second: bool,
    p: &Colors, cx: &mut Context<XoolFiles>, update: fn(&mut XoolFiles, bool),
) -> impl Styled + IntoElement {
    let (title, description) = text;
    let matched = search.matches(&[title, description.unwrap_or_default()]) || search.matches(&options);
    div().p(px(12.)).flex_shrink_0().flex().flex_wrap().items_center().justify_between().gap(px(8.))
        .when(matched, |row| row.bg(rgba(SETTINGS_HIGHLIGHT)).rounded(px(5.)))
        .child(settings_row_text(title, description, p))
        .child(settings_segment_group(id, options, usize::from(second), None, p, cx,
            move |this, index, _, cx| {
                update(this, index == 1);
                this.settings.save();
                cx.notify();
            }))
}

fn settings_layout_preview(app: &XoolFiles, p: &Colors) -> AnyElement {
    let prefs = &app.settings;
    let line = |width, color| div().w(px(width)).h(px(3.)).rounded_full().bg(color);
    let panel = |label, preview| {
        div().w(gpui::relative(0.24)).h_full().min_w(px(0.)).p(px(7.)).bg(p.panel_soft)
            .border_x_1().border_color(p.border_soft).flex().flex_col().gap(px(6.))
            .child(div().text_size(px(9.)).truncate().text_color(p.muted).child(label))
            .when(preview, |panel| panel.child(div().h(px(31.)).rounded(px(4.)).bg(p.accent_faint)))
            .children((0..4).map(|i| line(14. + (i % 3) as f32 * 7., if i == 0 { p.accent } else { p.border })))
    };
    let mut panes = div().h(px(114.)).flex();
    if prefs.sidebar_visible && !prefs.sidebar_right { panes = panes.child(panel(tr("Barra lateral", "Sidebar"), false)); }
    if prefs.show_preview && prefs.preview_left { panes = panes.child(panel(tr("Vista previa", "Preview"), true)); }
    panes = panes.child(div().flex_1().min_w(px(0.)).flex().flex_col().gap(px(7.)).p(px(8.))
        .child(div().text_size(px(9.)).text_color(p.muted).child(tr("Archivos", "Files")))
        .child(div().flex().flex_wrap().gap(px(7.)).children((0..8).map(|index| {
            div().w(px(28.)).h(px(29.)).rounded(px(3.)).bg(if index == 1 { p.selection } else { p.card })
                .flex().flex_col().items_center().justify_center().gap(px(3.))
                .child(glyph("\u{E8B7}", 10., if index == 1 { p.accent } else { p.dim }))
                .child(line(13., p.border))
        }))));
    if prefs.show_preview && !prefs.preview_left { panes = panes.child(panel(tr("Vista previa", "Preview"), true)); }
    if prefs.sidebar_visible && prefs.sidebar_right { panes = panes.child(panel(tr("Barra lateral", "Sidebar"), false)); }
    let status = || div().h(px(13.)).px(px(7.)).bg(p.titlebar).flex().items_center()
        .justify_between().child(line(35., p.border)).child(line(15., p.accent));
    div().rounded(px(7.)).overflow_hidden().border_1().border_color(p.border).bg(p.bg).flex().flex_col()
        .child(div().h(px(20.)).px(px(7.)).flex().items_center().gap(px(5.)).bg(p.titlebar)
            .children([line(22., p.accent), line(18., p.border), line(16., p.border)]))
        .when(prefs.status_bar_above, |map| map.child(status()))
        .child(div().h(px(19.)).p(px(4.)).flex().gap(px(4.)).bg(p.panel)
            .child(div().w(px(17.)).rounded(px(3.)).bg(p.card_hover))
            .child(div().flex_1().rounded(px(3.)).bg(p.input).pl(px(5.)).flex().items_center().child(line(30., p.border))))
        .when(prefs.command_bar, |map| map.child(div().h(px(14.)).px(px(7.)).flex().items_center().gap(px(4.))
            .children((0..6).map(|_| line(9., p.border)))))
        .child(panes)
        .when(!prefs.status_bar_above, |map| map.child(status()))
        .into_any_element()
}

fn wheel_tabs_setting(search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> impl Styled + IntoElement {
    toggle_row(search, "wheel-tabs", tr("Cambiar de pestaña con la rueda", "Switch tabs with the mouse wheel"),
        tr("Pasa a la pestaña anterior o siguiente al girarla sobre la barra de pestañas.", "Scroll over the tab bar to switch to the previous or next tab."),
        app.settings.wheel_tabs, p, cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
            this.settings.wheel_tabs = !this.settings.wheel_tabs;
            this.settings.save();
            cx.notify();
        }))
}

fn tab_settings_suboptions(reveal: &crate::motion::SettingsReveal, content: Div) -> Div {
    let progress = reveal.progress();
    let height = reveal.bounds.get().map_or(px(0.), |bounds| bounds.size.height);
    div().flex().flex_col().flex_shrink_0().overflow_hidden()
        .when(progress < 1., |section| section.h(height * progress))
        .child(div().relative().top(px(-8. * (1. - progress))).flex_shrink_0()
            .child(crate::motion::measure(reveal.bounds.clone()).absolute().size_full())
            .child(content))
}

fn tab_gradient_settings(
    search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>,
) -> Div {
    let prefs = app.settings.tab_gradient;
    div().flex().flex_col()
        .child(settings_divider(p))
        .child(settings_segments(search, "tab-gradient-intensity", tr("Intensidad", "Intensity"),
            [tr("Suave", "Soft"), tr("Media", "Medium"), tr("Intensa", "Strong")],
            prefs.intensity, p, cx, |this, index, _, cx| {
                this.settings.tab_gradient.intensity = index;
                this.settings.save();
                cx.notify();
            }).pl(px(24.)))
        .child(settings_divider(p))
        .child(settings_segments(search, "tab-gradient-border", tr("Borde", "Border"),
            [tr("Sin borde", "No border"), tr("Discreto", "Subtle"), tr("Del degradado", "Gradient color")],
            prefs.border, p, cx, |this, index, _, cx| {
                this.settings.tab_gradient.border = index;
                this.settings.save();
                cx.notify();
            }).pl(px(24.)))
}

fn tab_settings_preview(app: &XoolFiles, p: &Colors) -> Div {
    let prefs = &app.settings;
    let left = prefs.show_tab_close && prefs.tab_close_left;
    let downloads = app.quick.iter().find(|item| item.label == tr("Descargas", "Downloads")).map(|item| item.path.as_path());
    settings_surface(p).p(px(10.)).gap(px(8.))
        .child(div().flex().flex_wrap().gap(px(10.)).children([
            (tr("Carpeta", "Folder"), tr("Sin cursor", "Pointer away"), false),
            (tr("Descargas", "Downloads"), tr("Con cursor", "Pointer over tab"), true),
        ].into_iter().map(|(name, label, hovered)| {
            let show_close = prefs.show_tab_close && (prefs.tab_close_always || hovered);
            let close = || div().size(px(18.)).flex_shrink_0().flex().items_center().justify_center()
                .child(glyph("icons/tab-close.svg", 12., p.muted));
            let location = if hovered { downloads } else { None };
            div().flex().flex_col().gap(px(4.))
                .child(div().relative().h(px(TAB_HEIGHT)).w(px(TAB_WIDTH)).px(px(8.)).gap(px(6.))
                    .flex().items_center().rounded(px(8.)).border_1()
                    .border_color(if hovered || prefs.active_tab_gradient { p.accent_soft } else { p.border })
                    .bg(if hovered { p.card_hover } else { p.titlebar })
                    .when(prefs.active_tab_gradient, |tab| apply_active_tab_gradient(tab, p, prefs.tab_gradient))
                    .when(left && show_close, |tab| tab.child(close()))
                    .when(!(left && show_close) && !prefs.hide_tab_icons, |tab| {
                        tab.child(tab_location_icon(app, location.unwrap_or(std::path::Path::new("")), p.accent))
                    })
                    .child(div().flex_1().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD)
                        .whitespace_nowrap().child(name))
                    .when(!left && show_close, |tab| tab.child(close())))
                .child(div().text_size(px(11.)).text_color(p.muted).child(label))
        })))
}

fn settings_dropdown_trigger(id: &'static str, p: &Colors, cx: &gpui::App) -> gpui_component::button::Button {
    use gpui_component::button::{Button, ButtonCustomVariant, ButtonVariants};

    Button::new(id).rounded(px(11.)).h(px(36.)).px(px(11.)).gap(px(7.))
        .custom(ButtonCustomVariant::new(cx)
            .color(p.input.into())
            .border(p.border_soft.into())
            .foreground(p.text.into())
            .hover(p.card_hover.into())
            .active(p.card_hover.into()))
}

fn settings_dropdown_option(id: (&'static str, usize), selected: bool, p: &Colors) -> gpui::Stateful<Div> {
    div().id(id).h(px(28.)).px(px(8.)).rounded(px(8.))
        .flex().items_center().gap(px(7.)).cursor_pointer()
        .text_size(px(12.)).font_weight(FontWeight::SEMIBOLD)
        .text_color(if selected { p.text } else { p.muted })
        .when(selected, |row| row.bg(p.card_hover))
        .hover(|row| row.bg(p.card_hover).text_color(p.text))
}

fn startup_settings(search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    use crate::settings::{MAX_STARTUP_FOLDERS, StartupMode};
    use gpui_component::popover::Popover;

    let options = [
        (StartupMode::NewTab, tr("Nueva pestaña", "New tab")),
        (StartupMode::LastSession, tr("Última sesión", "Last session")),
        (StartupMode::Folders, tr("Abrir una página o páginas específicas", "Open a specific page or set of pages")),
    ];
    let mode = app.settings.startup_mode();
    let label = options.iter().find(|(value, _)| *value == mode).map_or(tr("Última sesión", "Last session"), |(_, label)| *label);
    let colors = *p;
    let owner = cx.weak_entity();
    let picker = Popover::new("startup-mode").anchor(gpui::Corner::TopRight).appearance(false)
        .trigger(settings_dropdown_trigger("startup-mode-trigger", p, cx)
            .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(label))
            .child(glyph("\u{E70D}", 9., p.muted)))
        .content(move |_, _, cx| {
            let panel = div().w(px(260.)).p(px(4.)).flex().flex_col().gap(px(1.))
                .bg(colors.panel).rounded(px(11.)).border_1().border_color(colors.border_soft).shadow_lg()
                .children(options.into_iter().enumerate().map(|(index, (choice, label))| {
                    let owner = owner.clone();
                    settings_dropdown_option(("startup-mode-option", index), choice == mode, &colors)
                        .child(div().flex_1().child(label))
                        .when(choice == mode, |row| row.child(glyph("\u{E73E}", 10., colors.accent)))
                        .on_click(cx.listener(move |popover, _: &ClickEvent, window, cx| {
                            popover.dismiss(window, cx);
                            let _ = owner.update(cx, |app, cx| {
                                app.settings.startup_mode = Some(choice);
                                app.settings.restore_session = choice == StartupMode::LastSession;
                                app.settings.save();
                                cx.notify();
                            });
                        }))
                }));
            popup_in(panel, "startup-mode-enter", |panel, slide| panel.relative().top(px(-slide)))
        });
    let description = tr("Elige qué pestañas se abren al iniciar Xool Files.", "Choose which tabs open when Xool Files starts.");
    let labels = [tr("Al abrir", "On startup"), description, tr("Nueva pestaña", "New tab"), tr("Última sesión", "Last session"), tr("Abrir una página o páginas específicas", "Open a specific page or set of pages"), tr("Añadir carpeta", "Add folder")];
    div().w_full().flex().flex_col()
        .child(search.mark(div().px(px(12.)).py(px(10.)).flex().flex_wrap()
                .items_center().justify_between().gap(px(8.)), &labels)
            .child(settings_row_text(tr("Al abrir", "On startup"), Some(description), p))
            .child(picker))
        .when(mode == StartupMode::Folders, |card| card
            .child(settings_divider(p))
            .child(div().px(px(12.)).py(px(8.))
                .flex().flex_col().gap(px(6.))
                .when(app.settings.startup_folders.is_empty(), |list| list.child(
                    div().text_size(px(12.)).text_color(p.muted)
                        .child(tr("Añade las carpetas que quieres abrir en pestañas.", "Add the folders you want to open in tabs."))))
                .children(app.settings.startup_folders.iter().enumerate().map(|(index, path)| {
                    div().flex().items_center().gap(px(7.))
                        .child(tab_location_icon(app, path, p.muted))
                        .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(12.)).child(path.display().to_string()))
                        .child(icon_button(("remove-startup-folder", index), "icons/tab-close.svg", 12., p, false, true,
                            cx.listener(move |app: &mut XoolFiles, _: &ClickEvent, _, cx| {
                                if index < app.settings.startup_folders.len() {
                                    app.settings.startup_folders.remove(index);
                                    app.settings.save();
                                    cx.notify();
                                }
                            })).size(px(24.)).tooltip(|window, cx| {
                                gpui_component::tooltip::Tooltip::new(tr("Quitar de las carpetas de inicio", "Remove from startup folders")).build(window, cx)
                            }))
                }))
                .when(app.settings.startup_folders.len() < MAX_STARTUP_FOLDERS, |list| list.child(
                    div().flex().child(secondary_button("add-startup-folder", tr("Añadir carpeta", "Add folder"), p,
                        cx.listener(|app: &mut XoolFiles, _: &ClickEvent, _, cx| app.add_startup_folders(cx))))))))
        .into_any_element()
}

fn general_settings(search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    let prefs = &app.settings;
    let languages = crate::i18n::Language::OPTIONS;
    let selected_language = languages.iter().position(|language| *language == prefs.language).unwrap_or(0);
    let general = div().flex().flex_col().flex_shrink_0().gap(px(9.))
        .child(settings_section(search, tr("IDIOMA", "LANGUAGE"), p))
        .child(settings_surface(p).child(settings_segments(search, "interface-language",
            tr("Idioma de la interfaz", "Interface language"),
            [tr("Automático", "Automatic"), "Español", "English"],
            selected_language, p, cx, |this, index, window, cx| {
                this.set_language(crate::i18n::Language::OPTIONS[index], window, cx);
            })))
        .child(settings_section(search, tr("PESTAÑAS Y PANELES", "TABS AND PANES"), p))
        .child(settings_surface(p)
            .child(startup_settings(search, app, p, cx))
            .child(settings_divider(p))
            .child(settings_choices(search, "new-tab-mode", (tr("Al crear una pestaña", "When creating a tab"), Some(tr("Elige si la nueva pestaña abre la carpeta actual o Inicio.", "Choose whether a new tab opens the current folder or Home."))), [tr("Carpeta actual", "Current folder"), tr("Inicio", "Home")],
                prefs.new_tab_home, p, cx, |this, value| this.settings.new_tab_home = value))
            .child(settings_divider(p))
            .child(wheel_tabs_setting(search, app, p, cx))
            .child(settings_divider(p))
            .child(settings_choices(search, "split-axis", (tr("Dirección de la división", "Split direction"), None), [tr("Lado a lado", "Side by side"), tr("Arriba y abajo", "Top and bottom")],
                prefs.split_below, p, cx, |this, value| this.settings.split_below = value)));

    general.child(settings_section(search, tr("DISTRIBUCIÓN", "LAYOUT"), p))
        .child(settings_surface(p).child(div().p(px(10.)).child(settings_layout_preview(app, p)))
        .child(settings_choices(search, "sidebar-position", (tr("Posición de la barra lateral", "Sidebar position"), None), [tr("Izquierda", "Left"), tr("Derecha", "Right")],
            prefs.sidebar_right, p, cx, |this, value| this.settings.sidebar_right = value))
        .child(settings_divider(p))
        .child(settings_choices(search, "status-bar-position", (tr("Posición de la barra de estado", "Status bar position"),
            Some(tr("Elige dónde se muestran la búsqueda de la carpeta, el contador de elementos y los controles de vista y zoom.", "Choose where to show folder search, the item count, and the view and zoom controls."))),
            [tr("Abajo", "Bottom"), tr("Debajo de las pestañas", "Below the tabs")], prefs.status_bar_above, p, cx,
            |this, above| this.settings.status_bar_above = above))
        .child(settings_divider(p))
        .child(toggle_row(search, "preview-visible", tr("Mostrar vista previa", "Show preview"), tr("Muestra el contenido y los datos del archivo seleccionado.", "Show the selected file’s contents and details."),
            prefs.show_preview, p, cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| this.toggle_preview(cx))))
        .child(settings_divider(p))
        .child(settings_choices(search, "preview-position", (tr("Posición de la vista previa", "Preview position"), None), [tr("Izquierda", "Left"), tr("Derecha", "Right")],
            !prefs.preview_left, p, cx, |this, value| this.settings.preview_left = !value))
        .child(settings_divider(p))
        .child(toggle_row(search, "show-command-bar", tr("Barra de comandos", "Command bar"), tr("Añade botones de archivo debajo de la ruta.", "Add file actions below the address bar."),
            prefs.command_bar, p, cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                this.settings.command_bar = !this.settings.command_bar;
                this.settings.save();
                cx.notify();
            })))
        )
        .child(settings_section(search, tr("VISTAS DE CARPETA", "FOLDER VIEWS"), p))
        .child(toggle_row(search, "remember-folders", tr("Recordar cada carpeta", "Remember each folder"), tr("Conserva la vista, el zoom y el orden por ubicación.", "Keep the view, zoom, and sort order for each location."),
            prefs.remember_folder_views, p, cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                this.settings.remember_folder_views = !this.settings.remember_folder_views;
                this.settings.save();
                cx.notify();
            })))
        .into_any_element()
}

fn appearance_settings(search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    let colors = div().p(px(14.)).flex().flex_col().gap(px(10.))
        .child(search.text(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD), tr("Color de acento", "Accent color")))
        .child(div().flex().flex_wrap().gap(px(8.)).children(
            theme::accents().into_iter().enumerate().map(|(index, (color, label))| {
                let selected = app.settings.accent == color;
                div().id(("accent", index)).w(px(62.)).py(px(6.)).rounded(px(5.))
                    .flex().flex_col().items_center().gap(px(5.)).cursor_pointer()
                    .hover(|style| style.bg(p.card_hover))
                    .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| this.set_accent(color, cx)))
                    .child(div().w(px(24.)).h(px(24.)).rounded_full().bg(rgb(color.rgb()))
                        .border_2().border_color(if selected { p.text } else { p.card }))
                    .child(search.text(div().text_size(px(11.)).text_color(if selected { p.text } else { p.muted }), label))
            })
        ));
    div().flex().flex_col().flex_shrink_0().gap(px(12.))
        .child(settings_section(search, "COLORES", p))
        .child(settings_surface(p)
            .child(settings_theme_dropdown(search, app, p, cx))
            .child(settings_divider(p))
            .child(colors))
        .child(settings_section(search, tr("FONDO DE LA VENTANA", "WINDOW BACKGROUND"), p))
        .child(window_background_settings(search, app, p, cx))
        .child(settings_section(search, "LISTA", p))
        .child(settings_surface(p)
            .child(settings_segments(search, "smooth-scroll", tr("Desplazamiento suave", "Smooth scrolling"), [tr("Desactivado", "Off"), tr("Firme", "Firm"), tr("Suave", "Smooth"), tr("Muy suave", "Very smooth")],
                match app.settings.wheel_scroll_ms { 85 => 1, 170 => 2, 260 => 3, _ => 0 },
                p, cx, |this, index, _, cx| {
                    this.settings.wheel_scroll_ms = [0, 85, 170, 260][index];
                    this.settings.save();
                    cx.notify();
                }))
            .child(settings_divider(p))
            .child(settings_segments(search, "name-lines", tr("Líneas del nombre en los iconos", "Filename lines in icon view"), ["1", "2", "3", "4"],
                usize::from(app.settings.grid_name_lines.clamp(1, 4) - 1),
                p, cx, |this, index, _, cx| {
                    this.settings.grid_name_lines = index as u8 + 1;
                    this.settings.save();
                    cx.notify();
                })))
        .into_any_element()
}

fn window_background_settings(
    search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>,
) -> impl IntoElement {
    let settings = &app.settings;
    let has_image = settings.background_image_path.is_some();
    let source = if app.background_image.is_some() {
        settings.background_image_path.as_ref().and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| tr("Imagen personalizada", "Custom image").into())
    } else if has_image {
        tr("Escritorio dinámico · imagen no cargada", "Dynamic desktop · image unavailable").into()
    } else {
        tr("Escritorio dinámico", "Dynamic desktop").into()
    };
    settings_surface(p)
        .child(toggle_row(search, "window-translucency", tr("Fondo translúcido", "Translucent background"),
            tr("Muestra un fondo difuminado que cambia con lo que queda detrás de la ventana.", "Show a blurred background that changes with what’s behind the window."),
            settings.translucent_background, p, cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                this.toggle_translucent_background(cx);
            })))
        .when(settings.translucent_background, |panel| panel
            .child(settings_divider(p))
            .child(search.mark(div().p(px(12.)).flex().flex_wrap().items_center().justify_between().gap(px(8.)),
                &[tr("Imagen de fondo", "Background image"), tr("Elegir imagen", "Choose image"), tr("Usar escritorio", "Use desktop")])
                .child(div().min_w(px(0.)).flex().flex_col().gap(px(2.))
                    .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(tr("Imagen de fondo", "Background image")))
                    .child(div().truncate().text_size(px(11.5)).text_color(p.muted).child(source)))
                .child(div().flex().flex_wrap().gap(px(6.))
                    .when(has_image, |buttons| buttons.child(secondary_button("reset-window-background", tr("Usar escritorio", "Use desktop"), p,
                        cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| this.use_desktop_background(cx)))))
                    .child(secondary_button("choose-window-background", tr("Elegir imagen", "Choose image"), p,
                        cx.listener(|this: &mut XoolFiles, _: &ClickEvent, _, cx| this.choose_background_image(cx))))))
            .child(settings_divider(p))
            .child(settings_segments(search, "window-tint", tr("Opacidad del tinte", "Tint opacity"), ["45 %", "60 %", "75 %", "90 %"],
                Settings::BACKGROUND_TINT_LEVELS.iter().position(|&level| level == settings.background_tint).unwrap_or(1),
                p, cx, |this, index, _, cx| {
                    this.settings.background_tint = Settings::BACKGROUND_TINT_LEVELS[index];
                    this.settings.save();
                    cx.notify();
                }))
            .when(has_image, |panel| panel
                .child(settings_divider(p))
                .child(settings_segments(search, "window-image-blur", tr("Desenfoque de la imagen", "Image blur"),
                    [tr("Ninguno", "None"), tr("Suave", "Smooth"), tr("Medio", "Medium"), tr("Intenso", "Strong")],
                    Settings::BACKGROUND_BLUR_LEVELS.iter().position(|&level| level == settings.background_blur).unwrap_or(2),
                    p, cx, |this, index, _, cx| {
                        this.settings.background_blur = Settings::BACKGROUND_BLUR_LEVELS[index];
                        this.settings.save();
                        this.refresh_background_image(cx);
                        cx.notify();
                    }))))
}

fn settings_theme_dropdown(search: &SettingsSearch<'_>, app: &XoolFiles, p: &Colors, cx: &mut Context<XoolFiles>) -> AnyElement {
    use gpui_component::popover::Popover;
    let themes = [
        (ThemeMode::Dark, tr("Oscuro", "Dark"), 0x1e1f22),
        (ThemeMode::Light, tr("Claro", "Light"), 0xf3f3f5),
        (ThemeMode::System, tr("Igual que Windows", "Follow Windows"), 0x868b96),
        (ThemeMode::MidnightSlate, tr("Pizarra nocturna", "Night Slate"), 0x263749),
        (ThemeMode::AyuMirage, "Ayu Mirage", 0x1f2430),
        (ThemeMode::Cherry, tr("Cerezo", "Cherry Blossom"), 0x543a4c),
    ];
    let matched = search.matches(&[tr("Tema", "Theme")]) || themes.iter().any(|(_, label, _)| search.matches(&[label]));
    let selected = app.settings.theme;
    let active = themes.iter().find(|(mode, _, _)| *mode == selected).unwrap_or(&themes[0]);
    let colors = *p;
    let owner = cx.entity().downgrade();
    let menu = Popover::new("settings-theme-menu")
        .anchor(gpui::Corner::TopRight)
        .appearance(false)
        .open(app.settings_theme_open)
        .on_open_change(cx.listener(|this: &mut XoolFiles, open: &bool, _, cx| {
            this.settings_theme_open = *open;
            cx.notify();
        }))
        .trigger(settings_dropdown_trigger("settings-theme-toggle", p, cx)
            .child(div().size(px(11.)).rounded_full().bg(rgb(active.2)).border_1().border_color(p.border))
            .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(active.1))
            .child(glyph("\u{E70D}", 9., p.muted)))
        .content(move |_, _, cx| {
            let panel = div().w(px(180.)).p(px(4.)).flex().flex_col().gap(px(1.))
                .bg(colors.panel).rounded(px(11.)).border_1().border_color(colors.border_soft).shadow_lg()
                .children(themes.into_iter().enumerate().map(|(index, (theme, label, color))| {
                    let owner = owner.clone();
                    settings_dropdown_option(("theme-option", index), theme == selected, &colors)
                        .child(div().size(px(12.)).rounded_full().bg(rgb(color)).border_1().border_color(colors.border))
                        .child(div().flex_1().child(label))
                        .when(theme == selected, |style| style.child(glyph("\u{E73E}", 10., colors.accent)))
                        .on_click(cx.listener(move |popover, _: &ClickEvent, window, cx| {
                            popover.dismiss(window, cx);
                            let _ = owner.update(cx, |this, cx| this.set_theme(theme, window, cx));
                        }))
                }));
            popup_in(panel, "settings-theme-enter", |panel, slide| panel.relative().top(px(-slide)))
        });
    div().px(px(14.)).py(px(10.)).flex().items_center().justify_between().gap(px(8.))
        .when(matched, |row| row.bg(rgba(SETTINGS_HIGHLIGHT)))
        .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(tr("Tema", "Theme")))
        .child(menu)
        .into_any_element()
}

fn compact_row_height(app: &XoolFiles) -> f32 {
    f32::from(app.settings.compact_row_height.clamp(20, 25))
}

fn settings_divider(p: &Colors) -> Div {
    div().w_full().h(px(1.)).flex_shrink_0().bg(p.border)
}

fn settings_surface(p: &Colors) -> Div {
    div().flex().flex_col().flex_shrink_0().overflow_hidden().p(px(2.)).rounded(px(11.))
        .border_1().border_color(p.border_soft).bg(p.card)
}

fn settings_segments<const N: usize>(search: &SettingsSearch<'_>,
    id: &'static str, label: &'static str, labels: [&'static str; N], active: usize,
    p: &Colors, cx: &mut Context<XoolFiles>,
    choose: fn(&mut XoolFiles, usize, &mut Window, &mut Context<XoolFiles>),
) -> impl Styled + IntoElement {
    let matched = search.matches(&[label]) || search.matches(&labels);
    div().px(px(14.)).py(px(10.)).flex().flex_wrap().gap(px(8.)).items_center().justify_between()
        .when(matched, |row| row.bg(rgba(SETTINGS_HIGHLIGHT)).rounded(px(5.)))
        .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(label))
        .child(settings_segment_group(id, labels, active, None, p, cx, choose))
}

fn settings_segment_group<const N: usize>(
    id: &'static str, labels: [&'static str; N], active: usize, extra: Option<(String, &'static str)>,
    p: &Colors, cx: &mut Context<XoolFiles>,
    choose: impl Fn(&mut XoolFiles, usize, &mut Window, &mut Context<XoolFiles>) + Copy + 'static,
) -> gpui::Stateful<Div> {
    let selection = crate::motion::selection();
    let options = labels.into_iter().map(|label| (SharedString::from(label), None))
        .chain(extra.map(|(label, hint)| (SharedString::from(label), Some(hint))));
    div().id(id).relative().flex().flex_wrap().gap(px(1.)).p(px(3.))
        .rounded(px(11.)).border_1().border_color(p.border_soft).bg(p.input)
        .child(crate::motion::SlidingFill::new("active-choice", selection.clone(), p.card_hover, px(8.)))
        .children(options.enumerate().map(|(index, (label, hint))| {
            let selected = index == active;
            div().id((id, index)).relative().h(px(28.)).px(px(10.))
                .when(N == 2, |item| item.min_w(px(65.)))
                .flex().items_center().justify_center().whitespace_nowrap()
                .text_size(px(12.)).font_weight(FontWeight::SEMIBOLD)
                .text_color(if selected { p.text } else { p.muted })
                .when(selected, |item| item.child(crate::motion::measure(selection.clone())
                    .absolute().size_full().left(px(0.)).top(px(0.))))
                .when_some(hint, |item, hint| item.tooltip(move |window, cx|
                    gpui_component::tooltip::Tooltip::new(hint).build(window, cx)))
                .when(!selected || hint.is_some(), |item| item.cursor_pointer()
                    .hover(|style| style.text_color(p.text))
                    .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, window, cx| {
                        choose(this, index, window, cx);
                    })))
                .child(label)
        }))
}

fn settings_section(search: &SettingsSearch<'_>, label: &'static str, p: &Colors) -> AnyElement {
    settings_section_label(search.mark(div(), &[label]), label, p)
        .pt(px(8.))
        .pb(px(6.))
        .into_any_element()
}

fn settings_section_label(element: Div, label: &'static str, p: &Colors) -> Div {
    element
        .text_size(px(11.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.dim)
        .child(label)
}

fn search_shortcut_text(text: &str) -> String {
    text.to_lowercase().chars().filter_map(|ch| match ch {
        'á' => Some('a'), 'é' => Some('e'), 'í' => Some('i'), 'ó' => Some('o'), 'ú' | 'ü' => Some('u'),
        '+' => Some(' '),
        other => Some(other),
    }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

fn shortcut_editor_row(search: &SettingsSearch<'_>, 
    app: &XoolFiles,
    def: &'static crate::shortcuts::ShortcutDef,
    index: usize,
    p: &Colors,
    cx: &mut Context<XoolFiles>,
) -> AnyElement {
    let recording = app.recording_shortcut == Some(def.id);
    let keys = app.settings.shortcut(def.id);
    let unbound = crate::shortcuts::is_unbound(&keys);
    let customized = app.settings.shortcuts.contains_key(def.id);

    let badge_text = if recording {
        tr("Pulsa una combinación…", "Press a key combination…").to_string()
    } else if unbound {
        tr("Sin atajo", "Unassigned").to_string()
    } else {
        crate::shortcuts::display(&keys)
    };

    let mut row = search.mark(div(), &[def.label(), &badge_text])
        .min_h(px(42.))
        .py(px(5.))
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_size(px(14.))
                .line_height(px(18.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.text)
                .child(def.label()),
        )
        .child(
            div()
                .id(("shortcut-key", index))
                .flex_shrink_0()
                .flex().items_center().gap(px(4.))
                .px(px(4.)).py(px(3.)).rounded(px(6.))
                .text_size(px(12.)).text_color(p.text)
                .when(recording || unbound, |button| button.px(px(8.)).border_1()
                    .border_color(if recording { p.accent } else { p.border })
                    .bg(if recording { p.accent_faint } else { p.input })
                    .text_color(if recording { p.accent } else { p.muted }))
                .cursor_pointer()
                .hover(|style| style.bg(p.card_hover))
                .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.recording_shortcut = Some(def.id);
                    this.context_menu = None;
                    this.sort_menu = false;
                    this.view_menu = false;
                    cx.notify();
                }))
                .when(recording || unbound, |button| button.child(badge_text.clone()))
                .when(!recording && !unbound, |button| button.children(
                    badge_text.split(" + ").enumerate().map(|(index, key)| {
                        div().flex().items_center().gap(px(4.))
                            .when(index != 0, |part| part.child(div().text_color(p.dim).child("+")))
                            .child(div().min_w(px(22.)).h(px(22.)).px(px(5.)).rounded(px(4.))
                                .flex().items_center().justify_center().bg(p.input)
                                .border_1().border_color(p.border).border_b(px(2.))
                                .whitespace_nowrap().child(key.to_owned()))
                    }))),
        );

    if !recording && !unbound {
        row = row.child(
            div()
                .id(("shortcut-clear", index))
                .w(px(26.))
                .h(px(26.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .text_size(px(14.))
                .text_color(p.dim)
                .cursor_pointer()
                .hover(|style| style.bg(p.card_hover).text_color(p.text))
                .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.clear_shortcut(def.id, cx);
                }))
                .child("×"),
        );
    }

    if customized && !recording {
        row = row.child(
            div()
                .id(("shortcut-reset", index))
                .w(px(26.))
                .h(px(26.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .text_color(p.dim)
                .cursor_pointer()
                .hover(|style| style.bg(p.card_hover).text_color(p.text))
                .on_click(cx.listener(move |this: &mut XoolFiles, _: &ClickEvent, _, cx| {
                    this.reset_shortcut(def.id, cx);
                }))
                .child(glyph("\u{E7A7}", 12., p.dim)),
        );
    }

    row.into_any_element()
}

fn storage_row(label: &str, bytes: u64, p: &Colors) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .text_size(px(11.5))
        .child(div().text_color(p.muted).child(label.to_string()))
        .child(div().text_color(p.text).child(format_bytes(bytes)))
        .into_any_element()
}

fn format_bytes(bytes: u64) -> String {
    fs_model::format_size(bytes)
}

fn toggle_row(search: &SettingsSearch<'_>, 
    id: &'static str,
    title: &'static str,
    description: &'static str,
    value: bool,
    p: &Colors,
    handler: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl Styled + IntoElement {
    div()
        .id(id)
        .min_h(px(54.))
        .flex_shrink_0()
        .px(px(12.))
        .py(px(8.))
        .rounded(px(8.))
        .bg(p.card)
        .when(search.matches(&[title, description]), |row| row.bg(rgba(SETTINGS_HIGHLIGHT)))
        .flex()
        .items_center()
        .gap(px(10.))
        .cursor_pointer()
        .hover(|style| style.bg(p.card_hover))
        .on_click(handler)
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(title))
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(p.muted)
                        .child(description),
                ),
        )
        .child(switch(value, p))
}

// ---------------------------------------------------------------------------
// Arrastrar y soltar
// ---------------------------------------------------------------------------

struct DragGhost {
    label: SharedString,
    count: usize,
    colors: Colors,
    icon: &'static str,
}

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(10.))
            .py(px(6.))
            .rounded(px(9.))
            .bg(self.colors.panel)
            .border_1()
            .border_color(self.colors.border)
            .flex()
            .items_center()
            .gap(px(7.))
            .child(glyph(self.icon, 12., self.colors.accent))
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(self.colors.text)
                    .child(self.label.clone()),
            )
            .when(self.count > 1, |style| {
                style.child(
                    div()
                        .px(px(5.))
                        .rounded(px(5.))
                        .bg(self.colors.accent_soft)
                        .text_size(px(9.))
                        .text_color(self.colors.accent)
                        .child(format!("{}", self.count)),
                )
            })
    }
}

// Allow GPUI to finish the callback before releasing its internal drag view.
fn start_native_drag(files: &DragFiles, window: &mut Window, cx: &mut gpui::App) -> gpui::Task<()> {
    let paths = files.paths.to_vec();
    let handle = window.window_handle();
    let owner = platform::hwnd(window);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (proceed_tx, proceed_rx) = std::sync::mpsc::channel();
    platform::start_file_drag(paths, ready_tx, proceed_rx);
    cx.spawn(async move |cx| {
        // Espera a que el objeto de datos del Shell esté listo.
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(2))
                .await;
            match ready_rx.try_recv() {
                Ok(()) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        // Mientras el cursor siga sobre Xool Files, el arrastre es interno (GPUI).
        // Al salir de la ventana, se cede a OLE para soltar en otros programas.
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(8))
                .await;
            if !platform::left_button_down() {
                // Se soltó dentro: el arrastre interno ya se encargó.
                break;
            }
            if !platform::cursor_over_hwnd(owner) {
                let handoff = cx.update_window(handle, |_, window, cx| {
                    cx.stop_active_drag(window)
                }).unwrap_or(false);
                if handoff { let _ = proceed_tx.send(()); }
                break;
            }
        }
    })
}
fn drag_ghost(files: &DragFiles, colors: Colors, cx: &mut gpui::App) -> Entity<DragGhost> {
    let label = files
        .paths
        .first()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| tr("Elementos", "Items").to_string());
    let count = files.paths.len();
    cx.new(|_| DragGhost {
        label: SharedString::from(label),
        count,
        colors,
        icon: "\u{E7C3}",
    })
}

fn tab_drag_ghost(drag: &DragTab, colors: Colors, cx: &mut gpui::App) -> Entity<DragGhost> {
    cx.new(|_| DragGhost {
        label: SharedString::from(drag.label.clone()),
        count: 0,
        colors,
        icon: "\u{E8B7}",
    })
}

fn update_pane_drop(
    app: &mut XoolFiles, pane: usize, zone: Option<TabDropZone>, bounds: Option<Bounds<Pixels>>,
    cx: &mut Context<XoolFiles>,
) {
    if let Some(zone) = zone {
        if app.tab_drop_zone == Some(zone) && app.tab_drop_pane == Some(pane)
            && app.folder_drop_bounds == bounds
        {
            return;
        }
        app.tab_drop_zone = Some(zone);
        app.tab_drop_pane = Some(pane);
        app.folder_drop_bounds = bounds;
    } else if app.tab_drop_pane == Some(pane) {
        app.tab_drop_zone = None;
        app.tab_drop_pane = None;
        app.folder_drop_bounds = None;
    } else {
        return;
    }
    cx.notify();
}

fn tab_drop_zone(bounds: Bounds<Pixels>, position: gpui::Point<Pixels>, threshold: f32) -> TabDropZone {
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    let x = f32::from(position.x - bounds.origin.x) / width.max(1.0);
    let y = f32::from(position.y - bounds.origin.y) / height.max(1.0);
    [
        (x, TabDropZone::Left),
        (1.0 - x, TabDropZone::Right),
        (y, TabDropZone::Top),
        (1.0 - y, TabDropZone::Bottom),
    ].into_iter()
        .filter(|(distance, zone)| *distance < threshold && zone.fits(width, height))
        .min_by(|(a, _), (b, _)| a.total_cmp(b))
        .map_or(TabDropZone::Center, |(_, zone)| zone)
}

fn tab_drop_overlay(zone: TabDropZone, p: &Colors) -> Div {
    let (label, icon) = match zone {
        TabDropZone::Left => (tr("Izquierda", "Left"), "\u{E76B}"),
        TabDropZone::Center => (tr("Centro", "Center"), "\u{E8B7}"),
        TabDropZone::Right => (tr("Derecha", "Right"), "\u{E76C}"),
        TabDropZone::Top => (tr("Arriba", "Top"), "\u{E74A}"),
        TabDropZone::Bottom => (tr("Abajo", "Bottom"), "\u{E74B}"),
    };

    let mut container = div()
        .absolute()
        .top(px(0.))
        .left(px(0.))
        .size_full()
        .p(px(8.))
        .flex()
        .bg(rgba(0x00000026));
    container = match zone {
        TabDropZone::Left => container.justify_start(),
        TabDropZone::Right => container.justify_end(),
        TabDropZone::Top => container.items_start(),
        TabDropZone::Bottom => container.items_end(),
        TabDropZone::Center => container.items_center().justify_center(),
    };

    let mut target = div()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .rounded(px(12.))
        .border_2()
        .border_color(p.accent)
        .bg(p.accent_soft)
        .text_color(p.accent)
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(glyph(icon, 14., p.accent))
        .child(label);
    target = match zone {
        TabDropZone::Left => target.w(gpui::relative(0.5)).h_full(),
        TabDropZone::Right => target.w(gpui::relative(0.5)).h_full(),
        TabDropZone::Top | TabDropZone::Bottom => target.w_full().h(gpui::relative(0.5)),
        TabDropZone::Center => target.w(gpui::relative(0.5)).h(gpui::relative(0.6)),
    };

    container.child(target)
}

fn folder_drop_target(
    element: gpui::Stateful<Div>, path: PathBuf, tab_id: Option<u64>,
    p: &Colors, cx: &mut Context<XoolFiles>,
) -> gpui::Stateful<Div> {
    let internal = path.clone();
    let external = path.clone();
    attach_drop_target(element, path, p, cx)
        .on_drag_move::<DragFiles>(cx.listener(
            move |this: &mut XoolFiles, event: &gpui::DragMoveEvent<DragFiles>, window, cx| {
                let inside = event.bounds.contains(&event.event.position)
                    && !event.drag(cx).paths.iter().any(|source| internal.starts_with(source));
                this.hover_drag_folder(tab_id, &internal, event.bounds, inside, window, cx);
            },
        ))
        .on_drag_move::<gpui::ExternalPaths>(cx.listener(
            move |this: &mut XoolFiles, event: &gpui::DragMoveEvent<gpui::ExternalPaths>, window, cx| {
                let inside = event.bounds.contains(&event.event.position)
                    && !event.drag(cx).paths().iter().any(|source| external.starts_with(source));
                this.hover_drag_folder(tab_id, &external, event.bounds, inside, window, cx);
            },
        ))
}

fn attach_drop_target<E: InteractiveElement + 'static>(
    mut element: E,
    destination: PathBuf,
    p: &Colors,
    cx: &mut Context<XoolFiles>,
) -> E {
    let internal_dest = destination.clone();
    element = element.on_drop(cx.listener(
        move |this: &mut XoolFiles, files: &DragFiles, window, cx| {
            this.handle_internal_drop(files, internal_dest.clone(), window, cx);
        },
    ));
    element = element.on_drop(cx.listener(
        move |this: &mut XoolFiles, files: &gpui::ExternalPaths, window, cx| {
            this.handle_external_drop(files, destination.clone(), window, cx);
        },
    ));
    let hover_bg = p.accent_soft;
    element
        .drag_over::<DragFiles>(move |style, _, _, _| style.bg(hover_bg))
        .drag_over::<gpui::ExternalPaths>(move |style, _, _, _| style.bg(hover_bg))
}

// ---------------------------------------------------------------------------
// Componentes pequeños
// ---------------------------------------------------------------------------

fn glyph(icon: &'static str, size: f32, color: Rgba) -> Div {
    glyph_inherit(icon, size).text_color(color)
}

fn glyph_inherit(icon: &'static str, size: f32) -> Div {
    if icon.starts_with("icons/") {
        return div().flex_shrink_0().size(px(size)).child(
            canvas(|_, _, _| (), move |bounds, (), window, cx| {
                let color = window.text_style().color;
                let _ = window.paint_svg(bounds, icon.into(), Default::default(), color, cx);
            }).size_full(),
        );
    }
    div().flex_shrink_0().font_family(ICON_FONT).text_size(px(size)).child(icon)
}

fn segment_button(
    id: impl Into<ElementId>,
    icon: &'static str,
    label: Option<&'static str>,
    p: &Colors,
    active: bool,
    handler: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let mut button = div()
        .id(id)
        .h_full()
        .px(px(if label.is_some() { 8. } else { 7. }))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(5.))
        .rounded(px(6.))
        .bg(if active { p.accent_faint } else { rgba(0x00000000) })
        .text_color(if active { p.accent } else { p.muted })
        .text_size(px(10.5))
        .when(active, |style| {
            style.border_1().border_color(p.accent_soft)
        })
        .when(!active, |style| {
            style.hover(|style| style.bg(p.card_hover).text_color(p.text))
        })
        .cursor_pointer()
        .on_click(handler)
        .child(if icon.is_empty() {
            split_glyph(if active { p.accent } else { p.muted })
        } else {
            glyph(icon, 11., if active { p.accent } else { p.muted })
        });
    if let Some(label) = label {
        button = button.child(label);
    }
    button.into_any_element()
}

fn split_glyph(color: Rgba) -> Div {
    div().w(px(13.)).h(px(11.)).rounded(px(2.)).border_1().border_color(color)
        .flex().justify_center().child(div().w(px(1.)).h_full().bg(color))
}

fn icon_button(
    id: impl Into<ElementId>,
    icon: &'static str,
    size: f32,
    p: &Colors,
    active: bool,
    enabled: bool,
    handler: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<Div> {
    let mut button = div()
        .id(id)
        .w(px(30.))
        .h(px(30.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .text_color(if active {
            p.accent
        } else if enabled {
            p.muted
        } else {
            p.dim
        })
        .when(active, |style| style.bg(p.accent_soft))
        .when(enabled, |style| {
            style
                .hover(|style| style.bg(p.card_hover).text_color(p.text))
                .cursor_pointer()
                .on_click(handler)
        })
        .when(!enabled, |style| style.opacity(0.35));

    if active {
        button = button.border_1().border_color(p.accent_soft);
    }

    button.child(glyph_inherit(icon, size))
}

fn primary_button(
    id: &'static str,
    label: &'static str,
    p: &Colors,
    handler: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .h(px(31.))
        .px(px(14.))
        .rounded(px(8.))
        .bg(p.accent)
        .text_color(p.accent_fg)
        .text_size(px(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|style| style.bg(p.accent_hover))
        .on_click(handler)
        .child(label)
        .into_any_element()
}

fn secondary_button(
    id: &'static str,
    label: &'static str,
    p: &Colors,
    handler: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .h(px(31.))
        .px(px(13.))
        .rounded(px(8.))
        .bg(p.card)
        .border_1()
        .border_color(p.border)
        .text_color(p.muted)
        .text_size(px(10.5))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|style| style.bg(p.card_hover).text_color(p.text))
        .on_click(handler)
        .child(label)
        .into_any_element()
}

fn danger_button(
    id: &'static str,
    label: &'static str,
    p: &Colors,
    handler: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .h(px(31.))
        .px(px(13.))
        .rounded(px(8.))
        .bg(p.danger)
        .text_color(rgb(0xffffff))
        .text_size(px(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|style| style.opacity(0.88))
        .on_click(handler)
        .child(label)
        .into_any_element()
}

fn switch(value: bool, p: &Colors) -> Div {
    let selection = crate::motion::selection();
    div()
        .relative()
        .w(px(32.))
        .h(px(17.))
        .flex_shrink_0()
        .rounded_full()
        .bg(if value { p.accent } else { p.border })
        .p(px(2.))
        .flex()
        .items_center()
        .when(value, |style| style.justify_end())
        .when(!value, |style| style.justify_start())
        .child(crate::motion::measure(selection.clone()).size(px(13.)))
        .child(crate::motion::SlidingFill::new("switch-thumb", selection,
            if value { p.accent_fg } else { p.muted }, px(7.)))
}

fn kind_glyph(kind: FileKind) -> &'static str {
    match kind {
        FileKind::Folder => "\u{E8B7}",
        FileKind::Image => "\u{E91B}",
        FileKind::Video => "\u{E714}",
        FileKind::Audio => "\u{E8D6}",
        FileKind::Document => "\u{E8A5}",
        FileKind::Code => "\u{E943}",
        FileKind::Archive => "\u{E7B8}",
        FileKind::Executable => "\u{E7C3}",
        FileKind::Font => "\u{E8D2}",
        FileKind::Other => "\u{E8A5}",
    }
}

fn toast_view(toast: &app::Toast, token: u64, viewport_w: f32, bottom: f32, p: &Colors) -> AnyElement {
    let (icon, color) = match toast.tone {
        ToastTone::Info => ("\u{E946}", p.accent),
        ToastTone::Success => ("\u{E73E}", p.success),
        ToastTone::Error => ("\u{E7BA}", p.danger),
    };

    div()
        .id("toast")
        .absolute()
        .bottom(px(bottom))
        .right(px(16.))
        .max_w(px((viewport_w - 32.).clamp(0., 380.)))
        .pl(px(10.))
        .pr(px(15.))
        .py(px(8.))
        .rounded(px(12.))
        .bg(p.card)
        .border_1()
        .border_color(p.border)
        .shadow_lg()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(div().size(px(26.)).flex_shrink_0().rounded_full()
            .bg(Rgba { a: 0.14, ..color })
            .flex().items_center().justify_center()
            .child(glyph(icon, 13., color)))
        .child(
            div().min_w(px(0.))
                .text_size(px(12.))
                .line_height(px(16.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.text)
                .child(SharedString::from(toast.text.clone())),
        )
        .with_animation(
            ("toast-in", token as usize),
            Animation::new(Duration::from_millis(160)).with_easing(ease_out_quint()),
            move |card, progress| card
                .bottom(px(bottom - 6. * (1. - progress)))
                .opacity(progress),
        )
        .into_any_element()
}


#[cfg(test)]
mod context_menu_tests {
    use super::{ContextMenuStyle, ContextPage, MenuAction, MenuRow, context_menu_axis, context_menu_layout};

    #[test]
    fn context_menu_styles_keep_extra_actions_accessible() {
        let tools = vec![
            MenuRow::item("", "Copiar", "", MenuAction::CopySelection),
            MenuRow::item("", "Cortar", "", MenuAction::CutSelection),
        ];
        let rows = vec![
            MenuRow::item("", "Abrir", "", MenuAction::Open(Default::default())),
            MenuRow::item("", "Compartir", "", MenuAction::ShellCommand(1)),
        ];

        let standard = context_menu_layout(ContextMenuStyle::default(), tools.clone(), rows.clone());
        assert_eq!(standard.tools.len(), 2);
        assert_eq!(standard.rows.len(), 2);

        let minimal = context_menu_layout(ContextMenuStyle::Minimal, tools, rows);
        assert!(minimal.tools.is_empty());
        assert!(matches!(minimal.rows.last(), Some(MenuRow::Item(entry))
            if matches!(&entry.action, MenuAction::ContextSubmenu(ContextPage::More, 0))));
        assert_eq!(minimal.more.len(), 2);
        assert!(matches!(minimal.more.first(), Some(MenuRow::Item(entry))
            if matches!(&entry.action, MenuAction::ShellCommand(1))));
    }

    #[test]
    fn context_menu_moves_inward_and_limits_height_to_the_window() {
        assert_eq!(context_menu_axis(120.0, 820.0, 510.0), (120.0, 510.0));
        assert_eq!(context_menu_axis(789.0, 820.0, 510.0), (302.0, 510.0));
        assert_eq!(context_menu_axis(400.0, 420.0, 510.0), (8.0, 404.0));
        assert_eq!(context_menu_axis(317.0, 320.0, 268.0), (44.0, 268.0));
    }
}
