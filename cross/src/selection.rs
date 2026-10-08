//! A pixel selection: soft coverage over the canvas, plus its outline for marching ants.

use crate::geom::{IRect, Point, Rect};
use crate::raster::Gray;
use rayon::prelude::*;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combine {
    Replace,
    Add,
    Subtract,
    Intersect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Rect,
    Ellipse,
}

#[derive(Clone, Debug)]
pub struct Selection {
    /// Canvas-sized coverage.
    pub mask: Arc<Gray>,
    /// Pixels holding any coverage.
    pub bounds: IRect,
    /// Outline segments in document pixels, for drawing.
    pub outline: Arc<Vec<(Point, Point)>>,
}

impl Selection {
    pub fn from_mask(mask: Gray) -> Option<Self> {
        let bounds = coverage_bounds(&mask)?;
        let outline = trace(&mask, bounds);
        Some(Self { mask: Arc::new(mask), bounds, outline: Arc::new(outline) })
    }
    pub fn all(w: usize, h: usize) -> Self {
        Self::from_mask(Gray::new(w, h, 255)).expect("non-empty canvas")
    }
    #[inline]
    pub fn at(&self, p: Point) -> f32 {
        let (x, y) = (p.x.floor() as i64, p.y.floor() as i64);
        if x < 0 || y < 0 || x >= self.mask.w as i64 || y >= self.mask.h as i64 {
            return 0.0;
        }
        self.mask.data[y as usize * self.mask.w + x as usize] as f32 / 255.0
    }
    pub fn inverted(&self) -> Option<Self> {
        let mut m = (*self.mask).clone();
        m.data.par_iter_mut().for_each(|v| *v = 255 - *v);
        Self::from_mask(m)
    }
}

/// Paints a rectangle or ellipse, antialiased, into canvas-sized coverage, combined with what was
/// selected before.
pub fn shape_selection(w: usize, h: usize, shape: Shape, r: Rect, previous: Option<&Selection>, mode: Combine) -> Option<Selection> {
    let mut m = match (mode, previous) {
        (Combine::Replace, _) | (_, None) => Gray::new(w, h, 0),
        (_, Some(p)) => (*p.mask).clone(),
    };
    if mode != Combine::Replace && previous.is_none() && mode != Combine::Add {
        return None;
    }
    let cx = r.x + r.w / 2.0;
    let cy = r.y + r.h / 2.0;
    let (rx, ry) = (r.w / 2.0, r.h / 2.0);
    // 4×4 supersampling for the edge.
    let coverage = |x: usize, y: usize| -> f32 {
        let mut hit = 0;
        for sy in 0..4 {
            for sx in 0..4 {
                let px = x as f64 + (sx as f64 + 0.5) / 4.0;
                let py = y as f64 + (sy as f64 + 0.5) / 4.0;
                let inside = match shape {
                    Shape::Rect => px >= r.x && px <= r.max_x() && py >= r.y && py <= r.max_y(),
                    Shape::Ellipse => {
                        rx > 0.0 && ry > 0.0 && ((px - cx) / rx).powi(2) + ((py - cy) / ry).powi(2) <= 1.0
                    }
                };
                hit += inside as i32;
            }
        }
        hit as f32 / 16.0
    };
    let area = IRect::covering(&r).clamp(w as i64, h as i64);
    let full = IRect::new(0, 0, w as i64, h as i64);
    let region = if mode == Combine::Intersect { full } else { area };
    m.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let y64 = y as i64;
        if y64 < region.y0 || y64 >= region.y1 {
            return;
        }
        for x in region.x0 as usize..region.x1 as usize {
            let inside_area = y64 >= area.y0 && y64 < area.y1 && (x as i64) >= area.x0 && (x as i64) < area.x1;
            let c = if inside_area { coverage(x, y) } else { 0.0 };
            let old = row[x] as f32 / 255.0;
            let new = match mode {
                Combine::Replace | Combine::Add => old.max(c),
                Combine::Subtract => old * (1.0 - c),
                Combine::Intersect => old * c,
            };
            row[x] = (new * 255.0).round() as u8;
        }
    });
    Selection::from_mask(m)
}

fn coverage_bounds(m: &Gray) -> Option<IRect> {
    let rows: Vec<Option<(usize, usize)>> = m
        .data
        .par_chunks(m.w)
        .map(|row| {
            let first = row.iter().position(|&v| v != 0)?;
            let last = row.iter().rposition(|&v| v != 0)?;
            Some((first, last + 1))
        })
        .collect();
    let mut r: Option<IRect> = None;
    for (y, span) in rows.iter().enumerate() {
        if let Some((x0, x1)) = span {
            let row = IRect::new(*x0 as i64, y as i64, *x1 as i64, y as i64 + 1);
            r = Some(r.map_or(row, |r| r.union(&row)));
        }
    }
    r
}

/// The 50% contour, traced on a grid coarse enough to stay a few thousand segments.
fn trace(m: &Gray, bounds: IRect) -> Vec<(Point, Point)> {
    let step = ((bounds.width().max(bounds.height()) as f64 / 1200.0).ceil() as i64).max(1);
    let inside = |x: i64, y: i64| -> bool {
        if x < 0 || y < 0 || x >= m.w as i64 || y >= m.h as i64 {
            return false;
        }
        m.data[y as usize * m.w + x as usize] >= 128
    };
    let mut out = Vec::new();
    let mut y = bounds.y0 - step;
    while y <= bounds.y1 {
        let mut x = bounds.x0 - step;
        while x <= bounds.x1 {
            let here = inside(x, y);
            // Edge to the right neighbor: a vertical segment between them.
            if here != inside(x + step, y) {
                let ex = (x + step) as f64;
                out.push((Point::new(ex, y as f64), Point::new(ex, (y + step) as f64)));
            }
            if here != inside(x, y + step) {
                let ey = (y + step) as f64;
                out.push((Point::new(x as f64, ey), Point::new((x + step) as f64, ey)));
            }
            x += step;
        }
        y += step;
    }
    out
}
