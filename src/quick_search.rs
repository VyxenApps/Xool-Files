//! Ventana flotante del buscador global.

use std::path::PathBuf;
use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
use std::time::Duration;

use gpui::{
    AnyWindowHandle, App, AppContext, Context, Entity, KeyDownEvent, Pixels, Render,
    RenderImage, ScrollHandle, Size, Subscription, Task, WeakEntity, Window, WindowBackgroundAppearance,
    WindowBounds, WindowHandle, WindowKind, WindowOptions, ParentElement, Styled, div,
};
use gpui_component::{Root, input::{InputEvent, InputState}};

use crate::app::{Toast, XoolFiles};
use crate::fs_model::FileKind;
use crate::platform::SearchMonitor;
use crate::{integration, theme, thumbs};

pub(crate) const ROW_HEIGHT: f32 = 42.;
pub(crate) const ROW_GAP: f32 = 2.;
pub(crate) const LIST_PADDING: f32 = 5.;
pub(crate) const HEADER_HEIGHT: f32 = 54.;
pub(crate) const FOOTER_HEIGHT: f32 = 27.;
pub(crate) const SHADOW_INSET: f32 = 11.;
const BASE_HEIGHT: f32 = HEADER_HEIGHT + FOOTER_HEIGHT + 2. + (FRAME_INSET + SHADOW_INSET) * 2.;
const WIDTH: f32 = 760. + SHADOW_INSET * 2.;
pub(crate) const CORNER_RADIUS: f32 = 12.;
pub(crate) const FRAME_INSET: f32 = 2.;

#[derive(Clone)]
pub(crate) struct QuickSearchWindow {
    window: WindowHandle<Root>,
    view: WeakEntity<QuickSearch>,
}

impl QuickSearchWindow {
    pub fn open(owner: &Entity<XoolFiles>, main: AnyWindowHandle, monitor: SearchMonitor, cx: &mut App) -> anyhow::Result<Self> {
        let bounds = monitor.bounds(monitor.size(WIDTH, BASE_HEIGHT), SHADOW_INSET);
        let mut view = None;
        let window = cx.open_window(WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some(crate::i18n::tr("Búsqueda de Xool Files", "Xool Files Search").into()),
                appears_transparent: true,
                traffic_light_position: None,
            }),
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            display_id: monitor.display,
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        }, |window, cx| {
            crate::platform::use_custom_search_border(window);
            let search = cx.new(|cx| QuickSearch::new(owner, main, monitor, window, cx));
            view = Some(search.downgrade());
            let weak = search.downgrade();
            window.defer(cx, move |window, cx| {
                let _ = weak.update(cx, |this, cx| this.activate(window, cx));
            });
            cx.new(|cx| Root::new(search, window, cx).with_transparent_background())
        })?;
        Ok(Self { window, view: view.expect("search window view") })
    }

    pub fn activate(&self, monitor: SearchMonitor, cx: &mut App) -> bool {
        self.window.update(cx, |_, window, cx| {
            self.view.update(cx, |this, cx| {
                this.monitor = monitor;
                this.activate(window, cx);
            }).is_ok()
        }).unwrap_or(false)
    }

    pub fn close(&self, cx: &mut App) {
        let _ = self.window.update(cx, |_, window, _| window.remove_window());
    }

    pub fn refresh(&self, cx: &mut App) {
        let _ = self.window.update(cx, |_, window, cx| {
            let _ = self.view.update(cx, |this, cx| this.search(window, cx));
        });
    }
}

pub(crate) struct QuickResult {
    pub path: PathBuf,
    pub is_dir: bool,
    pub visual: Option<Arc<RenderImage>>,
}

pub(crate) struct QuickSearch {
    pub owner: WeakEntity<XoolFiles>,
    main: AnyWindowHandle,
    monitor: SearchMonitor,
    pub input: Entity<InputState>,
    pub results: Vec<QuickResult>,
    pub selection: usize,
    pub scroll: ScrollHandle,
    pub searching: bool,
    pub indexing: bool,
    generation: Arc<AtomicU64>,
    task: Option<Task<()>>,
    pending_image_drops: Vec<Arc<RenderImage>>,
    toast_token: u64,
    was_active: bool,
    _subscriptions: [Subscription; 3],
}

impl QuickSearch {
    fn new(owner: &Entity<XoolFiles>, main: AnyWindowHandle, monitor: SearchMonitor, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(crate::i18n::tr("Buscar archivos y carpetas o escribir una ruta", "Search for files and folders or enter a path")));
        let subscription = cx.subscribe_in(&input, window, |this: &mut Self, _, event, window, cx| match event {
            InputEvent::Change => this.search(window, cx),
            InputEvent::PressEnter { .. } => this.open_result(this.selection, window, cx),
            _ => {}
        });
        let owner_subscription = cx.observe(owner, |_, _, cx| cx.notify());
        let activation_subscription = cx.observe_window_activation(window, |this, window, _| {
            if window.is_window_active() {
                this.was_active = true;
            } else if this.was_active && !crate::platform::search_has_foreground(window) {
                this.close(window);
            }
        });
        Self {
            owner: owner.downgrade(), main, monitor, input, results: Vec::new(),
            selection: 0, scroll: ScrollHandle::new(), searching: false, indexing: false,
            generation: Arc::default(), task: None, pending_image_drops: Vec::new(),
            toast_token: owner.read(cx).toast_token,
            was_active: false,
            _subscriptions: [subscription, owner_subscription, activation_subscription],
        }
    }

    fn size(&self) -> Size<Pixels> {
        let rows = self.results.len();
        let list_height = if rows == 0 {
            0.
        } else {
            rows as f32 * ROW_HEIGHT + (rows - 1) as f32 * ROW_GAP + LIST_PADDING * 2.
        };
        self.monitor.size(WIDTH, BASE_HEIGHT + list_height)
    }

    fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.monitor.place(window, self.size(), CORNER_RADIUS + SHADOW_INSET, SHADOW_INSET, cx);
        window.activate_window();
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let token = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.task = None;
        self.indexing = false;
        let text = self.input.read(cx).value().trim().to_string();
        let Some(owner) = self.owner.upgrade().filter(|_| !text.is_empty()) else {
            self.replace_results(Vec::new());
            self.searching = false;
            self.monitor.place(window, self.size(), CORNER_RADIUS + SHADOW_INSET, SHADOW_INSET, cx);
            cx.notify();
            return;
        };
        let app = owner.read(cx);
        let ignore_accents = app.settings.search_ignore_accents;
        let fallback = app.quick.first().map(|item| item.path.clone());
        let index = app.search_index.clone();

        self.searching = true;
        let generation = self.generation.clone();
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            loop {
                if generation.load(Ordering::Relaxed) != token { return; }
                let (results, indexing) = cx.background_executor().spawn({
                    let generation = generation.clone();
                    let index = index.clone();
                    let text = text.clone();
                    let fallback = fallback.clone();
                    async move {
                        let path = PathBuf::from(text.trim_matches('"'));
                        if path.is_absolute() && !text.contains(['*', '?']) && path.exists() {
                            return (vec![(path.is_dir(), path)], false);
                        }
                        index.schedule(false);
                        let status = index.status();
                        let paths = if status.ready {
                            index.find(&text, ignore_accents, &generation, token)
                        } else {
                            fallback.as_deref().map(|root| integration::quick_matches(
                                root, &text, ignore_accents, &generation, token)).unwrap_or_default()
                        };
                        let results = paths.into_iter().take(9).map(|path| (path.is_dir(), path)).collect::<Vec<_>>();
                        (results, status.building)
                    }
                }).await;
                if generation.load(Ordering::Relaxed) != token { return; }
                let Ok(missing_visuals) = this.update_in(cx, |this, window, cx| {
                    this.replace_results(results);
                    this.searching = false;
                    this.indexing = indexing;
                    this.monitor.place(window, this.size(), CORNER_RADIUS + SHADOW_INSET, SHADOW_INSET, cx);
                    cx.notify();
                    this.results.iter().enumerate().filter(|(_, result)| result.visual.is_none())
                        .map(|(row, result)| (row, result.is_dir, result.path.clone())).collect::<Vec<_>>()
                }) else { return; };

                for (row, is_dir, path) in missing_visuals {
                    if generation.load(Ordering::Relaxed) != token { return; }
                    let visual = cx.background_executor().spawn(async move {
                        let kind = FileKind::from_path(&path, is_dir);
                        let (thumb, icon) = thumbs::entry_visual(&path, is_dir, kind, 96);
                        thumb.or(icon)
                    }).await;
                    if let Some(visual) = visual {
                        let _ = this.update_in(cx, |this, _, cx| {
                            if this.generation.load(Ordering::Relaxed) == token
                                && let Some(result) = this.results.get_mut(row)
                            {
                                result.visual = Some(visual);
                                cx.notify();
                            }
                        });
                    }
                }

                if !indexing { return; }
                while index.status().building {
                    if generation.load(Ordering::Relaxed) != token { return; }
                    cx.background_executor().timer(Duration::from_millis(450)).await;
                }
                let status = index.status();
                if !status.ready || status.error.is_some() {
                    let _ = this.update_in(cx, |this, _, cx| { this.indexing = false; cx.notify(); });
                    return;
                }
            }
        }));
        cx.notify();
    }

    fn replace_results(&mut self, results: Vec<(bool, PathBuf)>) {
        let mut previous = std::mem::take(&mut self.results);
        self.results = results.into_iter().map(|(is_dir, path)| {
            let visual = previous.iter_mut().find(|result| result.path == path && result.is_dir == is_dir)
                .and_then(|result| result.visual.take());
            QuickResult { path, is_dir, visual }
        }).collect();
        self.pending_image_drops.extend(previous.into_iter().filter_map(|result| result.visual));
        self.selection = 0;
        self.scroll.scroll_to_item(0);
    }

    pub fn toast(&self, cx: &App) -> Option<Toast> {
        let entity = self.owner.upgrade()?;
        let owner = entity.read(cx);
        (owner.toast_token != self.toast_token).then(|| owner.toast.clone()).flatten()
    }

    pub fn open_result(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.searching { return; }
        let Some(result) = self.results.get(row) else { return; };
        let path = result.path.clone();
        let owner = self.owner.clone();
        let main = self.main;
        self.close(window);
        cx.defer(move |cx| {
            let _ = main.update(cx, |_, window, cx| {
                let _ = owner.update(cx, |app, cx| app.open_quick_location(path, window, cx));
            });
        });
    }

    pub fn close(&mut self, window: &mut Window) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.task = None;
        window.remove_window();
    }

    pub fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => self.close(window),
            "up" if !self.searching && !self.results.is_empty() => {
                self.selection = self.selection.saturating_sub(1);
                self.scroll.scroll_to_item(self.selection);
                cx.notify();
            }
            "down" if !self.searching && !self.results.is_empty() => {
                self.selection = (self.selection + 1).min(self.results.len() - 1);
                self.scroll.scroll_to_item(self.selection);
                cx.notify();
            }
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Drop for QuickSearch {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }
}

impl Render for QuickSearch {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        for image in self.pending_image_drops.drain(..) {
            let _ = window.drop_image(image);
        }
        let colors = self.owner.upgrade()
            .map(|owner| theme::palette(&owner.read(cx).settings))
            .unwrap_or_else(|| theme::palette(&crate::settings::Settings::default()));
        // Windows can report its fallback viewport until the popup placement takes effect.
        // Keep the search card at its intended size even during that first frame.
        let intended = self.size();
        div().size_full().flex().justify_center().items_start().child(
            div().w(intended.width).h(intended.height).max_w_full().max_h_full()
                .child(crate::ui::quick_search_view(self, &colors, cx)),
        )
    }
}
