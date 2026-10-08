//! Adjustment layers: their saved settings (the same JSON the macOS app writes) and how they
//! change pixels. The per-pixel work runs through the app's own C kernels.

use crate::raster::Rgba;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

mod ffi {
    extern "C" {
        pub fn levels_apply(pixels: *mut u8, count: usize, tables: *const f32);
        pub fn cube_apply(pixels: *mut u8, count: usize, cube: *const f32, dimension: i32);
        pub fn adjust_gradient_map(rgba: *mut u8, width: usize, height: usize, stride: usize, table: *const u8);
        pub fn adjust_grain(
            rgba: *mut u8,
            width: usize,
            height: usize,
            stride: usize,
            amount: f64,
            size: f64,
            roughness: f64,
            seed: u32,
            origin_x: f64,
            origin_y: f64,
            units_per_pixel: f64,
        );
        pub fn adjust_black_white(
            rgba: *mut u8,
            width: usize,
            height: usize,
            stride: usize,
            weights: *const f32,
            tint: i32,
            tint_hue: f64,
            tint_saturation: f64,
        );
        pub fn adjust_color_balance(
            rgba: *mut u8,
            width: usize,
            height: usize,
            stride: usize,
            shadows: *const f32,
            midtones: *const f32,
            highlights: *const f32,
            preserve_luminosity: i32,
        );
        pub fn noise_add_at(
            rgba: *mut u8,
            width: usize,
            height: usize,
            stride: usize,
            amount: f32,
            gaussian: i32,
            monochromatic: i32,
            seed: u32,
            origin_x: i64,
            origin_y: i64,
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdjustmentKind {
    #[serde(rename = "Hue/Saturation")]
    Hsv,
    Levels,
    Curves,
    Exposure,
    #[serde(rename = "Gradient Map")]
    GradientMap,
    Grain,
    #[serde(rename = "Add Noise")]
    AddNoise,
    #[serde(rename = "Gaussian Blur")]
    GaussianBlur,
    #[serde(rename = "Motion Blur")]
    MotionBlur,
    Invert,
    #[serde(rename = "Black & White")]
    BlackWhite,
    #[serde(rename = "Color Balance")]
    ColorBalance,
}

impl AdjustmentKind {
    pub const ALL: [AdjustmentKind; 12] = [
        AdjustmentKind::Hsv,
        AdjustmentKind::Levels,
        AdjustmentKind::Curves,
        AdjustmentKind::Exposure,
        AdjustmentKind::GradientMap,
        AdjustmentKind::Grain,
        AdjustmentKind::AddNoise,
        AdjustmentKind::GaussianBlur,
        AdjustmentKind::MotionBlur,
        AdjustmentKind::Invert,
        AdjustmentKind::BlackWhite,
        AdjustmentKind::ColorBalance,
    ];
    pub fn label(self) -> &'static str {
        match self {
            AdjustmentKind::Hsv => "Hue/Saturation",
            AdjustmentKind::Levels => "Levels",
            AdjustmentKind::Curves => "Curves",
            AdjustmentKind::Exposure => "Exposure",
            AdjustmentKind::GradientMap => "Gradient Map",
            AdjustmentKind::Grain => "Grain",
            AdjustmentKind::AddNoise => "Add Noise",
            AdjustmentKind::GaussianBlur => "Gaussian Blur",
            AdjustmentKind::MotionBlur => "Motion Blur",
            AdjustmentKind::Invert => "Invert",
            AdjustmentKind::BlackWhite => "Black & White",
            AdjustmentKind::ColorBalance => "Color Balance",
        }
    }
    /// Kinds that sample neighboring pixels arrived in format version 9.
    pub fn samples_neighbors(self) -> bool {
        matches!(self, AdjustmentKind::GaussianBlur | AdjustmentKind::MotionBlur | AdjustmentKind::AddNoise)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ColorRange {
    Master,
    Reds,
    Yellows,
    Greens,
    Cyans,
    Blues,
    Magentas,
}

impl ColorRange {
    pub const ALL: [ColorRange; 7] = [
        ColorRange::Master,
        ColorRange::Reds,
        ColorRange::Yellows,
        ColorRange::Greens,
        ColorRange::Cyans,
        ColorRange::Blues,
        ColorRange::Magentas,
    ];
    pub fn label(self) -> &'static str {
        match self {
            ColorRange::Master => "Master",
            ColorRange::Reds => "Reds",
            ColorRange::Yellows => "Yellows",
            ColorRange::Greens => "Greens",
            ColorRange::Cyans => "Cyans",
            ColorRange::Blues => "Blues",
            ColorRange::Magentas => "Magentas",
        }
    }
    pub fn default_band(self) -> HueBand {
        let (a, b, c, d) = match self {
            ColorRange::Master => (0.0, 0.0, 360.0, 360.0),
            ColorRange::Reds => (315.0, 345.0, 15.0, 45.0),
            ColorRange::Yellows => (15.0, 45.0, 75.0, 105.0),
            ColorRange::Greens => (75.0, 105.0, 135.0, 165.0),
            ColorRange::Cyans => (135.0, 165.0, 195.0, 225.0),
            ColorRange::Blues => (195.0, 225.0, 255.0, 285.0),
            ColorRange::Magentas => (255.0, 285.0, 315.0, 345.0),
        };
        HueBand { falloff_start: a, range_start: b, range_end: c, falloff_end: d }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct HueBand {
    #[serde(rename = "falloffStart")]
    pub falloff_start: f64,
    #[serde(rename = "rangeStart")]
    pub range_start: f64,
    #[serde(rename = "rangeEnd")]
    pub range_end: f64,
    #[serde(rename = "falloffEnd")]
    pub falloff_end: f64,
}

impl HueBand {
    fn forward(from: f64, to: f64) -> f64 {
        let d = (to - from) % 360.0;
        if d < 0.0 {
            d + 360.0
        } else {
            d
        }
    }
    pub fn weight(&self, hue: f64) -> f64 {
        let span = Self::forward(self.falloff_start, self.falloff_end);
        if span <= 0.0 {
            return 1.0;
        }
        let position = Self::forward(self.falloff_start, hue);
        if position > span {
            return 0.0;
        }
        let ramp_in = Self::forward(self.falloff_start, self.range_start);
        let plateau_end = Self::forward(self.falloff_start, self.range_end);
        if position < ramp_in {
            return if ramp_in > 0.0 { position / ramp_in } else { 1.0 };
        }
        if position <= plateau_end {
            return 1.0;
        }
        let ramp_out = span - plateau_end;
        if ramp_out > 0.0 {
            (span - position) / ramp_out
        } else {
            1.0
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct RangeAdjustment {
    #[serde(default)]
    pub hue: f64,
    #[serde(default)]
    pub saturation: f64,
    #[serde(default)]
    pub lightness: f64,
}

/// Swift encodes a dictionary keyed by an enum as `[key, value, key, value, …]`; read that or a
/// plain object, and write the array form so the Mac app reads it back.
mod range_map {
    use super::ColorRange;
    use serde::de::DeserializeOwned;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use serde_json::Value;
    use std::collections::BTreeMap;

    pub fn serialize<T: Serialize, S: Serializer>(map: &BTreeMap<ColorRange, T>, s: S) -> Result<S::Ok, S::Error> {
        let mut out: Vec<Value> = Vec::new();
        for (k, v) in map {
            out.push(serde_json::to_value(k).map_err(serde::ser::Error::custom)?);
            out.push(serde_json::to_value(v).map_err(serde::ser::Error::custom)?);
        }
        out.serialize(s)
    }
    pub fn deserialize<'de, T: DeserializeOwned, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<ColorRange, T>, D::Error> {
        let v = Value::deserialize(d)?;
        let mut map = BTreeMap::new();
        match v {
            Value::Array(items) => {
                for pair in items.chunks(2) {
                    if let [k, v] = pair {
                        let k: ColorRange = serde_json::from_value(k.clone()).map_err(serde::de::Error::custom)?;
                        let v: T = serde_json::from_value(v.clone()).map_err(serde::de::Error::custom)?;
                        map.insert(k, v);
                    }
                }
            }
            Value::Object(o) => {
                for (k, v) in o {
                    let k: ColorRange = serde_json::from_value(Value::String(k)).map_err(serde::de::Error::custom)?;
                    map.insert(k, serde_json::from_value(v).map_err(serde::de::Error::custom)?);
                }
            }
            _ => return Err(serde::de::Error::custom("expected a dictionary")),
        }
        Ok(map)
    }
}

fn default_bands() -> BTreeMap<ColorRange, HueBand> {
    ColorRange::ALL.iter().map(|r| (*r, r.default_band())).collect()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HueSaturationSettings {
    #[serde(default = "master")]
    pub range: ColorRange,
    #[serde(default)]
    pub colorize: bool,
    #[serde(default, rename = "invertRange")]
    pub invert_range: bool,
    #[serde(default, with = "range_map")]
    pub adjustments: BTreeMap<ColorRange, RangeAdjustment>,
    #[serde(default = "default_bands", with = "range_map")]
    pub bands: BTreeMap<ColorRange, HueBand>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn master() -> ColorRange {
    ColorRange::Master
}

impl HueSaturationSettings {
    pub fn new(hue: f64, saturation: f64, lightness: f64, colorize: bool) -> Self {
        let mut adjustments = BTreeMap::new();
        adjustments.insert(ColorRange::Master, RangeAdjustment { hue, saturation, lightness });
        Self { range: ColorRange::Master, colorize, invert_range: false, adjustments, bands: default_bands(), extra: Map::new() }
    }
    pub fn current(&self) -> RangeAdjustment {
        self.adjustments.get(&self.range).copied().unwrap_or_default()
    }
    pub fn current_mut(&mut self) -> &mut RangeAdjustment {
        self.adjustments.entry(self.range).or_default()
    }
    fn weight(&self, range: ColorRange, hue: f64) -> f64 {
        if range == ColorRange::Master {
            return 1.0;
        }
        let w = self.bands.get(&range).copied().unwrap_or(range.default_band()).weight(hue);
        if self.invert_range && range == self.range {
            1.0 - w
        } else {
            w
        }
    }
    pub fn is_identity(&self) -> bool {
        !self.colorize && self.adjustments.values().all(|a| *a == RangeAdjustment::default())
    }
    /// The 33³ lookup the C kernel applies: every corner converted to HSL, adjusted, and back.
    pub fn cube(&self) -> Vec<f32> {
        const N: usize = 33;
        let response: Vec<(f64, f64, f64)> = (0..=360)
            .map(|deg| {
                let mut r = (0.0, 0.0, 0.0);
                for (range, a) in &self.adjustments {
                    if *a == RangeAdjustment::default() {
                        continue;
                    }
                    let w = self.weight(*range, deg as f64);
                    if w > 0.0 {
                        r.0 += a.hue * w;
                        r.1 += a.saturation * w;
                        r.2 += a.lightness * w;
                    }
                }
                r
            })
            .collect();
        let current = self.current();
        let mut values = vec![0f32; N * N * N * 4];
        values.par_chunks_mut(N * N * 4).enumerate().for_each(|(b, plane)| {
            let step = (N - 1) as f64;
            let mut i = 0;
            for g in 0..N {
                for r in 0..N {
                    let (mut h, mut s, mut l) = to_hsl(r as f64 / step, g as f64 / step, b as f64 / step);
                    let lightness_amount;
                    if self.colorize {
                        h = current.hue % 360.0;
                        s = (current.saturation / 100.0).clamp(0.0, 1.0);
                        lightness_amount = current.lightness / 100.0;
                    } else {
                        let sampled = response[(h.round() as usize).min(360)];
                        lightness_amount = sampled.2 / 100.0;
                        h = (h + sampled.0) % 360.0;
                        if h < 0.0 {
                            h += 360.0;
                        }
                        s = adjusted_saturation(s, sampled.1);
                    }
                    let amount = lightness_amount.clamp(-1.0, 1.0);
                    l = if amount >= 0.0 { l + (1.0 - l) * amount } else { l * (1.0 + amount) };
                    let (rr, gg, bb) = to_rgb(h, s, l.clamp(0.0, 1.0));
                    plane[i] = rr as f32;
                    plane[i + 1] = gg as f32;
                    plane[i + 2] = bb as f32;
                    plane[i + 3] = 1.0;
                    i += 4;
                }
            }
        });
        values
    }
}

fn adjusted_saturation(s: f64, amount: f64) -> f64 {
    let amount = (amount / 100.0).clamp(-1.0, 1.0);
    if amount <= 0.0 {
        return (s * (1.0 + amount)).max(0.0);
    }
    if amount >= 1.0 {
        if s > 0.0 {
            1.0
        } else {
            0.0
        }
    } else {
        (s / (1.0 - amount)).min(1.0)
    }
}

fn to_hsl(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let high = r.max(g).max(b);
    let low = r.min(g).min(b);
    let l = (high + low) / 2.0;
    let delta = high - low;
    if delta <= 0.0 {
        return (0.0, 0.0, l);
    }
    let s = delta / (1.0 - (2.0 * l - 1.0).abs());
    let mut h = if high == r {
        (g - b) / delta
    } else if high == g {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    };
    h *= 60.0;
    if h < 0.0 {
        h += 360.0;
    }
    (h, s.min(1.0), l)
}

fn to_rgb(h: f64, s: f64, l: f64) -> (f64, f64, f64) {
    if s <= 0.0 {
        return (l, l, l);
    }
    let chroma = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let sector = h / 60.0;
    let second = chroma * (1.0 - ((sector % 2.0) - 1.0).abs());
    let base = l - chroma / 2.0;
    let (r, g, b) = match sector as i64 {
        0 => (chroma, second, 0.0),
        1 => (second, chroma, 0.0),
        2 => (0.0, chroma, second),
        3 => (0.0, second, chroma),
        4 => (second, 0.0, chroma),
        _ => (chroma, 0.0, second),
    };
    ((r + base).clamp(0.0, 1.0), (g + base).clamp(0.0, 1.0), (b + base).clamp(0.0, 1.0))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LevelsChannel {
    #[default]
    #[serde(rename = "RGB")]
    Rgb,
    Red,
    Green,
    Blue,
}

impl LevelsChannel {
    pub const ALL: [LevelsChannel; 4] = [LevelsChannel::Rgb, LevelsChannel::Red, LevelsChannel::Green, LevelsChannel::Blue];
    pub fn index(self) -> usize {
        self as usize
    }
    pub fn label(self) -> &'static str {
        ["RGB", "Red", "Green", "Blue"][self as usize]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LevelRange {
    #[serde(default)]
    pub black: f64,
    #[serde(default = "one")]
    pub gamma: f64,
    #[serde(default = "full")]
    pub white: f64,
    #[serde(default, rename = "outputBlack")]
    pub output_black: f64,
    #[serde(default = "full", rename = "outputWhite")]
    pub output_white: f64,
}

fn one() -> f64 {
    1.0
}
fn full() -> f64 {
    255.0
}

impl Default for LevelRange {
    fn default() -> Self {
        Self { black: 0.0, gamma: 1.0, white: 255.0, output_black: 0.0, output_white: 255.0 }
    }
}

impl LevelRange {
    pub fn normalized(&self) -> Self {
        let clamp = |n: f64, lo: f64, hi: f64, fallback: f64| if n.is_finite() { n.clamp(lo, hi) } else { fallback };
        let black = clamp(self.black, 0.0, 254.0, 0.0);
        Self {
            black,
            white: clamp(self.white, black + 1.0, 255.0, 255.0),
            gamma: clamp(self.gamma, 0.1, 9.99, 1.0),
            output_black: clamp(self.output_black, 0.0, 255.0, 0.0),
            output_white: clamp(self.output_white, 0.0, 255.0, 255.0),
        }
    }
    pub fn apply(&self, value: f64) -> f64 {
        let s = self.normalized();
        let input = ((value * 255.0 - s.black) / (s.white - s.black)).clamp(0.0, 1.0);
        (s.output_black + input.powf(1.0 / s.gamma) * (s.output_white - s.output_black)) / 255.0
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LevelsSettings {
    #[serde(default)]
    pub channel: LevelsChannel,
    #[serde(default = "default_ranges")]
    pub ranges: Vec<LevelRange>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn default_ranges() -> Vec<LevelRange> {
    vec![LevelRange::default(); 4]
}

impl Default for LevelsSettings {
    fn default() -> Self {
        Self { channel: LevelsChannel::Rgb, ranges: default_ranges(), extra: Map::new() }
    }
}

impl LevelsSettings {
    pub fn is_identity(&self) -> bool {
        self.ranges.iter().all(|r| r.normalized() == LevelRange::default())
    }
    /// Individual channels, followed by the composite RGB adjustment.
    pub fn tables(&self) -> Vec<f32> {
        let mut t = Vec::with_capacity(768);
        for channel in 1..4 {
            for i in 0..256 {
                let v = self.ranges[0].apply(self.ranges[channel].apply(i as f64 / 255.0));
                t.push(v as f32);
            }
        }
        t
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CurvesSettings {
    #[serde(default)]
    pub channel: LevelsChannel,
    #[serde(default = "default_curves")]
    pub channels: Vec<Vec<CurvePoint>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn default_curves() -> Vec<Vec<CurvePoint>> {
    vec![vec![CurvePoint { x: 0.0, y: 0.0 }, CurvePoint { x: 255.0, y: 255.0 }]; 4]
}

impl Default for CurvesSettings {
    fn default() -> Self {
        Self { channel: LevelsChannel::Rgb, channels: default_curves(), extra: Map::new() }
    }
}

impl CurvesSettings {
    pub fn is_valid(&self) -> bool {
        self.channels.len() == 4
            && self.channels.iter().all(|p| {
                (2..=32).contains(&p.len())
                    && p.first().is_some_and(|f| f.x == 0.0)
                    && p.last().is_some_and(|l| l.x == 255.0)
                    && p.iter().all(|c| (0.0..=255.0).contains(&c.x) && (0.0..=255.0).contains(&c.y))
                    && p.windows(2).all(|w| w[0].x < w[1].x)
            })
    }
    /// Shape-preserving cubic Hermite interpolation, which avoids overshoot between handles.
    pub fn value(&self, x: f64, channel: usize) -> f64 {
        let p = &self.channels[channel];
        let n = p.len();
        let i = p.iter().rposition(|c| c.x <= x).unwrap_or(0).min(n - 2);
        let d: Vec<f64> = p.windows(2).map(|w| (w[1].y - w[0].y) / (w[1].x - w[0].x)).collect();
        let slope = |j: usize| -> f64 {
            if j == 0 {
                return d[0];
            }
            if j == n - 1 {
                return d[d.len() - 1];
            }
            if d[j - 1] * d[j] <= 0.0 {
                return 0.0;
            }
            2.0 / (1.0 / d[j - 1] + 1.0 / d[j])
        };
        let h = p[i + 1].x - p[i].x;
        let t = ((x - p[i].x) / h).clamp(0.0, 1.0);
        let (t2, t3) = (t * t, t * t * t);
        let y = (2.0 * t3 - 3.0 * t2 + 1.0) * p[i].y
            + (t3 - 2.0 * t2 + t) * h * slope(i)
            + (-2.0 * t3 + 3.0 * t2) * p[i + 1].y
            + (t3 - t2) * h * slope(i + 1);
        y.clamp(0.0, 255.0)
    }
    pub fn tables(&self) -> Vec<f32> {
        let mut t = Vec::with_capacity(768);
        for channel in 1..4 {
            for i in 0..256 {
                t.push((self.value(self.value(i as f64, channel), 0) / 255.0) as f32);
            }
        }
        t
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExposureSettings {
    #[serde(default)]
    pub exposure: f64,
    #[serde(default)]
    pub offset: f64,
    #[serde(default = "one")]
    pub gamma: f64,
}

impl Default for ExposureSettings {
    fn default() -> Self {
        Self { exposure: 0.0, offset: 0.0, gamma: 1.0 }
    }
}

impl ExposureSettings {
    pub fn table(&self) -> Vec<f32> {
        let scale = 2f64.powf(self.exposure);
        let one: Vec<f32> = (0..256)
            .map(|i| {
                let e = i as f64 / 255.0;
                let mut lin = if e <= 0.04045 { e / 12.92 } else { ((e + 0.055) / 1.055).powf(2.4) };
                lin = (lin * scale + self.offset).max(0.0).powf(1.0 / self.gamma);
                let out = if lin <= 0.0031308 { lin * 12.92 } else { 1.055 * lin.powf(1.0 / 2.4) - 0.055 };
                out.clamp(0.0, 1.0) as f32
            })
            .collect();
        [one.clone(), one.clone(), one].concat()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdjustmentColor {
    pub red: f64,
    pub green: f64,
    pub blue: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GradientMapSettings {
    #[serde(default = "black")]
    pub shadows: AdjustmentColor,
    #[serde(default = "white")]
    pub highlights: AdjustmentColor,
    #[serde(default)]
    pub reversed: bool,
}

fn black() -> AdjustmentColor {
    AdjustmentColor { red: 0.0, green: 0.0, blue: 0.0 }
}
fn white() -> AdjustmentColor {
    AdjustmentColor { red: 1.0, green: 1.0, blue: 1.0 }
}

impl Default for GradientMapSettings {
    fn default() -> Self {
        Self { shadows: black(), highlights: white(), reversed: false }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GrainSettings {
    #[serde(default = "grain_amount")]
    pub amount: f64,
    #[serde(default = "grain_size")]
    pub size: f64,
    #[serde(default = "grain_roughness")]
    pub roughness: f64,
    #[serde(default)]
    pub seed: u32,
}

fn grain_amount() -> f64 {
    25.0
}
fn grain_size() -> f64 {
    1.5
}
fn grain_roughness() -> f64 {
    50.0
}

impl Default for GrainSettings {
    fn default() -> Self {
        Self { amount: 25.0, size: 1.5, roughness: 50.0, seed: 0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlackWhiteSettings {
    pub reds: f64,
    pub yellows: f64,
    pub greens: f64,
    pub cyans: f64,
    pub blues: f64,
    pub magentas: f64,
    #[serde(default)]
    pub tint: bool,
    #[serde(default = "tint_hue", rename = "tintHue")]
    pub tint_hue: f64,
    #[serde(default = "tint_saturation", rename = "tintSaturation")]
    pub tint_saturation: f64,
}

fn tint_hue() -> f64 {
    40.0
}
fn tint_saturation() -> f64 {
    20.0
}

impl Default for BlackWhiteSettings {
    fn default() -> Self {
        Self {
            reds: 40.0,
            yellows: 60.0,
            greens: 40.0,
            cyans: 60.0,
            blues: 20.0,
            magentas: 80.0,
            tint: false,
            tint_hue: 40.0,
            tint_saturation: 20.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColorBalanceSettings {
    #[serde(default)]
    pub shadow_cyan_red: f64,
    #[serde(default)]
    pub shadow_magenta_green: f64,
    #[serde(default)]
    pub shadow_yellow_blue: f64,
    #[serde(default)]
    pub mid_cyan_red: f64,
    #[serde(default)]
    pub mid_magenta_green: f64,
    #[serde(default)]
    pub mid_yellow_blue: f64,
    #[serde(default)]
    pub highlight_cyan_red: f64,
    #[serde(default)]
    pub highlight_magenta_green: f64,
    #[serde(default)]
    pub highlight_yellow_blue: f64,
    #[serde(default = "yes")]
    pub preserve_luminosity: bool,
}

fn yes() -> bool {
    true
}

impl Default for ColorBalanceSettings {
    fn default() -> Self {
        Self {
            shadow_cyan_red: 0.0,
            shadow_magenta_green: 0.0,
            shadow_yellow_blue: 0.0,
            mid_cyan_red: 0.0,
            mid_magenta_green: 0.0,
            mid_yellow_blue: 0.0,
            highlight_cyan_red: 0.0,
            highlight_magenta_green: 0.0,
            highlight_yellow_blue: 0.0,
            preserve_luminosity: true,
        }
    }
}

/// An adjustment layer's settings. Every kind's settings live on the one record, as on the Mac.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Adjustment {
    pub kind: AdjustmentKind,
    #[serde(default)]
    pub hue: f64,
    #[serde(default)]
    pub saturation: f64,
    #[serde(default)]
    pub lightness: f64,
    #[serde(default)]
    pub colorize: bool,
    #[serde(default, rename = "hsvSettings", skip_serializing_if = "Option::is_none")]
    pub hsv_settings: Option<HueSaturationSettings>,
    #[serde(default)]
    pub levels: LevelsSettings,
    #[serde(default)]
    pub curves: CurvesSettings,
    #[serde(default, rename = "exposureSettings", skip_serializing_if = "Option::is_none")]
    pub exposure_settings: Option<ExposureSettings>,
    #[serde(default, rename = "gradientMapSettings", skip_serializing_if = "Option::is_none")]
    pub gradient_map_settings: Option<GradientMapSettings>,
    #[serde(default, rename = "grainSettings", skip_serializing_if = "Option::is_none")]
    pub grain_settings: Option<GrainSettings>,
    #[serde(default, rename = "blackWhiteSettings", skip_serializing_if = "Option::is_none")]
    pub black_white_settings: Option<BlackWhiteSettings>,
    #[serde(default, rename = "colorBalanceSettings", skip_serializing_if = "Option::is_none")]
    pub color_balance_settings: Option<ColorBalanceSettings>,
    #[serde(default, rename = "blurRadius", skip_serializing_if = "Option::is_none")]
    pub blur_radius: Option<f64>,
    #[serde(default, rename = "motionAngle", skip_serializing_if = "Option::is_none")]
    pub motion_angle: Option<f64>,
    #[serde(default, rename = "motionDistance", skip_serializing_if = "Option::is_none")]
    pub motion_distance: Option<f64>,
    #[serde(default, rename = "noiseAmount", skip_serializing_if = "Option::is_none")]
    pub noise_amount: Option<f64>,
    #[serde(default, rename = "noiseGaussian", skip_serializing_if = "Option::is_none")]
    pub noise_gaussian: Option<bool>,
    #[serde(default, rename = "noiseMonochromatic", skip_serializing_if = "Option::is_none")]
    pub noise_monochromatic: Option<bool>,
    #[serde(default, rename = "noiseSeed", skip_serializing_if = "Option::is_none")]
    pub noise_seed: Option<u32>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Where a buffer sits on the document, for kernels whose result depends on position or scale.
#[derive(Clone, Copy, Debug)]
pub struct Placement {
    /// Document position of the buffer's top-left corner.
    pub origin_x: f64,
    pub origin_y: f64,
    /// Buffer pixels per document pixel.
    pub scale: f64,
}

impl Adjustment {
    pub fn new(kind: AdjustmentKind) -> Self {
        let mut a = Self {
            kind,
            hue: 0.0,
            saturation: 0.0,
            lightness: 0.0,
            colorize: false,
            hsv_settings: None,
            levels: LevelsSettings::default(),
            curves: CurvesSettings::default(),
            exposure_settings: None,
            gradient_map_settings: None,
            grain_settings: None,
            black_white_settings: None,
            color_balance_settings: None,
            blur_radius: None,
            motion_angle: None,
            motion_distance: None,
            noise_amount: None,
            noise_gaussian: None,
            noise_monochromatic: None,
            noise_seed: None,
            extra: Map::new(),
        };
        let seed = || uuid::Uuid::new_v4().as_u128() as u32;
        match kind {
            AdjustmentKind::Grain => a.grain_settings = Some(GrainSettings { seed: seed(), ..Default::default() }),
            AdjustmentKind::AddNoise => a.noise_seed = Some(seed()),
            _ => {}
        }
        a
    }
    pub fn hsv(&self) -> HueSaturationSettings {
        self.hsv_settings
            .clone()
            .unwrap_or_else(|| HueSaturationSettings::new(self.hue, self.saturation, self.lightness, self.colorize))
    }
    pub fn exposure(&self) -> ExposureSettings {
        self.exposure_settings.unwrap_or_default()
    }
    pub fn gradient_map(&self) -> GradientMapSettings {
        self.gradient_map_settings.unwrap_or_default()
    }
    pub fn grain(&self) -> GrainSettings {
        self.grain_settings.unwrap_or_default()
    }
    pub fn black_white(&self) -> BlackWhiteSettings {
        self.black_white_settings.unwrap_or_default()
    }
    pub fn color_balance(&self) -> ColorBalanceSettings {
        self.color_balance_settings.unwrap_or_default()
    }
    pub fn gaussian_radius(&self) -> f64 {
        self.blur_radius.unwrap_or(10.0)
    }
    pub fn motion(&self) -> (f64, f64) {
        (self.motion_angle.unwrap_or(0.0), self.motion_distance.unwrap_or(10.0))
    }
    /// Document pixels a partial render must read beyond what it shows.
    pub fn sampling_margin(&self) -> f64 {
        match self.kind {
            AdjustmentKind::GaussianBlur => self.gaussian_radius() * 3.0 + 2.0,
            AdjustmentKind::MotionBlur => self.motion().1 / 2.0 + 2.0,
            _ => 0.0,
        }
    }

    /// Changes `buf` in place.
    pub fn apply(&self, buf: &mut Rgba, at: Placement) {
        let (w, h) = (buf.w, buf.h);
        if w == 0 || h == 0 {
            return;
        }
        let count = w * h;
        let stride = w * 4;
        let ptr = buf.data.as_mut_ptr();
        // SAFETY: every kernel reads and writes `w × h` RGBA pixels at `stride` bytes per row, which is
        // exactly the buffer; the tables are the sizes the C headers document.
        unsafe {
            match self.kind {
                AdjustmentKind::Hsv => {
                    let s = self.hsv();
                    if !s.is_identity() {
                        let cube = s.cube();
                        in_bands(buf, |p, n| ffi::cube_apply(p, n, cube.as_ptr(), 33));
                    }
                }
                AdjustmentKind::Levels => {
                    if !self.levels.is_identity() {
                        let t = self.levels.tables();
                        in_bands(buf, |p, n| ffi::levels_apply(p, n, t.as_ptr()));
                    }
                }
                AdjustmentKind::Curves => {
                    if self.curves.is_valid() {
                        let t = self.curves.tables();
                        in_bands(buf, |p, n| ffi::levels_apply(p, n, t.as_ptr()));
                    }
                }
                AdjustmentKind::Exposure => {
                    let t = self.exposure().table();
                    in_bands(buf, |p, n| ffi::levels_apply(p, n, t.as_ptr()));
                }
                AdjustmentKind::GradientMap => {
                    let g = self.gradient_map();
                    let (dark, light) = if g.reversed { (g.highlights, g.shadows) } else { (g.shadows, g.highlights) };
                    let mut table = Vec::with_capacity(768);
                    for i in 0..256 {
                        let t = i as f64 / 255.0;
                        for (a, b) in [(dark.red, light.red), (dark.green, light.green), (dark.blue, light.blue)] {
                            table.push(((a + (b - a) * t) * 255.0).round().clamp(0.0, 255.0) as u8);
                        }
                    }
                    ffi::adjust_gradient_map(ptr, w, h, stride, table.as_ptr());
                }
                AdjustmentKind::Grain => {
                    let g = self.grain();
                    if g.amount > 0.0 {
                        ffi::adjust_grain(ptr, w, h, stride, g.amount, g.size, g.roughness, g.seed, at.origin_x, at.origin_y, 1.0 / at.scale);
                    }
                }
                AdjustmentKind::BlackWhite => {
                    let b = self.black_white();
                    let weights: Vec<f32> =
                        [b.reds, b.yellows, b.greens, b.cyans, b.blues, b.magentas].iter().map(|v| (*v / 100.0) as f32).collect();
                    ffi::adjust_black_white(ptr, w, h, stride, weights.as_ptr(), b.tint as i32, b.tint_hue, b.tint_saturation / 100.0);
                }
                AdjustmentKind::ColorBalance => {
                    let c = self.color_balance();
                    let f = |v: f64| (v / 100.0) as f32;
                    let s = [f(c.shadow_cyan_red), f(c.shadow_magenta_green), f(c.shadow_yellow_blue)];
                    let m = [f(c.mid_cyan_red), f(c.mid_magenta_green), f(c.mid_yellow_blue)];
                    let hl = [f(c.highlight_cyan_red), f(c.highlight_magenta_green), f(c.highlight_yellow_blue)];
                    if s.iter().chain(&m).chain(&hl).any(|v| *v != 0.0) {
                        ffi::adjust_color_balance(ptr, w, h, stride, s.as_ptr(), m.as_ptr(), hl.as_ptr(), c.preserve_luminosity as i32);
                    }
                }
                AdjustmentKind::AddNoise => {
                    ffi::noise_add_at(
                        ptr,
                        w,
                        h,
                        stride,
                        self.noise_amount.unwrap_or(10.0) as f32,
                        self.noise_gaussian.unwrap_or(false) as i32,
                        self.noise_monochromatic.unwrap_or(false) as i32,
                        self.noise_seed.unwrap_or(0),
                        (at.origin_x * at.scale).round() as i64,
                        (at.origin_y * at.scale).round() as i64,
                    );
                }
                AdjustmentKind::Invert => {
                    buf.data.par_chunks_mut(4).for_each(|p| {
                        let a = p[3];
                        p[0] = a.saturating_sub(p[0]);
                        p[1] = a.saturating_sub(p[1]);
                        p[2] = a.saturating_sub(p[2]);
                    });
                }
                AdjustmentKind::GaussianBlur => gaussian_blur(buf, self.gaussian_radius() * at.scale),
                AdjustmentKind::MotionBlur => {
                    let (angle, distance) = self.motion();
                    motion_blur(buf, angle, distance * at.scale);
                }
            }
        }
        let _ = count;
    }
}

/// Splits a per-pixel kernel across the cores.
unsafe fn in_bands(buf: &mut Rgba, f: impl Fn(*mut u8, usize) + Sync) {
    let rows_per_band = (buf.h / (rayon::current_num_threads() * 4)).max(16);
    let band_bytes = rows_per_band * buf.w * 4;
    buf.data.par_chunks_mut(band_bytes).for_each(|band| f(band.as_mut_ptr(), band.len() / 4));
}

/// Three box passes approximate a Gaussian with standard deviation `sigma`, edges clamped.
pub fn gaussian_blur(buf: &mut Rgba, sigma: f64) {
    if sigma < 0.3 {
        return;
    }
    // Box widths for three passes (Kovesi's construction).
    let n = 3.0;
    let ideal = ((12.0 * sigma * sigma / n) + 1.0).sqrt();
    let mut wl = ideal.floor() as i64;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wu = wl + 2;
    let m = ((12.0 * sigma * sigma - n * (wl * wl) as f64 - 4.0 * n * wl as f64 - 3.0 * n) / (-4.0 * wl as f64 - 4.0)).round() as i64;
    let radii: Vec<usize> = (0..3).map(|i| (((if i < m { wl } else { wu }) - 1) / 2).max(0) as usize).collect();
    let (w, h) = (buf.w, buf.h);
    let mut plane: Vec<f32> = buf.data.iter().map(|&v| v as f32).collect();
    let mut tmp = vec![0f32; plane.len()];
    for r in radii {
        if r == 0 {
            continue;
        }
        box_horizontal(&plane, &mut tmp, w, h, r);
        box_vertical(&tmp, &mut plane, w, h, r);
    }
    buf.data.par_iter_mut().zip(plane.par_iter()).for_each(|(d, s)| *d = s.round().clamp(0.0, 255.0) as u8);
    // Premultiplied colors must not exceed their alpha after rounding.
    buf.data.par_chunks_mut(4).for_each(|p| {
        let a = p[3];
        for c in &mut p[..3] {
            *c = (*c).min(a);
        }
    });
}

fn box_horizontal(src: &[f32], dst: &mut [f32], w: usize, _h: usize, r: usize) {
    let norm = 1.0 / (2 * r + 1) as f32;
    dst.par_chunks_mut(w * 4).zip(src.par_chunks(w * 4)).for_each(|(out, row)| {
        for c in 0..4 {
            let at = |x: i64| row[(x.clamp(0, w as i64 - 1) as usize) * 4 + c];
            let mut sum: f32 = (-(r as i64)..=r as i64).map(at).sum();
            for x in 0..w {
                out[x * 4 + c] = sum * norm;
                sum += at(x as i64 + r as i64 + 1) - at(x as i64 - r as i64);
            }
        }
    });
}

fn box_vertical(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize) {
    let norm = 1.0 / (2 * r + 1) as f32;
    // Column blocks so each worker walks its own strip of columns.
    let cols_per = 64usize;
    let blocks: Vec<usize> = (0..w).step_by(cols_per).collect();
    let results: Vec<(usize, Vec<f32>)> = blocks
        .par_iter()
        .map(|&x0| {
            let x1 = (x0 + cols_per).min(w);
            let bw = x1 - x0;
            let mut out = vec![0f32; bw * h * 4];
            for x in x0..x1 {
                for c in 0..4 {
                    let at = |y: i64| src[((y.clamp(0, h as i64 - 1) as usize) * w + x) * 4 + c];
                    let mut sum: f32 = (-(r as i64)..=r as i64).map(at).sum();
                    for y in 0..h {
                        out[(y * bw + (x - x0)) * 4 + c] = sum * norm;
                        sum += at(y as i64 + r as i64 + 1) - at(y as i64 - r as i64);
                    }
                }
            }
            (x0, out)
        })
        .collect();
    for (x0, out) in results {
        let bw = out.len() / (h * 4);
        for y in 0..h {
            let d = (y * w + x0) * 4;
            dst[d..d + bw * 4].copy_from_slice(&out[y * bw * 4..(y + 1) * bw * 4]);
        }
    }
}

/// Averages along a line `distance` pixels long at `angle` degrees, edges clamped.
pub fn motion_blur(buf: &mut Rgba, angle: f64, distance: f64) {
    if distance < 1.0 {
        return;
    }
    let taps = (distance.ceil() as usize).clamp(2, 96);
    let (s, c) = angle.to_radians().sin_cos();
    // Image y runs down, and a positive angle tilts up, as in Photoshop.
    let (dx, dy) = (c, -s);
    let src = buf.clone();
    let (w, h) = (src.w, src.h);
    buf.data.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let mut acc = [0f32; 4];
            for t in 0..taps {
                let f = (t as f64 / (taps - 1) as f64 - 0.5) * distance;
                let sx = (x as f64 + 0.5 + dx * f).clamp(0.5, w as f64 - 0.5);
                let sy = (y as f64 + 0.5 + dy * f).clamp(0.5, h as f64 - 0.5);
                let p = crate::raster::sample_rgba(&src, sx, sy);
                for k in 0..4 {
                    acc[k] += p[k];
                }
            }
            for k in 0..4 {
                row[x * 4 + k] = (acc[k] / taps as f32).round().clamp(0.0, 255.0) as u8;
            }
            let a = row[x * 4 + 3];
            for k in 0..3 {
                row[x * 4 + k] = row[x * 4 + k].min(a);
            }
        }
    });
}
