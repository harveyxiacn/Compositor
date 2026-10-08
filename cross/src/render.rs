//! Flattens a document the way the macOS app does: folders are pass-through, a folder's opacity and
//! mask apply to each layer inside it, clipped layers form stacks that blend in their base's mode,
//! and an adjustment layer changes everything drawn below it.

use crate::adjust::Placement;
use crate::blend::composite;
use crate::geom::{Affine, IRect, Point, Rect};
use crate::model::{BlendMode, Document, LayerTransform, Sampling};
use crate::raster::{sample_gray, sample_rgba, sample_rgba_nearest, MaskAsset, Rgba};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use uuid::Uuid;

/// The pixels being rendered: `w × h` output pixels, mapped onto the document by `to_doc`
/// (a scale and an offset).
#[derive(Clone, Copy, Debug)]
pub struct Target {
    pub w: usize,
    pub h: usize,
    pub origin: Point,
    /// Output pixels per document pixel.
    pub scale: f64,
}

impl Target {
    pub fn full(doc: &Document) -> Self {
        Self { w: doc.width, h: doc.height, origin: Point::default(), scale: 1.0 }
    }
    pub fn to_doc(&self) -> Affine {
        Affine::translation(self.origin.x, self.origin.y).after(&Affine::scale(1.0 / self.scale, 1.0 / self.scale))
    }
    pub fn from_doc(&self) -> Affine {
        Affine::scale(self.scale, self.scale).after(&Affine::translation(-self.origin.x, -self.origin.y))
    }
    fn placement(&self) -> Placement {
        Placement { origin_x: self.origin.x, origin_y: self.origin.y, scale: self.scale }
    }
}

type Coverage = Arc<Vec<u8>>;

struct Renderer<'a> {
    doc: &'a Document,
    t: Target,
    index: HashMap<Uuid, usize>,
    coverage: HashMap<Uuid, Option<Coverage>>,
    visiting: HashSet<Uuid>,
    stacks: HashMap<Uuid, Vec<Uuid>>,
    stacked: HashSet<Uuid>,
    folder_clips: HashMap<Uuid, Option<Coverage>>,
    /// Layers drawn with their own pixels swapped for these (a brush stroke in progress uses the doc directly).
    hidden: Option<Uuid>,
}

/// Renders the document into a fresh premultiplied buffer.
pub fn render(doc: &Document, t: Target) -> Rgba {
    render_skipping(doc, t, None)
}

/// As `render`, leaving one layer out (used to show a mask alone or preview edits).
pub fn render_skipping(doc: &Document, t: Target, hidden: Option<Uuid>) -> Rgba {
    let mut canvas = Rgba::new(t.w, t.h);
    if t.w == 0 || t.h == 0 {
        return canvas;
    }
    let mut r = Renderer {
        doc,
        t,
        index: doc.layers.iter().enumerate().map(|(i, l)| (l.id, i)).collect(),
        coverage: HashMap::new(),
        visiting: HashSet::new(),
        stacks: HashMap::new(),
        stacked: HashSet::new(),
        folder_clips: HashMap::new(),
        hidden,
    };
    let ids: Vec<Uuid> = doc
        .visible_layers()
        .into_iter()
        .map(|i| doc.layers[i].id)
        .filter(|id| Some(*id) != hidden)
        .collect();
    r.prepare_stacks(&ids);
    for id in &ids {
        let clip = r.parent_clip(*id);
        r.draw_composite(*id, &mut canvas, clip.as_deref().map(|v| v.as_slice()));
    }
    canvas
}

impl<'a> Renderer<'a> {
    fn layer(&self, id: Uuid) -> Option<&'a crate::model::Layer> {
        self.index.get(&id).map(|i| &self.doc.layers[*i])
    }

    fn prepare_stacks(&mut self, ids: &[Uuid]) {
        for (i, base) in ids.iter().enumerate() {
            let Some(b) = self.layer(*base) else { continue };
            if b.mask_source.is_some() || b.adjustment.is_some() {
                continue;
            }
            let mut children = Vec::new();
            for child in &ids[i + 1..] {
                let Some(c) = self.layer(*child) else { break };
                if c.mask_source != Some(*base) || c.parent != b.parent {
                    break;
                }
                children.push(*child);
            }
            if !children.is_empty() {
                self.stacked.extend(children.iter().copied());
                self.stacks.insert(*base, children);
            }
        }
    }

    /// The combined mask of every folder holding `id`, or nil when none of them has one.
    fn parent_clip(&mut self, id: Uuid) -> Option<Coverage> {
        let parent = self.layer(id)?.parent?;
        self.folder_clip(parent, 0)
    }

    fn folder_clip(&mut self, folder: Uuid, depth: usize) -> Option<Coverage> {
        if let Some(c) = self.folder_clips.get(&folder) {
            return c.clone();
        }
        let f = self.layer(folder)?;
        let outer = match (f.parent, depth < 64) {
            (Some(p), true) => self.folder_clip(p, depth + 1),
            _ => None,
        };
        let own = f
            .mask
            .as_ref()
            .filter(|m| m.enabled)
            .map(|m| self.mask_coverage(&m.asset, m.placement.as_ref().unwrap_or(&f.transform)));
        let combined = match (own, outer) {
            (None, o) => o,
            (Some(a), None) => Some(Arc::new(a)),
            (Some(mut a), Some(b)) => {
                a.par_iter_mut().zip(b.par_iter()).for_each(|(x, y)| *x = mul8(*x, *y));
                Some(Arc::new(a))
            }
        };
        self.folder_clips.insert(folder, combined.clone());
        combined
    }

    /// A mask stretched over `rect` in target pixels; nothing shows outside the rectangle.
    fn mask_coverage(&self, mask: &MaskAsset, rect: &LayerTransform) -> Vec<u8> {
        let t = self.t;
        let (mw, mh) = (mask.image.w, mask.image.h);
        let mut out = vec![0u8; t.w * t.h];
        let Some(doc_to_mask) = rect.pixel_to_document(mw, mh).inverted() else { return out };
        let out_to_mask = doc_to_mask.after(&t.to_doc());
        let factor = out_to_mask.mean_scale();
        let (level, sx, sy) = mask.level(factor);
        let bbox = IRect::covering(&t.from_doc().map_rect(&rect.bounds())).clamp(t.w as i64, t.h as i64);
        out.par_chunks_mut(t.w).enumerate().for_each(|(y, row)| {
            if (y as i64) < bbox.y0 || (y as i64) >= bbox.y1 {
                return;
            }
            for x in bbox.x0 as usize..bbox.x1 as usize {
                let p = out_to_mask.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5));
                if p.x < 0.0 || p.y < 0.0 || p.x > mw as f64 || p.y > mh as f64 {
                    continue;
                }
                row[x] = (sample_gray(level, p.x * sx, p.y * sy) * 255.0).round() as u8;
            }
        });
        out
    }

    fn draw_composite(&mut self, id: Uuid, canvas: &mut Rgba, clip: Option<&[u8]>) {
        if self.stacked.contains(&id) {
            return;
        }
        let Some(layer) = self.layer(id) else { return };
        if layer.adjustment.is_some() {
            if layer.mask_source.is_none() {
                self.adjust(id, canvas, clip);
            }
            return;
        }
        let Some(children) = self.stacks.get(&id).cloned() else {
            self.draw(id, canvas, clip);
            return;
        };
        let mut group = Rgba::new(canvas.w, canvas.h);
        self.draw_own(id, &mut group, None);
        let alpha: Vec<u8> = group.data.par_chunks(4).map(|p| p[3]).collect();
        group.data.par_chunks_mut(4).for_each(|p| {
            let a = p[3] as u32;
            if a == 0 {
                p[..3].fill(0);
            } else if a < 255 {
                for c in &mut p[..3] {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
            p[3] = 255;
        });
        for child in children {
            if Some(child) == self.hidden {
                continue;
            }
            let Some(c) = self.layer(child) else { continue };
            if c.adjustment.is_some() {
                self.adjust(child, &mut group, None);
            } else {
                self.draw_own(child, &mut group, None);
            }
        }
        group.data.par_chunks_mut(4).zip(alpha.par_iter()).for_each(|(p, a)| {
            let a = *a as u32;
            for c in &mut p[..3] {
                *c = ((*c as u32 * a + 127) / 255) as u8;
            }
            p[3] = a as u8;
        });
        let mode = layer.blend;
        let w = canvas.w;
        canvas.data.par_chunks_mut(w * 4).zip(group.data.par_chunks(w * 4)).enumerate().for_each(|(y, (dst, src))| {
            for x in 0..w {
                let s = &src[x * 4..x * 4 + 4];
                if s[3] == 0 {
                    continue;
                }
                let k = clip.map_or(1.0, |c| c[y * w + x] as f32 / 255.0);
                if k <= 0.0 {
                    continue;
                }
                let px = [s[0] as f32 * k, s[1] as f32 * k, s[2] as f32 * k, s[3] as f32 * k];
                composite(mode, &mut dst[x * 4..x * 4 + 4], px);
            }
        });
    }

    /// A layer clipped by its clipping base's coverage, if it has one.
    fn draw(&mut self, id: Uuid, dest: &mut Rgba, clip: Option<&[u8]>) {
        let Some(layer) = self.layer(id) else { return };
        let live = match layer.mask_source {
            Some(source) => match self.coverage(source) {
                Some(c) => Some(c),
                None => return,
            },
            None => None,
        };
        match (live, clip) {
            (None, c) => self.draw_own(id, dest, c),
            (Some(l), None) => self.draw_own(id, dest, Some(l.as_slice())),
            (Some(l), Some(c)) => {
                let both: Vec<u8> = l.par_iter().zip(c.par_iter()).map(|(a, b)| mul8(*a, *b)).collect();
                self.draw_own(id, dest, Some(both.as_slice()));
            }
        }
    }

    /// The alpha a layer draws with (its own clipping included), whether or not it is visible.
    fn coverage(&mut self, id: Uuid) -> Option<Coverage> {
        if let Some(c) = self.coverage.get(&id) {
            return c.clone();
        }
        if self.visiting.contains(&id) || self.visiting.len() >= 256 {
            return None;
        }
        self.visiting.insert(id);
        let mut temp = Rgba::new(self.t.w, self.t.h);
        self.draw(id, &mut temp, None);
        self.visiting.remove(&id);
        let alpha: Coverage = Arc::new(temp.data.par_chunks(4).map(|p| p[3]).collect());
        self.coverage.insert(id, Some(alpha.clone()));
        Some(alpha)
    }

    /// Draws one layer's pixels, mask and opacity in its blend mode, scaled by `extra` coverage.
    fn draw_own(&self, id: Uuid, dest: &mut Rgba, extra: Option<&[u8]>) {
        let Some(&index) = self.index.get(&id) else { return };
        let layer = &self.doc.layers[index];
        let Some(asset) = layer.pixels.as_ref() else { return };
        let opacity = self.doc.effective_opacity(index) as f32;
        if opacity <= 0.0 {
            return;
        }
        let t = self.t;
        let img = &asset.image;
        let Some(doc_to_px) = layer.transform.pixel_to_document(img.w, img.h).inverted() else { return };
        let out_to_px = doc_to_px.after(&t.to_doc());
        let factor = out_to_px.mean_scale();
        let nearest = layer.transform.sampling == Sampling::Nearest;
        let (level, level_scale) = if nearest { (img, 1.0) } else { asset.level(factor) };
        // Upright and drawn pixel for pixel: copy straight across rather than filtering.
        let exact = (out_to_px.a - 1.0).abs() < 1e-9
            && (out_to_px.d - 1.0).abs() < 1e-9
            && out_to_px.b.abs() < 1e-12
            && out_to_px.c.abs() < 1e-12
            && (out_to_px.tx - out_to_px.tx.round()).abs() < 1e-6
            && (out_to_px.ty - out_to_px.ty.round()).abs() < 1e-6;

        let mask = layer.mask.as_ref().filter(|m| m.enabled);
        let mask_map = mask.map(|m| {
            let (mw, mh) = (m.asset.image.w as f64, m.asset.image.h as f64);
            match m.placement.filter(|p| !p.same_placement(&layer.transform)) {
                None => {
                    let s = Affine::scale(mw / img.w as f64, mh / img.h as f64).after(&out_to_px);
                    (s, None)
                }
                Some(p) => {
                    let inv = p.pixel_to_document(m.asset.image.w, m.asset.image.h).inverted().unwrap_or_default();
                    (inv.after(&t.to_doc()), Some(m.asset.background()))
                }
            }
        });
        let mask_level = mask.zip(mask_map.as_ref()).map(|(m, (map, _))| m.asset.level(map.mean_scale()));

        let bbox = IRect::covering(&t.from_doc().map_rect(&layer.transform.bounds())).clamp(t.w as i64, t.h as i64);
        if bbox.is_empty() {
            return;
        }
        let (iw, ih) = (img.w as f64, img.h as f64);
        let mode = layer.blend;
        let w = dest.w;
        dest.data.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
            let yi = y as i64;
            if yi < bbox.y0 || yi >= bbox.y1 {
                return;
            }
            for x in bbox.x0 as usize..bbox.x1 as usize {
                let k_extra = extra.map_or(1.0, |e| e[y * w + x] as f32 / 255.0);
                if k_extra <= 0.0 {
                    continue;
                }
                let p = out_to_px.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5));
                if p.x < -1.0 || p.y < -1.0 || p.x > iw + 1.0 || p.y > ih + 1.0 {
                    continue;
                }
                let mut px = if nearest {
                    if p.x < 0.0 || p.y < 0.0 || p.x >= iw || p.y >= ih {
                        continue;
                    }
                    sample_rgba_nearest(level, p.x, p.y)
                } else if exact {
                    sample_rgba_nearest(level, p.x, p.y)
                } else {
                    sample_rgba(level, p.x * level_scale, p.y * level_scale)
                };
                if px[3] <= 0.0 {
                    continue;
                }
                let mut k = opacity * k_extra;
                if let (Some((map, background)), Some((ml, sx, sy))) = (mask_map.as_ref(), mask_level.as_ref()) {
                    let q = map.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5));
                    let m = mask.unwrap();
                    let (mw, mh) = (m.asset.image.w as f64, m.asset.image.h as f64);
                    let v = match background {
                        Some(bg) if q.x < 0.0 || q.y < 0.0 || q.x > mw || q.y > mh => *bg,
                        _ => sample_gray(ml, q.x * sx, q.y * sy),
                    };
                    k *= v;
                }
                if k <= 0.0 {
                    continue;
                }
                if k < 1.0 {
                    for c in &mut px {
                        *c *= k;
                    }
                }
                composite(mode, &mut row[x * 4..x * 4 + 4], px);
            }
        });
    }

    /// Runs an adjustment over everything drawn so far, through its opacity, its mask and `clip`.
    fn adjust(&self, id: Uuid, buf: &mut Rgba, clip: Option<&[u8]>) {
        let Some(&index) = self.index.get(&id) else { return };
        let layer = &self.doc.layers[index];
        let Some(adjustment) = layer.adjustment.as_ref() else { return };
        let opacity = self.doc.effective_opacity(index) as f32;
        if opacity <= 0.0 {
            return;
        }
        let mut adjusted = buf.clone();
        adjustment.apply(&mut adjusted, self.t.placement());
        let mode = layer.blend;
        let mask = layer
            .mask
            .as_ref()
            .filter(|m| m.enabled)
            .map(|m| self.mask_coverage(&m.asset, &layer.transform));
        let w = buf.w;
        buf.data.par_chunks_mut(w * 4).zip(adjusted.data.par_chunks(w * 4)).enumerate().for_each(|(y, (dst, top))| {
            for x in 0..w {
                let i = y * w + x;
                let mut k = opacity;
                if let Some(c) = clip {
                    k *= c[i] as f32 / 255.0;
                }
                if let Some(m) = &mask {
                    k *= m[i] as f32 / 255.0;
                }
                if k <= 0.0 {
                    continue;
                }
                let d = &mut dst[x * 4..x * 4 + 4];
                let a = d[3] as f32;
                let mut new = [top[x * 4] as f32, top[x * 4 + 1] as f32, top[x * 4 + 2] as f32, top[x * 4 + 3] as f32];
                if mode != BlendMode::Normal {
                    // Blend the colors at full coverage, then put the original alpha back.
                    if a <= 0.0 {
                        continue;
                    }
                    let cb = [d[0] as f32 / a, d[1] as f32 / a, d[2] as f32 / a];
                    let ta = new[3].max(1.0);
                    let cs = [new[0] / ta, new[1] / ta, new[2] / ta];
                    let b = crate::blend::blend(mode, cb.map(|v| v.min(1.0)), cs.map(|v| v.min(1.0)));
                    new = [b[0] * a, b[1] * a, b[2] * a, a];
                }
                for c in 0..4 {
                    let v = d[c] as f32 + (new[c] - d[c] as f32) * k;
                    d[c] = v.round().clamp(0.0, 255.0) as u8;
                }
                let alpha = d[3];
                for c in 0..3 {
                    d[c] = d[c].min(alpha);
                }
            }
        });
    }
}

#[inline]
fn mul8(a: u8, b: u8) -> u8 {
    ((a as u32 * b as u32 + 127) / 255) as u8
}

/// How far past a view the canvas has to render so blurs read real neighbors (document pixels).
pub fn sampling_margin(doc: &Document) -> f64 {
    doc.visible_layers()
        .into_iter()
        .filter_map(|i| doc.layers[i].adjustment.as_ref())
        .map(|a| a.sampling_margin())
        .fold(0.0, f64::max)
}

/// Renders the part of the document a view shows: `view` in document pixels at `scale` output
/// pixels per document pixel, reading past its edges as far as blur adjustments need.
pub fn render_view(doc: &Document, view: Rect, scale: f64, px_origin: (i64, i64), px_size: (usize, usize)) -> Rgba {
    let margin_doc = sampling_margin(doc);
    let margin = (margin_doc * scale).ceil() as i64;
    if margin == 0 {
        let t = Target { w: px_size.0, h: px_size.1, origin: Point::new(view.x, view.y), scale };
        return render(doc, t);
    }
    // Grow the target in whole output pixels, clamped to the document, then cut the view back out.
    let doc_px = IRect::new(0, 0, (doc.width as f64 * scale).ceil() as i64, (doc.height as f64 * scale).ceil() as i64);
    let want = IRect::new(px_origin.0, px_origin.1, px_origin.0 + px_size.0 as i64, px_origin.1 + px_size.1 as i64);
    let grown = want.outset(margin);
    let grown = IRect::new(grown.x0.max(doc_px.x0), grown.y0.max(doc_px.y0), grown.x1.min(doc_px.x1), grown.y1.min(doc_px.y1));
    let grown = grown.union(&want);
    let t = Target {
        w: grown.width() as usize,
        h: grown.height() as usize,
        origin: Point::new(view.x - (want.x0 - grown.x0) as f64 / scale, view.y - (want.y0 - grown.y0) as f64 / scale),
        scale,
    };
    let full = render(doc, t);
    full.crop(IRect::new(want.x0 - grown.x0, want.y0 - grown.y0, want.x1 - grown.x0, want.y1 - grown.y0))
}
