//! Pixel storage: premultiplied RGBA8 layers and 8-bit gray masks, each with a pyramid of
//! halvings so a zoomed-out canvas samples sharp, alias-free pixels.

use crate::geom::IRect;
use rayon::prelude::*;

/// Premultiplied RGBA, 8 bits per channel, rows top to bottom.
#[derive(Clone, Debug, PartialEq)]
pub struct Rgba {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

impl Rgba {
    pub fn new(w: usize, h: usize) -> Self {
        Self { w, h, data: vec![0; w * h * 4] }
    }
    pub fn filled(w: usize, h: usize, color: [u8; 4]) -> Self {
        let mut data = Vec::with_capacity(w * h * 4);
        for _ in 0..w * h {
            data.extend_from_slice(&color);
        }
        Self { w, h, data }
    }
    /// From straight (unpremultiplied) RGBA bytes, as PNG stores them.
    pub fn from_straight(w: usize, h: usize, mut data: Vec<u8>) -> Self {
        data.par_chunks_mut(4 * w.max(1)).for_each(|row| {
            for p in row.chunks_exact_mut(4) {
                let a = p[3] as u32;
                if a < 255 {
                    for c in &mut p[..3] {
                        *c = ((*c as u32 * a + 127) / 255) as u8;
                    }
                }
            }
        });
        Self { w, h, data }
    }
    pub fn to_straight(&self) -> Vec<u8> {
        let mut out = self.data.clone();
        out.par_chunks_mut(4 * self.w.max(1)).for_each(|row| {
            for p in row.chunks_exact_mut(4) {
                let a = p[3] as u32;
                if a == 0 {
                    p[0] = 0;
                    p[1] = 0;
                    p[2] = 0;
                } else if a < 255 {
                    for c in &mut p[..3] {
                        *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                    }
                }
            }
        });
        out
    }
    #[inline]
    pub fn pixel(&self, x: usize, y: usize) -> [u8; 4] {
        let i = (y * self.w + x) * 4;
        [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
    }
    /// The smallest rectangle holding every pixel that isn't fully transparent.
    pub fn opaque_bounds(&self) -> Option<IRect> {
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
        for y in 0..self.h {
            let row = &self.data[y * self.w * 4..(y + 1) * self.w * 4];
            let mut any = false;
            for x in 0..self.w {
                if row[x * 4 + 3] != 0 {
                    any = true;
                    x0 = x0.min(x);
                    x1 = x1.max(x + 1);
                }
            }
            if any {
                y0 = y0.min(y);
                y1 = y + 1;
            }
        }
        (x0 != usize::MAX).then(|| IRect::new(x0 as i64, y0 as i64, x1 as i64, y1 as i64))
    }
    pub fn crop(&self, r: IRect) -> Rgba {
        let r = r.clamp(self.w as i64, self.h as i64);
        let (w, h) = (r.width() as usize, r.height() as usize);
        let mut out = Rgba::new(w, h);
        for y in 0..h {
            let src = ((r.y0 as usize + y) * self.w + r.x0 as usize) * 4;
            out.data[y * w * 4..(y + 1) * w * 4].copy_from_slice(&self.data[src..src + w * 4]);
        }
        out
    }
}

/// 8-bit coverage, white reveals and black hides.
#[derive(Clone, Debug, PartialEq)]
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

impl Gray {
    pub fn new(w: usize, h: usize, value: u8) -> Self {
        Self { w, h, data: vec![value; w * h] }
    }
    pub fn is_uniform(&self) -> Option<u8> {
        let first = *self.data.first()?;
        self.data.iter().all(|&v| v == first).then_some(first)
    }
}

/// How many halvings an image of this size keeps.
fn pyramid_depth(w: usize, h: usize) -> usize {
    let mut side = w.max(h);
    let mut depth = 0;
    while side > 96 && depth < 14 {
        side = side.div_ceil(2);
        depth += 1;
    }
    depth
}

/// One halving: each pixel averages a 2×2 block; past the right and bottom edge counts as clear,
/// so level `n` covers exactly `2^n` source pixels per pixel.
fn halve_rgba(src: &Rgba, dst: &mut Rgba, rows: std::ops::Range<usize>, cols: std::ops::Range<usize>) {
    let (sw, sh) = (src.w, src.h);
    let dw = dst.w;
    let row_bytes = dw * 4;
    let start = rows.start;
    dst.data[rows.start * row_bytes..rows.end * row_bytes]
        .par_chunks_mut(row_bytes)
        .enumerate()
        .for_each(|(i, row)| {
            let y = start + i;
            for x in cols.clone() {
                let mut sum = [0u32; 4];
                for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                    let (sx, sy) = (x * 2 + dx, y * 2 + dy);
                    if sx < sw && sy < sh {
                        let p = (sy * sw + sx) * 4;
                        for c in 0..4 {
                            sum[c] += src.data[p + c] as u32;
                        }
                    }
                }
                for c in 0..4 {
                    row[x * 4 + c] = ((sum[c] + 2) / 4) as u8;
                }
            }
        });
}

fn halve_gray(src: &Gray, dst: &mut Gray, rows: std::ops::Range<usize>, cols: std::ops::Range<usize>) {
    let (sw, sh) = (src.w, src.h);
    let dw = dst.w;
    let start = rows.start;
    dst.data[rows.start * dw..rows.end * dw].par_chunks_mut(dw).enumerate().for_each(|(i, row)| {
        let y = start + i;
        for x in cols.clone() {
            let mut sum = 0u32;
            let mut n = 0u32;
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let (sx, sy) = ((x * 2 + dx).min(sw - 1), (y * 2 + dy).min(sh - 1));
                sum += src.data[sy * sw + sx] as u32;
                n += 1;
            }
            row[x] = ((sum + n / 2) / n) as u8;
        }
    });
}

/// A layer's pixels with their halvings. Immutable once shared; a brush stroke owns a copy and
/// refreshes the halvings of what it touched.
#[derive(Clone, Debug)]
pub struct PixelAsset {
    pub image: Rgba,
    pub mips: Vec<Rgba>,
}

impl PixelAsset {
    pub fn new(image: Rgba) -> Self {
        let mut mips: Vec<Rgba> = Vec::new();
        for _ in 0..pyramid_depth(image.w, image.h) {
            let src = mips.last().unwrap_or(&image);
            let mut dst = Rgba::new(src.w.div_ceil(2), src.h.div_ceil(2));
            let (h, w) = (dst.h, dst.w);
            halve_rgba(src, &mut dst, 0..h, 0..w);
            mips.push(dst);
        }
        Self { image, mips }
    }
    /// The level to sample when `factor` source pixels land on one output pixel, and its scale.
    pub fn level(&self, factor: f64) -> (&Rgba, f64) {
        if factor < 2.0 || self.mips.is_empty() {
            return (&self.image, 1.0);
        }
        let n = (factor.log2().floor() as usize).min(self.mips.len());
        (&self.mips[n - 1], 1.0 / (1u64 << n) as f64)
    }
    /// Recomputes the halvings under a changed rectangle of the full-size image.
    pub fn refresh(&mut self, region: IRect) {
        let mut r = region.clamp(self.image.w as i64, self.image.h as i64);
        for i in 0..self.mips.len() {
            r = IRect::new(r.x0 / 2, r.y0 / 2, (r.x1 + 1) / 2, (r.y1 + 1) / 2);
            let (head, tail) = self.mips.split_at_mut(i);
            let src = if i == 0 { &self.image } else { &head[i - 1] };
            let dst = &mut tail[0];
            let r2 = r.clamp(dst.w as i64, dst.h as i64);
            if r2.is_empty() {
                break;
            }
            halve_rgba(src, dst, r2.y0 as usize..r2.y1 as usize, r2.x0 as usize..r2.x1 as usize);
        }
    }
}

#[derive(Clone, Debug)]
pub struct MaskAsset {
    pub image: Gray,
    pub mips: Vec<Gray>,
}

impl MaskAsset {
    pub fn new(image: Gray) -> Self {
        let mut mips: Vec<Gray> = Vec::new();
        for _ in 0..pyramid_depth(image.w, image.h) {
            let src = mips.last().unwrap_or(&image);
            let mut dst = Gray::new(src.w.div_ceil(2), src.h.div_ceil(2), 0);
            let (h, w) = (dst.h, dst.w);
            halve_gray(src, &mut dst, 0..h, 0..w);
            mips.push(dst);
        }
        Self { image, mips }
    }
    pub fn level(&self, factor: f64) -> (&Gray, f64, f64) {
        if factor < 2.0 || self.mips.is_empty() {
            return (&self.image, 1.0, 1.0);
        }
        let n = (factor.log2().floor() as usize).min(self.mips.len());
        let m = &self.mips[n - 1];
        // Gray halvings clamp at the edge, so they stretch exactly over the image.
        (m, m.w as f64 / self.image.w as f64, m.h as f64 / self.image.h as f64)
    }
    pub fn refresh(&mut self, region: IRect) {
        let mut r = region.clamp(self.image.w as i64, self.image.h as i64);
        for i in 0..self.mips.len() {
            r = IRect::new(r.x0 / 2, r.y0 / 2, (r.x1 + 1) / 2, (r.y1 + 1) / 2);
            let (head, tail) = self.mips.split_at_mut(i);
            let src = if i == 0 { &self.image } else { &head[i - 1] };
            let dst = &mut tail[0];
            let r2 = r.clamp(dst.w as i64, dst.h as i64);
            if r2.is_empty() {
                break;
            }
            halve_gray(src, dst, r2.y0 as usize..r2.y1 as usize, r2.x0 as usize..r2.x1 as usize);
        }
    }
    /// What the mask shows past its own pixels once placed apart from its layer: white or black,
    /// whichever most of its edge is.
    pub fn background(&self) -> f32 {
        let g = self.mips.last().unwrap_or(&self.image);
        let (w, h) = (g.w, g.h);
        let (mut total, mut count) = (0u64, 0u64);
        for y in 0..h {
            for x in 0..w {
                if y == 0 || y == h - 1 || x == 0 || x == w - 1 {
                    total += g.data[y * w + x] as u64;
                    count += 1;
                }
            }
        }
        if total * 2 >= count * 255 {
            1.0
        } else {
            0.0
        }
    }
}

/// Bilinear sample at continuous pixel coordinates (pixel centers at `i + 0.5`); outside reads clear.
/// Returns premultiplied channels, 0–255.
#[inline]
pub fn sample_rgba(img: &Rgba, x: f64, y: f64) -> [f32; 4] {
    let fx = x - 0.5;
    let fy = y - 0.5;
    let x0 = fx.floor();
    let y0 = fy.floor();
    let tx = (fx - x0) as f32;
    let ty = (fy - y0) as f32;
    let (x0, y0) = (x0 as i64, y0 as i64);
    let (w, h) = (img.w as i64, img.h as i64);
    if x0 >= 0 && y0 >= 0 && x0 + 1 < w && y0 + 1 < h {
        let i = ((y0 * w + x0) * 4) as usize;
        let j = i + (w as usize) * 4;
        let d = &img.data;
        let mut out = [0f32; 4];
        for c in 0..4 {
            let top = d[i + c] as f32 + (d[i + 4 + c] as f32 - d[i + c] as f32) * tx;
            let bottom = d[j + c] as f32 + (d[j + 4 + c] as f32 - d[j + c] as f32) * tx;
            out[c] = top + (bottom - top) * ty;
        }
        return out;
    }
    let mut out = [0f32; 4];
    for (dy, wy) in [(0i64, 1.0 - ty), (1, ty)] {
        for (dx, wx) in [(0i64, 1.0 - tx), (1, tx)] {
            let (sx, sy) = (x0 + dx, y0 + dy);
            if sx >= 0 && sy >= 0 && sx < w && sy < h {
                let i = ((sy * w + sx) * 4) as usize;
                let weight = wx * wy;
                for c in 0..4 {
                    out[c] += img.data[i + c] as f32 * weight;
                }
            }
        }
    }
    out
}

#[inline]
pub fn sample_rgba_nearest(img: &Rgba, x: f64, y: f64) -> [f32; 4] {
    let (sx, sy) = (x.floor() as i64, y.floor() as i64);
    if sx < 0 || sy < 0 || sx >= img.w as i64 || sy >= img.h as i64 {
        return [0.0; 4];
    }
    let i = ((sy as usize) * img.w + sx as usize) * 4;
    [img.data[i] as f32, img.data[i + 1] as f32, img.data[i + 2] as f32, img.data[i + 3] as f32]
}

/// Bilinear gray sample, clamped to the edge; 0–1.
#[inline]
pub fn sample_gray(img: &Gray, x: f64, y: f64) -> f32 {
    let fx = (x - 0.5).clamp(0.0, (img.w - 1) as f64);
    let fy = (y - 0.5).clamp(0.0, (img.h - 1) as f64);
    let x0 = fx.floor() as usize;
    let y0 = fy.floor() as usize;
    let x1 = (x0 + 1).min(img.w - 1);
    let y1 = (y0 + 1).min(img.h - 1);
    let tx = (fx - x0 as f64) as f32;
    let ty = (fy - y0 as f64) as f32;
    let d = &img.data;
    let top = d[y0 * img.w + x0] as f32 * (1.0 - tx) + d[y0 * img.w + x1] as f32 * tx;
    let bottom = d[y1 * img.w + x0] as f32 * (1.0 - tx) + d[y1 * img.w + x1] as f32 * tx;
    (top + (bottom - top) * ty) / 255.0
}

pub fn decode_png_rgba(bytes: &[u8]) -> anyhow::Result<Rgba> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok(Rgba::from_straight(w as usize, h as usize, rgba.into_raw()))
}

pub fn decode_png_gray(bytes: &[u8]) -> anyhow::Result<Gray> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)?;
    let luma = img.to_luma8();
    let (w, h) = luma.dimensions();
    Ok(Gray { w: w as usize, h: h as usize, data: luma.into_raw() })
}

pub fn encode_png_rgba(img: &Rgba) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new_with_quality(
        &mut out,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Adaptive,
    );
    image::ImageEncoder::write_image(encoder, &img.to_straight(), img.w as u32, img.h as u32, image::ExtendedColorType::Rgba8)?;
    Ok(out)
}

pub fn encode_png_gray(img: &Gray) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new_with_quality(
        &mut out,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Adaptive,
    );
    image::ImageEncoder::write_image(encoder, &img.data, img.w as u32, img.h as u32, image::ExtendedColorType::L8)?;
    Ok(out)
}
