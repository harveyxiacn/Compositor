//! The canvas: renders the visible part of the document at screen resolution, draws guides,
//! selection outlines and transform handles over it, and turns pointer input into tool actions.

use crate::app::{App, DragMode, Interaction, Tool, TransformDrag};
use crate::brush::{Stroke, StrokeError};
use crate::geom::{Point, Rect};
use crate::model::{GuideAxis, LayerTransform};
use crate::ops;
use crate::render::render_view;
use crate::selection::{shape_selection, Combine, Shape};
use eframe::egui::{self, Color32, CursorIcon, Pos2, Sense, Stroke as Line, Vec2};

/// Where the pointer is pressed relative to the active layer's transform box.
fn hit_test(t: &LayerTransform, to_screen: &impl Fn(Point) -> Pos2, pos: Pos2, handles: bool) -> DragMode {
    if handles {
        for (i, h) in LayerTransform::HANDLES.iter().enumerate() {
            if to_screen(t.point(*h)).distance(pos) <= 8.0 {
                return DragMode::Resize(i);
            }
        }
        let corners = [0usize, 2, 4, 6];
        for i in corners {
            let c = to_screen(t.point(LayerTransform::HANDLES[i]));
            if c.distance(pos) <= 30.0 {
                return DragMode::Rotate;
            }
        }
    }
    DragMode::Move
}

/// The transform after dragging from `start` to `point` — the macOS app's `TransformDrag.updated`.
fn dragged(original: &LayerTransform, start: Point, mode: DragMode, point: Point, lock_ratio: bool, shift: bool, option: bool) -> LayerTransform {
    let mut r = *original;
    match mode {
        DragMode::Move => {
            let (mut dx, mut dy) = (point.x - start.x, point.y - start.y);
            if shift {
                if dx.abs() >= dy.abs() {
                    dy = 0.0;
                } else {
                    dx = 0.0;
                }
            }
            r.origin.x += dx;
            r.origin.y += dy;
        }
        DragMode::Rotate => {
            let c = original.center();
            let delta = (point.y - c.y).atan2(point.x - c.x) - (start.y - c.y).atan2(start.x - c.x);
            r.rotation += delta.to_degrees();
            if shift {
                r.rotation = (r.rotation / 15.0).round() * 15.0;
            }
        }
        DragMode::Resize(index) => {
            let handle = LayerTransform::HANDLES[index];
            let anchor_unit = if option { Point::new(0.5, 0.5) } else { Point::new(1.0 - handle.x, 1.0 - handle.y) };
            let anchor = original.point(anchor_unit);
            let initial = original.point(handle);
            let dx = initial.x + point.x - start.x - anchor.x;
            let dy = initial.y + point.y - start.y - anchor.y;
            let span = if option { 2.0 } else { 1.0 };
            let (s, c) = original.radians().sin_cos();
            let local_x = (dx * c + dy * s) * span;
            let local_y = (-dx * s + dy * c) * span;
            let sx = handle.x * 2.0 - 1.0;
            let sy = handle.y * 2.0 - 1.0;
            let raw_w = if sx == 0.0 { original.size.0 } else { local_x * sx };
            let raw_h = if sy == 0.0 { original.size.1 } else { local_y * sy };
            let (mirror_x, mirror_y) = (raw_w < 0.0, raw_h < 0.0);
            let mut w = raw_w.abs().max(1.0);
            let mut h = raw_h.abs().max(1.0);
            if lock_ratio != shift {
                let (ow, oh) = original.size;
                let factor = if sx == 0.0 {
                    h / oh
                } else if sy == 0.0 {
                    w / ow
                } else {
                    (1.0 / ow.min(oh)).max((local_x * sx * ow + local_y * sy * oh) / (ow * ow + oh * oh))
                };
                w = ow * factor;
                h = oh * factor;
            }
            r.size = (w, h);
            if mirror_x {
                r.flip_x = !r.flip_x;
            }
            if mirror_y {
                r.flip_y = !r.flip_y;
            }
            let ox = (0.5 - anchor_unit.x) * w * if mirror_x { -1.0 } else { 1.0 };
            let oy = (0.5 - anchor_unit.y) * h * if mirror_y { -1.0 } else { 1.0 };
            let center = Point::new(anchor.x + ox * c - oy * s, anchor.y + ox * s + oy * c);
            r.origin = Point::new(center.x - w / 2.0, center.y - h / 2.0);
        }
    }
    if r.is_valid() {
        r
    } else {
        *original
    }
}

/// The smallest move that puts one of `guides` on one of `targets`, within `tolerance`.
fn snap(guides: [f64; 3], targets: &[f64], tolerance: f64) -> f64 {
    let mut best: Option<f64> = None;
    for g in guides {
        for t in targets {
            let m = t - g;
            if m.abs() <= tolerance && best.is_none_or(|b| m.abs() < b.abs()) {
                best = Some(m);
            }
        }
    }
    best.unwrap_or(0.0)
}

impl App {
    fn effective_tool(&self, ctx: &egui::Context) -> Tool {
        if !ctx.egui_wants_keyboard_input() && ctx.input(|i| i.key_down(egui::Key::Space)) {
            Tool::Hand
        } else {
            self.tool
        }
    }

    pub fn canvas_ui(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap();
        self.canvas_rect = rect;
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        if self.projects.is_empty() {
            painter.text(rect.center() - Vec2::new(0.0, 30.0), egui::Align2::CENTER_CENTER, "Compositor", egui::FontId::proportional(28.0), Color32::from_gray(170));
            painter.text(
                rect.center() + Vec2::new(0.0, 6.0),
                egui::Align2::CENTER_CENTER,
                "Ctrl+N new document · Ctrl+O open an image or project · drop files here",
                egui::FontId::proportional(14.0),
                Color32::from_gray(130),
            );
            return;
        }
        if self.checker.is_none() {
            let img = egui::ColorImage::from_rgba_unmultiplied(
                [2, 2],
                &[255, 255, 255, 255, 204, 204, 204, 255, 204, 204, 204, 255, 255, 255, 255, 255],
            );
            self.checker = Some(ctx.load_texture("checker", img, egui::TextureOptions::NEAREST_REPEAT));
        }
        let ppp = ctx.pixels_per_point();
        {
            let p = &mut self.projects[self.current];
            if p.view.needs_fit {
                let (w, h) = (p.doc.width as f32, p.doc.height as f32);
                let fit = ((rect.width() - 48.0) / w).min((rect.height() - 48.0) / h).max(0.01);
                p.view.zoom = fit.min(4.0);
                p.view.offset = (rect.size() - Vec2::new(w, h) * p.view.zoom) / 2.0;
                p.view.needs_fit = false;
            }
        }
        self.view_input(ctx, &response, rect);
        self.tool_input(ctx, &response, rect);
        self.draw_canvas(ctx, &painter, rect, ppp);
        self.draw_overlays(ctx, &painter, rect, response.hover_pos());
    }

    fn view_input(&mut self, ctx: &egui::Context, response: &egui::Response, rect: egui::Rect) {
        let (scroll, zoom, alt, middle_pressed, delta, any_down) = ctx.input(|i| {
            (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers.alt, i.pointer.button_pressed(egui::PointerButton::Middle), i.pointer.delta(), i.pointer.any_down())
        });
        let hover = response.hover_pos();
        if let Some(pos) = hover {
            if zoom != 1.0 {
                self.zoom_by(zoom, Some(pos), rect);
            } else if alt && scroll.y != 0.0 {
                self.zoom_by((1.0 + scroll.y * 0.003).clamp(0.5, 2.0), Some(pos), rect);
            } else if scroll != Vec2::ZERO {
                if let Some(p) = self.project() {
                    p.view.offset += scroll;
                }
            }
            if middle_pressed {
                if let Some(p) = self.project() {
                    p.interaction = Interaction::Pan;
                }
            }
        }
        if let Some(p) = self.project() {
            if matches!(p.interaction, Interaction::Pan) {
                if any_down {
                    p.view.offset += delta;
                } else {
                    p.interaction = Interaction::None;
                }
            }
        }
    }

    fn tool_input(&mut self, ctx: &egui::Context, response: &egui::Response, rect: egui::Rect) {
        let tool = self.effective_tool(ctx);
        let (pressed, down, pos, mods) = ctx.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.interact_pos(), i.modifiers));
        let Some(pos) = pos else { return };
        let (zoom, offset) = {
            let p = &self.projects[self.current];
            (p.view.zoom as f64, p.view.offset)
        };
        let to_doc = |s: Pos2| Point::new(((s.x - rect.min.x - offset.x) as f64) / zoom, ((s.y - rect.min.y - offset.y) as f64) / zoom);
        let to_screen = |d: Point| Pos2::new(rect.min.x + offset.x + (d.x * zoom) as f32, rect.min.y + offset.y + (d.y * zoom) as f32);
        let doc_pos = to_doc(pos);
        if response.hovered() {
            self.cursor_doc = Some(doc_pos);
            ctx.set_cursor_icon(match tool {
                Tool::Hand => {
                    if down {
                        CursorIcon::Grabbing
                    } else {
                        CursorIcon::Grab
                    }
                }
                Tool::Move => CursorIcon::Move,
                Tool::Zoom => CursorIcon::ZoomIn,
                Tool::Brush | Tool::Eraser => CursorIcon::None,
                _ => CursorIcon::Crosshair,
            });
        } else {
            self.cursor_doc = None;
        }

        let started = pressed && response.hovered();
        if started {
            match tool {
                Tool::Hand => self.projects[self.current].interaction = Interaction::Pan,
                Tool::Zoom => self.zoom_by(if mods.alt { 0.5 } else { 2.0 }, Some(pos), rect),
                Tool::Eyedropper => self.pick_color(doc_pos),
                Tool::Brush | Tool::Eraser if mods.alt => self.pick_color(doc_pos),
                Tool::Brush | Tool::Eraser => self.begin_paint(ctx, tool, doc_pos, mods.shift),
                Tool::Marquee | Tool::Ellipse => {
                    let mode = match (mods.shift, mods.alt) {
                        (true, true) => Combine::Intersect,
                        (true, false) => Combine::Add,
                        (false, true) => Combine::Subtract,
                        _ => Combine::Replace,
                    };
                    let shape = if tool == Tool::Ellipse { Shape::Ellipse } else { Shape::Rect };
                    self.projects[self.current].interaction = Interaction::Marquee { start: doc_pos, current: doc_pos, mode, shape };
                }
                Tool::Move => self.begin_transform(doc_pos, pos, &to_screen),
            }
            return;
        }

        let p = &mut self.projects[self.current];
        let interaction = std::mem::replace(&mut p.interaction, Interaction::None);
        p.interaction = match interaction {
            Interaction::Paint(mut stroke) => {
                if down {
                    if let Some(area) = stroke.add_point(&mut p.doc, doc_pos) {
                        p.touch(area);
                    }
                    Interaction::Paint(stroke)
                } else {
                    // The canvas already shows the finished stroke.
                    p.last_paint = Some(doc_pos);
                    p.thumbs.remove(&(stroke.layer, stroke.on_mask));
                    p.dirty = true;
                    Interaction::None
                }
            }
            Interaction::Marquee { start, mode, shape, .. } => {
                let mut current = doc_pos;
                if mods.shift && mode == Combine::Replace {
                    // Shift while dragging draws a square or circle.
                    let side = (current.x - start.x).abs().max((current.y - start.y).abs());
                    current = Point::new(start.x + side.copysign(current.x - start.x), start.y + side.copysign(current.y - start.y));
                }
                if down {
                    Interaction::Marquee { start, current, mode, shape }
                } else {
                    let r = Rect::from_points(start, current);
                    if r.w < 1.0 && r.h < 1.0 {
                        if mode == Combine::Replace && p.doc.selection.is_some() {
                            p.checkpoint("Deselect");
                            p.doc.selection = None;
                            p.changed();
                        }
                    } else {
                        let sel = shape_selection(p.doc.width, p.doc.height, shape, r, p.doc.selection.as_ref(), mode);
                        p.checkpoint(if shape == Shape::Ellipse { "Ellipse Marquee" } else { "Rectangle Marquee" });
                        p.doc.selection = sel;
                        p.changed();
                    }
                    Interaction::None
                }
            }
            Interaction::Transform(mut drag) => {
                let draft = dragged(&drag.original, drag.start, drag.mode, doc_pos, true, mods.shift, mods.alt);
                let mut draft = draft;
                if matches!(drag.mode, DragMode::Move) {
                    // Snap the box's edges and center to the canvas and the guides.
                    let b = draft.bounds();
                    let (w, h) = (p.doc.width as f64, p.doc.height as f64);
                    let mut xs = vec![0.0, w / 2.0, w];
                    let mut ys = vec![0.0, h / 2.0, h];
                    for g in &p.doc.guides {
                        match g.axis {
                            GuideAxis::Vertical => xs.push(g.position),
                            GuideAxis::Horizontal => ys.push(g.position),
                        }
                    }
                    let tolerance = 8.0 / zoom;
                    if !mods.ctrl {
                        draft.origin.x += snap([b.x, b.x + b.w / 2.0, b.max_x()], &xs, tolerance);
                        draft.origin.y += snap([b.y, b.y + b.h / 2.0, b.max_y()], &ys, tolerance);
                    }
                }
                let finished = !down;
                if finished {
                    draft = draft.rounded();
                }
                if draft != drag.original || drag.moved {
                    if !drag.moved {
                        let label = match drag.mode {
                            DragMode::Move => "Move",
                            DragMode::Resize(_) => "Scale",
                            DragMode::Rotate => "Rotate",
                        };
                        p.checkpoint(label);
                        drag.moved = true;
                    }
                    let (dx, dy) = (draft.origin.x - drag.original.origin.x, draft.origin.y - drag.original.origin.y);
                    let single = drag.layers.len() == 1 && !p.doc.layer(drag.layers[0].0).is_some_and(|l| l.is_group);
                    for (id, t, m) in &drag.layers {
                        if let Some(layer) = p.doc.layer_mut(*id) {
                            let to = if single {
                                draft
                            } else {
                                let mut moved = *t;
                                moved.origin.x += dx;
                                moved.origin.y += dy;
                                moved
                            };
                            ops::set_transform(layer, t, *m, to);
                        }
                    }
                    p.revision += 1;
                    if finished {
                        p.changed();
                    }
                }
                if finished {
                    Interaction::None
                } else {
                    Interaction::Transform(drag)
                }
            }
            other => other,
        };
    }

    fn pick_color(&mut self, at: Point) {
        let p = &self.projects[self.current];
        if let Some(c) = ops::sample_color(&p.doc, at) {
            if c[3] > 0 {
                self.fg = [c[0], c[1], c[2]];
            }
        }
    }

    fn begin_paint(&mut self, ctx: &egui::Context, tool: Tool, at: Point, straight: bool) {
        let erase = tool == Tool::Eraser;
        let settings = if erase { self.eraser } else { self.brush };
        let color = self.fg;
        let p = &mut self.projects[self.current];
        let Some(id) = p.doc.active else { return };
        let before = p.doc.clone();
        match Stroke::begin(&mut p.doc, id, p.on_mask, settings, color, erase) {
            Ok(mut stroke) => {
                p.history.record(if erase { "Eraser" } else { "Brush" }, &before);
                p.edit_label = None;
                p.changed();
                if let (true, Some(last)) = (straight, p.last_paint) {
                    if let Some(area) = stroke.add_point(&mut p.doc, last) {
                        p.touch(area);
                    }
                }
                if let Some(area) = stroke.add_point(&mut p.doc, at) {
                    p.touch(area);
                }
                p.interaction = Interaction::Paint(stroke);
            }
            Err(StrokeError::NotPaintable(msg)) => {
                p.doc = before;
                self.say(ctx, msg);
            }
        }
    }

    /// The box a transform edits: the layer's own, or for a folder the box around its contents.
    fn transform_box(&self) -> Option<(LayerTransform, Vec<uuid::Uuid>, bool)> {
        let p = self.project_ref()?;
        let layer = p.doc.active_layer()?;
        if !layer.is_group {
            return Some((layer.transform, vec![layer.id], true));
        }
        let mut ids = p.doc.descendants(layer.id);
        let mut r = Rect::default();
        for id in &ids {
            if let Some(l) = p.doc.layer(*id) {
                if !l.is_group {
                    r = r.union(&l.transform.bounds());
                }
            }
        }
        if r.is_empty() {
            r = layer.transform.bounds();
        }
        ids.push(layer.id);
        Some((LayerTransform::new(Point::new(r.x, r.y), r.w.max(1.0), r.h.max(1.0)), ids, false))
    }

    fn begin_transform(&mut self, doc_pos: Point, screen: Pos2, to_screen: &impl Fn(Point) -> Pos2) {
        let Some((original, ids, handles)) = self.transform_box() else { return };
        let mode = hit_test(&original, to_screen, screen, handles);
        let p = &mut self.projects[self.current];
        let layers = ids
            .iter()
            .filter_map(|id| p.doc.layer(*id).map(|l| (l.id, l.transform, l.mask.as_ref().and_then(|m| m.placement))))
            .collect();
        p.interaction = Interaction::Transform(TransformDrag { layers, original, start: doc_pos, mode, moved: false });
    }

    fn draw_canvas(&mut self, ctx: &egui::Context, painter: &egui::Painter, rect: egui::Rect, ppp: f32) {
        let checker = self.checker.as_ref().map(|t| t.id());
        let p = &mut self.projects[self.current];
        let s = (p.view.zoom * ppp) as f64;
        // Keep the document's corner on a whole device pixel so 100% stays sharp.
        let o = ((rect.min.to_vec2() + p.view.offset) * ppp).round();
        p.view.offset = o / ppp - rect.min.to_vec2();
        let (dw, dh) = (p.doc.width as f64 * s, p.doc.height as f64 * s);
        let clip = (rect.min.to_vec2() * ppp, rect.max.to_vec2() * ppp);
        let x0 = ((clip.0.x - o.x) as f64).max(0.0).floor();
        let y0 = ((clip.0.y - o.y) as f64).max(0.0).floor();
        let x1 = ((clip.1.x - o.x) as f64).min(dw).ceil();
        let y1 = ((clip.1.y - o.y) as f64).min(dh).ceil();
        let doc_screen = egui::Rect::from_min_size(Pos2::new(o.x / ppp, o.y / ppp), Vec2::new((dw / ppp as f64) as f32, (dh / ppp as f64) as f32));
        if let Some(id) = checker {
            let area = doc_screen.intersect(rect);
            let uv = egui::Rect::from_min_max(
                Pos2::new((area.min.x - doc_screen.min.x) / 16.0, (area.min.y - doc_screen.min.y) / 16.0),
                Pos2::new((area.max.x - doc_screen.min.x) / 16.0, (area.max.y - doc_screen.min.y) / 16.0),
            );
            painter.image(id, area, uv, Color32::WHITE);
        }
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let geometry = (x0 as i64, y0 as i64, w, h, s.to_bits());
        if p.canvas.geometry != Some(geometry) || p.canvas.revision != p.revision || p.canvas.texture.is_none() {
            // A brush stroke since the last frame: redraw only what it touched.
            let partial = match (p.damage, p.canvas.geometry == Some(geometry) && p.canvas.texture.is_some()) {
                (Some((from, to, r)), true) if from == p.canvas.revision && to == p.revision => Some(r),
                _ => None,
            };
            match partial {
                Some(r) => {
                    let px0 = ((r.x * s).floor() - 2.0).max(x0);
                    let py0 = ((r.y * s).floor() - 2.0).max(y0);
                    let px1 = ((r.max_x() * s).ceil() + 2.0).min(x1);
                    let py1 = ((r.max_y() * s).ceil() + 2.0).min(y1);
                    if px1 > px0 && py1 > py0 {
                        let (pw, ph) = ((px1 - px0) as usize, (py1 - py0) as usize);
                        let view = Rect::new(px0 / s, py0 / s, pw as f64 / s, ph as f64 / s);
                        let pixels = render_view(&p.doc, view, s, (px0 as i64, py0 as i64), (pw, ph));
                        let image = egui::ColorImage::from_rgba_premultiplied([pw, ph], &pixels.data);
                        if let Some(t) = &mut p.canvas.texture {
                            t.set_partial([(px0 - x0) as usize, (py0 - y0) as usize], image, egui::TextureOptions::NEAREST);
                        }
                    }
                }
                None => {
                    let view = Rect::new(x0 / s, y0 / s, w as f64 / s, h as f64 / s);
                    let pixels = render_view(&p.doc, view, s, (x0 as i64, y0 as i64), (w, h));
                    let image = egui::ColorImage::from_rgba_premultiplied([w, h], &pixels.data);
                    match &mut p.canvas.texture {
                        Some(t) => t.set(image, egui::TextureOptions::NEAREST),
                        None => p.canvas.texture = Some(ctx.load_texture("canvas", image, egui::TextureOptions::NEAREST)),
                    }
                }
            }
            p.canvas.geometry = Some(geometry);
            p.canvas.revision = p.revision;
            p.damage = None;
        }
        if let Some(t) = &p.canvas.texture {
            let at = egui::Rect::from_min_size(
                Pos2::new((o.x + x0 as f32) / ppp, (o.y + y0 as f32) / ppp),
                Vec2::new(w as f32 / ppp, h as f32 / ppp),
            );
            painter.image(t.id(), at, egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        }
        painter.rect_stroke(doc_screen, 0.0, Line::new(1.0, Color32::from_black_alpha(160)), egui::StrokeKind::Outside);
    }

    fn draw_overlays(&mut self, ctx: &egui::Context, painter: &egui::Painter, rect: egui::Rect, hover: Option<Pos2>) {
        let tool = self.effective_tool(ctx);
        let brush_size = if self.tool == Tool::Eraser { self.eraser.size } else { self.brush.size };
        let transform_box = if tool == Tool::Move { self.transform_box() } else { None };
        let time = ctx.input(|i| i.time);
        let p = &self.projects[self.current];
        let zoom = p.view.zoom as f64;
        let offset = p.view.offset;
        let to_screen = |d: Point| Pos2::new(rect.min.x + offset.x + (d.x * zoom) as f32, rect.min.y + offset.y + (d.y * zoom) as f32);

        for g in &p.doc.guides {
            let color = Color32::from_rgb(0, 200, 230);
            match g.axis {
                GuideAxis::Vertical => {
                    let x = to_screen(Point::new(g.position, 0.0)).x;
                    painter.line_segment([Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)], Line::new(1.0, color));
                }
                GuideAxis::Horizontal => {
                    let y = to_screen(Point::new(0.0, g.position)).y;
                    painter.line_segment([Pos2::new(rect.min.x, y), Pos2::new(rect.max.x, y)], Line::new(1.0, color));
                }
            }
        }

        // Marching ants.
        if let Some(sel) = &p.doc.selection {
            let phase = ((time * 8.0) as i64) & 1;
            for (a, b) in sel.outline.iter() {
                let (sa, sb) = (to_screen(*a), to_screen(*b));
                if !rect.expand(2.0).contains(sa) && !rect.expand(2.0).contains(sb) {
                    continue;
                }
                let dark = ((((a.x + a.y) as i64 / 4) + phase) & 1) == 0;
                painter.line_segment([sa, sb], Line::new(1.0, if dark { Color32::BLACK } else { Color32::WHITE }));
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(130));
        }

        if let Interaction::Marquee { start, current, shape, .. } = &p.interaction {
            let r = egui::Rect::from_two_pos(to_screen(*start), to_screen(*current));
            let line = Line::new(1.0, Color32::WHITE);
            match shape {
                Shape::Rect => {
                    painter.rect_stroke(r, 0.0, Line::new(3.0, Color32::from_black_alpha(120)), egui::StrokeKind::Middle);
                    painter.rect_stroke(r, 0.0, line, egui::StrokeKind::Middle);
                }
                Shape::Ellipse => {
                    let points: Vec<Pos2> = (0..96)
                        .map(|i| {
                            let a = i as f32 / 96.0 * std::f32::consts::TAU;
                            Pos2::new(r.center().x + r.width() / 2.0 * a.cos(), r.center().y + r.height() / 2.0 * a.sin())
                        })
                        .collect();
                    painter.add(egui::Shape::closed_line(points.clone(), Line::new(3.0, Color32::from_black_alpha(120))));
                    painter.add(egui::Shape::closed_line(points, line));
                }
            }
        }

        if let Some((t, _, handles)) = transform_box {
            let corners: Vec<Pos2> = t.corners().iter().map(|c| to_screen(*c)).collect();
            painter.add(egui::Shape::closed_line(corners, Line::new(1.0, Color32::from_rgb(70, 150, 255))));
            if handles {
                for h in LayerTransform::HANDLES {
                    let c = to_screen(t.point(h));
                    let r = egui::Rect::from_center_size(c, Vec2::splat(7.0));
                    painter.rect_filled(r, 0.0, Color32::WHITE);
                    painter.rect_stroke(r, 0.0, Line::new(1.0, Color32::from_rgb(70, 150, 255)), egui::StrokeKind::Inside);
                }
            }
        }

        if matches!(tool, Tool::Brush | Tool::Eraser) {
            if let Some(h) = hover {
                let radius = (brush_size * zoom / 2.0) as f32;
                if radius >= 3.0 {
                    painter.circle_stroke(h, radius, Line::new(2.5, Color32::from_black_alpha(140)));
                    painter.circle_stroke(h, radius, Line::new(1.0, Color32::WHITE));
                } else {
                    for (a, b) in [(Vec2::new(-5.0, 0.0), Vec2::new(5.0, 0.0)), (Vec2::new(0.0, -5.0), Vec2::new(0.0, 5.0))] {
                        painter.line_segment([h + a, h + b], Line::new(1.0, Color32::WHITE));
                    }
                }
            }
        }
    }

    pub fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.vertical_centered(|ui| {
            for tool in Tool::ALL {
                let selected = self.tool == tool;
                let button = egui::Button::new(egui::RichText::new(tool.icon()).size(18.0)).selected(selected).min_size(Vec2::splat(32.0));
                if ui.add(button).on_hover_text(tool.label()).clicked() {
                    self.tool = tool;
                }
            }
            ui.add_space(12.0);
            ui.color_edit_button_srgb(&mut self.fg).on_hover_text("Foreground color");
            ui.color_edit_button_srgb(&mut self.bg).on_hover_text("Background color");
            if ui.small_button("⇄").on_hover_text("Swap colors (X)").clicked() {
                std::mem::swap(&mut self.fg, &mut self.bg);
            }
            if ui.small_button("◐").on_hover_text("Default colors (D)").clicked() {
                self.fg = [0, 0, 0];
                self.bg = [255, 255, 255];
            }
        });
    }

    pub fn options_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(self.tool.label()).strong());
            ui.separator();
            match self.tool {
                Tool::Brush | Tool::Eraser => {
                    let b = if self.tool == Tool::Eraser { &mut self.eraser } else { &mut self.brush };
                    ui.label("Size");
                    ui.add(egui::Slider::new(&mut b.size, 1.0..=2000.0).logarithmic(true).suffix(" px").max_decimals(0));
                    ui.label("Hardness");
                    let mut hardness = b.hardness * 100.0;
                    if ui.add(egui::Slider::new(&mut hardness, 0.0..=100.0).suffix("%").max_decimals(0)).changed() {
                        b.hardness = hardness / 100.0;
                    }
                    ui.label("Opacity");
                    let mut opacity = b.opacity * 100.0;
                    if ui.add(egui::Slider::new(&mut opacity, 1.0..=100.0).suffix("%").max_decimals(0)).changed() {
                        b.opacity = opacity / 100.0;
                    }
                    ui.label(egui::RichText::new("[ ] size · Shift-click: straight line · Alt-click: pick color").weak());
                }
                Tool::Marquee | Tool::Ellipse => {
                    ui.label(egui::RichText::new("Shift: add · Alt: subtract · Shift+Alt: intersect · Shift while dragging: square · click: deselect").weak());
                }
                Tool::Move => {
                    ui.label(egui::RichText::new("Drag to move (snaps to canvas and guides; hold Ctrl to not snap) · handles scale (Shift: free, Alt: from center) · just outside a corner rotates · arrow keys nudge").weak());
                }
                Tool::Eyedropper => {
                    ui.label(egui::RichText::new("Click to pick the foreground color from the image").weak());
                }
                Tool::Hand => {
                    ui.label(egui::RichText::new("Drag to pan · scroll pans · Ctrl+scroll or Alt+scroll zooms").weak());
                }
                Tool::Zoom => {
                    ui.label(egui::RichText::new("Click to zoom in · Alt-click to zoom out").weak());
                }
            }
            if let Some(p) = self.project() {
                let has_mask = p.doc.active_layer().is_some_and(|l| l.mask.is_some());
                if has_mask {
                    ui.separator();
                    ui.label("Target:");
                    ui.selectable_value(&mut p.on_mask, false, "Pixels");
                    ui.selectable_value(&mut p.on_mask, true, "Mask");
                }
            }
        });
    }

    pub fn status_bar(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let now = ctx.input(|i| i.time);
        let ppp = ctx.pixels_per_point();
        ui.horizontal(|ui| {
            if let Some(p) = self.project_ref() {
                ui.label(format!("{:.0}%", p.view.zoom * ppp * 100.0));
                ui.separator();
                ui.label(format!("{} × {} px · {:.0} ppi", p.doc.width, p.doc.height, p.doc.resolution));
                if let Some(c) = self.cursor_doc {
                    ui.separator();
                    ui.label(format!("x {:.0}  y {:.0}", c.x.floor(), c.y.floor()));
                }
                if p.on_mask {
                    ui.separator();
                    ui.colored_label(Color32::from_rgb(240, 180, 80), "Editing mask");
                }
            }
            if let Some((msg, at)) = &self.status {
                if now - at < 6.0 {
                    ui.separator();
                    ui.label(msg);
                    ctx.request_repaint_after(std::time::Duration::from_secs(1));
                }
            }
        });
    }
}
