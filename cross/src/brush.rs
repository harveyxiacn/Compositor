//! Painting: round brushes with hardness, on a layer's pixels or its mask, kept inside the selection.

use crate::geom::{Affine, IRect, Point};
use crate::model::{Document, LayerTransform};
use crate::raster::{Gray, MaskAsset, PixelAsset, Rgba};
use crate::selection::Selection;
use rayon::prelude::*;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrushSettings {
    /// Diameter in document pixels.
    pub size: f64,
    /// 0–1: the share of the radius at full strength.
    pub hardness: f64,
    /// 0–1.
    pub opacity: f64,
}

impl Default for BrushSettings {
    fn default() -> Self {
        Self { size: 40.0, hardness: 0.8, opacity: 1.0 }
    }
}

/// The soft-brush falloff across the region between the hardness radius and the rim.
fn falloff(u: f64) -> f64 {
    let k = 2.5;
    (((-k * u * u).exp() - (-k).exp()) / (1.0 - (-k).exp())).max(0.0)
}

enum Original {
    Pixels(Arc<PixelAsset>),
    Mask(Arc<MaskAsset>),
}

pub struct Stroke {
    pub layer: Uuid,
    pub on_mask: bool,
    original: Original,
    w: usize,
    h: usize,
    coverage: Vec<f32>,
    px_to_doc: Affine,
    doc_to_px: Affine,
    radius: f64,
    settings: BrushSettings,
    color: [u8; 3],
    erase: bool,
    selection: Option<Selection>,
    last: Option<Point>,
    pending: f64,
}

pub enum StrokeError {
    NotPaintable(&'static str),
}

/// Grid a mask covers when it has no pixels of its own yet.
fn mask_grid(layer: &crate::model::Layer) -> (usize, usize) {
    match &layer.pixels {
        Some(p) => (p.image.w, p.image.h),
        None => (layer.transform.size.0.round().max(1.0) as usize, layer.transform.size.1.round().max(1.0) as usize),
    }
}

impl Stroke {
    /// Prepares the active layer for painting: the document's layer gets its own copy of the pixels,
    /// which the stroke changes in place; the previous ones stay with the undo history.
    pub fn begin(
        doc: &mut Document,
        layer_id: Uuid,
        on_mask: bool,
        settings: BrushSettings,
        color: [u8; 3],
        erase: bool,
    ) -> Result<Stroke, StrokeError> {
        let (w, h) = (doc.width, doc.height);
        let selection = doc.selection.clone();
        let layer = doc.layer_mut(layer_id).ok_or(StrokeError::NotPaintable("No layer is selected."))?;
        let (original, gw, gh, px_to_doc);
        if on_mask {
            let Some(mask) = layer.mask.as_mut() else { return Err(StrokeError::NotPaintable("This layer has no mask.")) };
            let (mw, mh) = if mask.asset.image.w <= 1 && mask.asset.image.h <= 1 {
                (0, 0)
            } else {
                (mask.asset.image.w, mask.asset.image.h)
            };
            original = Original::Mask(mask.asset.clone());
            let (gw2, gh2) = if mw == 0 {
                let grid = mask_grid(layer);
                let fill = layer.mask.as_ref().unwrap().asset.image.data[0];
                layer.mask.as_mut().unwrap().asset = Arc::new(MaskAsset::new(Gray::new(grid.0, grid.1, fill)));
                grid
            } else {
                let copy = (*layer.mask.as_ref().unwrap().asset).clone();
                layer.mask.as_mut().unwrap().asset = Arc::new(copy);
                (mw, mh)
            };
            gw = gw2;
            gh = gh2;
            let m = layer.mask.as_ref().unwrap();
            let rect: LayerTransform = m.placement.unwrap_or(layer.transform);
            px_to_doc = rect.pixel_to_document(gw, gh);
        } else {
            if layer.is_group {
                return Err(StrokeError::NotPaintable("Folders have no pixels to paint. Select a layer, or the folder's mask."));
            }
            if layer.adjustment.is_some() {
                return Err(StrokeError::NotPaintable("Adjustment layers have no pixels. Paint on the layer's mask instead."));
            }
            match layer.pixels.clone() {
                Some(p) => {
                    original = Original::Pixels(p.clone());
                    layer.pixels = Some(Arc::new((*p).clone()));
                    gw = p.image.w;
                    gh = p.image.h;
                }
                None => {
                    // An empty layer gets canvas-sized pixels when painting begins.
                    let blank = Arc::new(PixelAsset::new(Rgba::new(w, h)));
                    original = Original::Pixels(blank.clone());
                    layer.pixels = Some(Arc::new((*blank).clone()));
                    let old = layer.transform;
                    layer.transform = LayerTransform::new(Point::default(), w as f64, h as f64);
                    layer.transform.sampling = old.sampling;
                    gw = w;
                    gh = h;
                }
            }
            layer.rasterize_metadata();
            px_to_doc = layer.transform.pixel_to_document(gw, gh);
        }
        let doc_to_px = px_to_doc.inverted().ok_or(StrokeError::NotPaintable("This layer is too small to paint."))?;
        let radius = (settings.size / 2.0 * doc_to_px.mean_scale()).max(0.5);
        Ok(Stroke {
            layer: layer_id,
            on_mask,
            original,
            w: gw,
            h: gh,
            coverage: vec![0.0; gw * gh],
            px_to_doc,
            doc_to_px,
            radius,
            settings,
            color,
            erase,
            selection,
            last: None,
            pending: 0.0,
        })
    }

    /// Extends the stroke to a document point, returning the document area it changed.
    pub fn add_point(&mut self, doc: &mut Document, point: Point) -> Option<crate::geom::Rect> {
        let p = self.doc_to_px.apply(point);
        let spacing = (self.radius * 0.2).max(0.5);
        let mut dirty = IRect::default();
        match self.last {
            None => dirty = self.dab(p),
            Some(last) => {
                let length = last.distance(p);
                let mut travelled = spacing - self.pending;
                while travelled <= length {
                    let t = travelled / length.max(1e-9);
                    let q = Point::new(last.x + (p.x - last.x) * t, last.y + (p.y - last.y) * t);
                    dirty = dirty.union(&self.dab(q));
                    travelled += spacing;
                }
                self.pending = length - (travelled - spacing);
            }
        }
        self.last = Some(p);
        if dirty.is_empty() {
            return None;
        }
        self.apply(doc, dirty);
        let r = crate::geom::Rect::new(dirty.x0 as f64, dirty.y0 as f64, dirty.width() as f64, dirty.height() as f64);
        Some(self.px_to_doc.map_rect(&r))
    }

    fn dab(&mut self, c: Point) -> IRect {
        let r = self.radius;
        let area = IRect::new((c.x - r - 1.0).floor() as i64, (c.y - r - 1.0).floor() as i64, (c.x + r + 1.0).ceil() as i64, (c.y + r + 1.0).ceil() as i64)
            .clamp(self.w as i64, self.h as i64);
        if area.is_empty() {
            return area;
        }
        let hardness = self.settings.hardness.clamp(0.0, 1.0);
        let w = self.w;
        let rows = &mut self.coverage[area.y0 as usize * w..area.y1 as usize * w];
        rows.par_chunks_mut(w).enumerate().for_each(|(i, row)| {
            let y = area.y0 as f64 + i as f64 + 0.5;
            for x in area.x0 as usize..area.x1 as usize {
                let d = Point::new(x as f64 + 0.5, y).distance(c);
                let edge = (r + 0.5 - d).clamp(0.0, 1.0);
                if edge <= 0.0 {
                    continue;
                }
                let u = d / r;
                let v = if u <= hardness || hardness >= 1.0 { 1.0 } else { falloff(((u - hardness) / (1.0 - hardness)).min(1.0)) };
                let v = (v * edge) as f32;
                if v > row[x] {
                    row[x] = v;
                }
            }
        });
        area
    }

    fn apply(&self, doc: &mut Document, area: IRect) {
        let Some(layer) = doc.layer_mut(self.layer) else { return };
        let opacity = self.settings.opacity as f32;
        let w = self.w;
        let sel = self.selection.as_ref();
        let px_to_doc = self.px_to_doc;
        let strength = |x: usize, y: usize| -> f32 {
            let mut c = self.coverage[y * w + x] * opacity;
            if c > 0.0 {
                if let Some(s) = sel {
                    c *= s.at(px_to_doc.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5)));
                }
            }
            c
        };
        match (&self.original, self.on_mask) {
            (Original::Mask(orig), true) => {
                let Some(mask) = layer.mask.as_mut() else { return };
                let Some(asset) = Arc::get_mut(&mut mask.asset) else { return };
                let lum = (0.299 * self.color[0] as f32 + 0.587 * self.color[1] as f32 + 0.114 * self.color[2] as f32).round();
                let value = if self.erase { 255.0 - lum } else { lum };
                let uniform = (orig.image.w <= 1 && orig.image.h <= 1).then(|| orig.image.data[0]);
                let rows = &mut asset.image.data[area.y0 as usize * w..area.y1 as usize * w];
                rows.par_chunks_mut(w).enumerate().for_each(|(i, row)| {
                    let y = area.y0 as usize + i;
                    for x in area.x0 as usize..area.x1 as usize {
                        let c = strength(x, y);
                        let o = uniform.unwrap_or_else(|| orig.image.data[y * w + x]) as f32;
                        row[x] = (o + (value - o) * c).round() as u8;
                    }
                });
                asset.refresh(area);
            }
            (Original::Pixels(orig), false) => {
                let Some(pixels) = layer.pixels.as_mut() else { return };
                let Some(asset) = Arc::get_mut(pixels) else { return };
                let color = [self.color[0] as f32, self.color[1] as f32, self.color[2] as f32, 255.0];
                let erase = self.erase;
                let rows = &mut asset.image.data[area.y0 as usize * w * 4..area.y1 as usize * w * 4];
                rows.par_chunks_mut(w * 4).enumerate().for_each(|(i, row)| {
                    let y = area.y0 as usize + i;
                    for x in area.x0 as usize..area.x1 as usize {
                        let c = strength(x, y);
                        let o = (y * w + x) * 4;
                        for k in 0..4 {
                            let src = orig.image.data[o + k] as f32;
                            let v = if erase { src * (1.0 - c) } else { src + (color[k] - src) * c };
                            row[x * 4 + k] = v.round().clamp(0.0, 255.0) as u8;
                        }
                    }
                });
                asset.refresh(area);
            }
            _ => {}
        }
    }
}

pub enum RegionOp {
    Fill([u8; 3]),
    Clear,
}

/// Fills or clears the selected part of a layer (or of its mask), as one edit.
pub fn edit_region(doc: &mut Document, layer_id: Uuid, on_mask: bool, op: RegionOp) -> Result<(), &'static str> {
    let fill = match op {
        RegionOp::Fill(c) => Some(c),
        RegionOp::Clear => None,
    };
    let settings = BrushSettings { size: 1.0, hardness: 1.0, opacity: 1.0 };
    // Clearing a mask paints it black (hidden); clearing pixels erases them.
    let color = fill.unwrap_or([0, 0, 0]);
    let mut stroke = match Stroke::begin(doc, layer_id, on_mask, settings, color, fill.is_none() && !on_mask) {
        Ok(s) => s,
        Err(StrokeError::NotPaintable(msg)) => return Err(msg),
    };
    let selection = doc.selection.clone();
    // Full coverage everywhere: the selection, sampled per pixel, limits it.
    stroke.coverage.iter_mut().for_each(|c| *c = 1.0);
    let area = match &selection {
        Some(s) => {
            let r = s.bounds;
            let doc_rect = crate::geom::Rect::new(r.x0 as f64, r.y0 as f64, r.width() as f64, r.height() as f64);
            IRect::covering(&stroke.doc_to_px.map_rect(&doc_rect)).outset(1).clamp(stroke.w as i64, stroke.h as i64)
        }
        None => IRect::new(0, 0, stroke.w as i64, stroke.h as i64),
    };
    stroke.apply(doc, area);
    Ok(())
}
