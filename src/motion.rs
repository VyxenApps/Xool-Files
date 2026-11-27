use std::{cell::Cell, rc::Rc, time::{Duration, Instant}};

use gpui::{
    App, BorderStyle, Bounds, Corners, Edges, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Position, Rgba, Size, Style, Window,
    canvas, px, quad, relative, rgba,
};

type Selection = Rc<Cell<Option<Bounds<Pixels>>>>;

pub(crate) fn selection() -> Selection { Rc::new(Cell::new(None)) }

pub(crate) fn measure(target: Selection) -> gpui::Canvas<()> {
    canvas(move |bounds, _, _| target.set(Some(bounds)), |_, _, _, _| {})
}

pub(crate) struct VisibilityMotion {
    pub(crate) progress: f32,
    target: bool,
    transition: Option<(Instant, f32)>,
}

impl VisibilityMotion {
    pub(crate) fn new(target: bool) -> Self {
        Self { progress: if target { 1. } else { 0. }, target, transition: None }
    }

    pub(crate) fn tick(&mut self, target: bool, now: Instant, duration: Duration) -> bool {
        if let Some((start, initial)) = self.transition {
            let elapsed = (now.duration_since(start).as_secs_f32() / duration.as_secs_f32()).min(1.);
            let eased = 1. - (1. - elapsed).powi(3);
            self.progress = initial + (f32::from(u8::from(self.target)) - initial) * eased;
            if elapsed == 1. { self.transition = None; }
        }
        if self.target != target {
            self.target = target;
            self.transition = Some((now, self.progress));
        }
        self.transition.is_some()
    }
}

pub(crate) struct SettingsReveal {
    motion: VisibilityMotion,
    pub(crate) bounds: Selection,
}

impl SettingsReveal {
    pub(crate) fn new(visible: bool) -> Self {
        Self { motion: VisibilityMotion::new(visible), bounds: selection() }
    }

    pub(crate) fn tick(&mut self, visible: bool, now: Instant) -> bool {
        self.motion.tick(visible, now, Duration::from_millis(180))
    }

    pub(crate) fn progress(&self) -> f32 {
        self.motion.progress
    }
}

pub(crate) struct SlidingFill {
    id: ElementId,
    target: Selection,
    color: Rgba,
    radius: Pixels,
}

impl SlidingFill {
    pub(crate) fn new(id: impl Into<ElementId>, target: Selection, color: Rgba, radius: Pixels) -> Self {
        Self { id: id.into(), target, color, radius }
    }
}

struct Motion {
    from: Bounds<Pixels>,
    to: Bounds<Pixels>,
    started: Instant,
}

impl Motion {
    fn position(&self, now: Instant) -> (Bounds<Pixels>, bool) {
        let progress = (now.duration_since(self.started).as_secs_f32() / 0.16).min(1.);
        let t = 1. - (1. - progress).powi(3);
        let mix = |a: Pixels, b: Pixels| a + (b - a) * t;
        (Bounds::new(
            self.from.origin + (self.to.origin - self.from.origin) * t,
            gpui::size(mix(self.from.size.width, self.to.size.width),
                mix(self.from.size.height, self.to.size.height)),
        ), progress < 1.)
    }
}

impl IntoElement for SlidingFill {
    type Element = Self;
    fn into_element(self) -> Self { self }
}

impl Element for SlidingFill {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> { Some(self.id.clone()) }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> { None }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>,
        window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let full = relative(1.).into();
        let style = Style {
            position: Position::Absolute,
            inset: Edges { left: px(0.).into(), top: px(0.).into(), ..Default::default() },
            size: Size { width: full, height: full },
            ..Default::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>,
        _: Bounds<Pixels>, _: &mut (), _: &mut Window, _: &mut App) {}

    fn paint(&mut self, id: Option<&GlobalElementId>, _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, _: &mut App) {
        window.with_element_state(id.expect("sliding fill id"), |state: Option<Option<Motion>>, window| {
            let next = self.target.get().map(|target| {
                let target = target - bounds.origin;
                let now = Instant::now();
                let motion = match state.flatten() {
                    Some(motion) if motion.to == target => motion,
                    Some(motion) => Motion { from: motion.position(now).0, to: target, started: now },
                    None => Motion { from: target, to: target, started: now },
                };
                let (position, moving) = motion.position(now);
                if moving && motion.from != motion.to { window.request_animation_frame(); }
                window.paint_quad(quad(position + bounds.origin, Corners::all(self.radius), self.color,
                    Edges::all(px(0.)), rgba(0x00000000), BorderStyle::Solid));
                motion
            });
            ((), next)
        });
    }
}

