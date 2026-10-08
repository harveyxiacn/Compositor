//! Blend modes, computed the way Photoshop does in sRGB, composited with the PDF/W3C formula.

use crate::model::BlendMode;

#[inline]
fn burn(cb: f32, cs: f32) -> f32 {
    if cb >= 1.0 {
        1.0
    } else if cs <= 0.0 {
        0.0
    } else {
        1.0 - ((1.0 - cb) / cs).min(1.0)
    }
}

#[inline]
fn dodge(cb: f32, cs: f32) -> f32 {
    if cb <= 0.0 {
        0.0
    } else if cs >= 1.0 {
        1.0
    } else {
        (cb / (1.0 - cs)).min(1.0)
    }
}

#[inline]
fn hard_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        cb * 2.0 * cs
    } else {
        let s = 2.0 * cs - 1.0;
        cb + s - cb * s
    }
}

#[inline]
fn separable(mode: BlendMode, cb: f32, cs: f32) -> f32 {
    match mode {
        BlendMode::Multiply => cb * cs,
        BlendMode::Screen => cb + cs - cb * cs,
        BlendMode::Overlay => hard_light(cs, cb),
        BlendMode::Darken => cb.min(cs),
        BlendMode::Lighten => cb.max(cs),
        BlendMode::ColorDodge => dodge(cb, cs),
        BlendMode::ColorBurn => burn(cb, cs),
        BlendMode::HardLight => hard_light(cb, cs),
        BlendMode::SoftLight => {
            // Photoshop's soft light, not the W3C variant.
            if cs <= 0.5 {
                cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
            } else {
                cb + (2.0 * cs - 1.0) * (cb.sqrt() - cb)
            }
        }
        BlendMode::Difference => (cb - cs).abs(),
        BlendMode::Exclusion => cb + cs - 2.0 * cb * cs,
        BlendMode::LinearBurn => (cb + cs - 1.0).max(0.0),
        BlendMode::LinearDodge => (cb + cs).min(1.0),
        BlendMode::VividLight => {
            if cs <= 0.5 {
                burn(cb, 2.0 * cs)
            } else {
                dodge(cb, 2.0 * (cs - 0.5))
            }
        }
        BlendMode::LinearLight => (cb + 2.0 * cs - 1.0).clamp(0.0, 1.0),
        BlendMode::PinLight => {
            if cs <= 0.5 {
                cb.min(2.0 * cs)
            } else {
                cb.max(2.0 * cs - 1.0)
            }
        }
        BlendMode::HardMix => {
            if cb + cs >= 1.0 {
                1.0
            } else {
                0.0
            }
        }
        BlendMode::Subtract => (cb - cs).max(0.0),
        BlendMode::Divide => {
            if cs <= 0.0 {
                if cb <= 0.0 {
                    0.0
                } else {
                    1.0
                }
            } else {
                (cb / cs).min(1.0)
            }
        }
        _ => cs,
    }
}

#[inline]
fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

#[inline]
fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut out = c;
    if n < 0.0 {
        for v in &mut out {
            *v = l + (*v - l) * l / (l - n).max(1e-6);
        }
    }
    if x > 1.0 {
        for v in &mut out {
            *v = l + (*v - l) * (1.0 - l) / (x - l).max(1e-6);
        }
    }
    out
}

#[inline]
fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

#[inline]
fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

#[inline]
fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let max = c[0].max(c[1]).max(c[2]);
    let min = c[0].min(c[1]).min(c[2]);
    let range = max - min;
    let mut out = [0.0; 3];
    if range > 0.0 {
        for i in 0..3 {
            out[i] = (c[i] - min) * s / range;
        }
    }
    out
}

/// The blended color of a backdrop `cb` and source `cs`, both straight (unpremultiplied) 0–1.
#[inline]
pub fn blend(mode: BlendMode, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    match mode {
        BlendMode::Normal => cs,
        BlendMode::Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        BlendMode::Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        BlendMode::Color => set_lum(cs, lum(cb)),
        BlendMode::Luminosity => set_lum(cb, lum(cs)),
        m => [separable(m, cb[0], cs[0]), separable(m, cb[1], cs[1]), separable(m, cb[2], cs[2])],
    }
}

/// Composites a premultiplied source pixel (0–255 floats) onto a premultiplied byte pixel.
#[inline]
pub fn composite(mode: BlendMode, dst: &mut [u8], src: [f32; 4]) {
    let sa = src[3] / 255.0;
    if sa <= 0.0 {
        return;
    }
    let da = dst[3] as f32 / 255.0;
    if mode == BlendMode::Normal || da <= 0.0 {
        let k = 1.0 - sa;
        for c in 0..4 {
            dst[c] = (src[c] + dst[c] as f32 * k).round().clamp(0.0, 255.0) as u8;
        }
        return;
    }
    let cs = [src[0] / 255.0 / sa, src[1] / 255.0 / sa, src[2] / 255.0 / sa];
    let cb = [dst[0] as f32 / 255.0 / da, dst[1] as f32 / 255.0 / da, dst[2] as f32 / 255.0 / da];
    let b = blend(mode, [cb[0].min(1.0), cb[1].min(1.0), cb[2].min(1.0)], [cs[0].min(1.0), cs[1].min(1.0), cs[2].min(1.0)]);
    let ao = sa + da - sa * da;
    for c in 0..3 {
        let v = src[c] / 255.0 * (1.0 - da) + dst[c] as f32 / 255.0 * (1.0 - sa) + sa * da * b[c];
        dst[c] = (v * 255.0).round().clamp(0.0, ao * 255.0 + 0.5) as u8;
    }
    dst[3] = (ao * 255.0).round().clamp(0.0, 255.0) as u8;
}
