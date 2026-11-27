use std::{collections::BTreeSet, ops::Range, path::PathBuf};

use gpui::{Bounds, Context, MouseDownEvent, MouseMoveEvent, Pixels, Point, Window, point, px, size};

use crate::app::XoolFiles;

#[derive(Clone)]
pub(crate) struct Layout {
    pub viewport: Bounds<Pixels>,
    pub offset: Point<Pixels>,
    pub row_height: Pixels,
    pub rows: Range<usize>,
    pub columns: usize,
    pub gap: f32,
    pub grid_cell_width: Option<f32>,
}

impl Layout {
    fn cells(&self, total: usize) -> impl Iterator<Item = (usize, Bounds<Pixels>)> + '_ {
        let width = self.grid_cell_width.map(px)
            .unwrap_or((self.viewport.size.width - px(self.gap * (self.columns - 1) as f32)) / self.columns as f32);
        let height = self.row_height - px(if self.grid_cell_width.is_some() { self.gap } else { 0. });
        (self.rows.start * self.columns..(self.rows.end * self.columns).min(total)).map(move |index| {
            let row = index / self.columns;
            let column = index % self.columns;
            let origin = self.viewport.origin + self.offset
                + point((width + px(self.gap)) * column as f32, self.row_height * row as f32);
            (index, Bounds::new(origin, size(width, height)).intersect(&self.viewport))
        })
    }

    fn clamp(&self, position: Point<Pixels>) -> Point<Pixels> {
        point(position.x.clamp(self.viewport.origin.x, self.viewport.right()),
              position.y.clamp(self.viewport.origin.y, self.viewport.bottom()))
    }
}

pub(crate) struct Selection {
    pub tab_id: u64,
    start: Point<Pixels>,
    base: BTreeSet<PathBuf>,
    pub bounds: Option<Bounds<Pixels>>,
}

pub(crate) fn begin(app: &mut XoolFiles, tab_id: u64, event: &MouseDownEvent, window: &mut Window) {
    let Some(layout) = app.marquee_layouts.get(&tab_id) else { return; };
    let Some(tab) = app.tab_by_id(tab_id) else { return; };
    if layout.cells(tab.visible.len()).any(|(_, bounds)| bounds.contains(&event.position)) {
        return;
    }
    app.marquee = Some(Selection {
        tab_id,
        start: event.position,
        base: if event.modifiers.control { tab.selection.clone() } else { BTreeSet::new() },
        bounds: None,
    });
    app.focus_root(window);
}

pub(crate) fn update(app: &mut XoolFiles, event: &MouseMoveEvent, cx: &mut Context<XoolFiles>) {
    let Some(drag) = app.marquee.as_ref() else { return; };
    if !event.dragging() {
        finish(app, cx);
        return;
    }
    let Some(layout) = app.marquee_layouts.get(&drag.tab_id) else { return; };
    let Some(tab) = app.tab_by_id(drag.tab_id) else { return; };
    let delta = event.position - drag.start;
    if drag.bounds.is_none() && delta.x.abs().max(delta.y.abs()) < px(4.) {
        return;
    }
    let start = layout.clamp(drag.start);
    let end = layout.clamp(event.position);
    let bounds = Bounds::from_corners(point(start.x.min(end.x), start.y.min(end.y)),
                                      point(start.x.max(end.x), start.y.max(end.y)));
    let mut selected = drag.base.clone();
    let mut cursor = None;
    for (index, cell) in layout.cells(tab.visible.len()) {
        if cell.intersects(&bounds) {
            let path = &tab.entries[tab.visible[index]].path;
            selected.insert(path.clone());
            cursor = Some(path.clone());
        }
    }
    let tab_id = drag.tab_id;
    if let Some(drag) = &mut app.marquee { drag.bounds = Some(bounds); }
    if let Some(tab) = app.tab_by_id_mut(tab_id) {
        tab.selection = selected;
        tab.anchor = cursor.clone();
        tab.cursor = cursor;
    }
    cx.notify();
}

pub(crate) fn finish(app: &mut XoolFiles, cx: &mut Context<XoolFiles>) -> bool {
    if app.marquee.take().is_some_and(|drag| drag.bounds.is_some()) {
        app.refresh_preview(cx);
        cx.notify();
        true
    } else {
        false
    }
}
