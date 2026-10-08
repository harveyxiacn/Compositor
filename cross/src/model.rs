//! The document: layers, folders, masks and adjustments, mirroring the macOS app's model so a
//! `.comp` project reads and writes the same way on every platform.

use crate::adjust::Adjustment;
use crate::geom::{Affine, Point, Rect};
use crate::raster::{Gray, MaskAsset, PixelAsset};
use crate::selection::Selection;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use uuid::Uuid;

pub const MAX_SIDE: usize = 30_000;
pub const MAX_LAYERS: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Sampling {
    #[serde(rename = "Nearest")]
    Nearest,
    #[serde(rename = "Smooth")]
    Smooth,
    #[default]
    #[serde(rename = "High quality")]
    High,
}

impl Sampling {
    pub const ALL: [Sampling; 3] = [Sampling::Nearest, Sampling::Smooth, Sampling::High];
    pub fn label(self) -> &'static str {
        match self {
            Sampling::Nearest => "Nearest",
            Sampling::Smooth => "Smooth",
            Sampling::High => "High quality",
        }
    }
}

/// Unrotated bounds in document pixels; rotation is clockwise around their center, in degrees.
/// Points and sizes are stored as `[x, y]` and `[width, height]`, as Core Graphics encodes them.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerTransform {
    #[serde(with = "pair")]
    pub origin: Point,
    #[serde(with = "pair_size")]
    pub size: (f64, f64),
    #[serde(default)]
    pub rotation: f64,
    #[serde(default, rename = "flipX")]
    pub flip_x: bool,
    #[serde(default, rename = "flipY")]
    pub flip_y: bool,
    #[serde(default)]
    pub sampling: Sampling,
}

mod pair {
    use crate::geom::Point;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(p: &Point, s: S) -> Result<S::Ok, S::Error> {
        [p.x, p.y].serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Point, D::Error> {
        let v = super::PairOrObject::deserialize(d)?;
        Ok(Point::new(v.0, v.1))
    }
}

mod pair_size {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(p: &(f64, f64), s: S) -> Result<S::Ok, S::Error> {
        [p.0, p.1].serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<(f64, f64), D::Error> {
        let v = super::PairOrObject::deserialize(d)?;
        Ok((v.0, v.1))
    }
}

/// `[a, b]`, or the keyed form `{"x": a, "y": b}` / `{"width": a, "height": b}`.
struct PairOrObject(f64, f64);

impl<'de> Deserialize<'de> for PairOrObject {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        let num = |v: Option<&Value>| v.and_then(Value::as_f64);
        let pair = match &v {
            Value::Array(a) if a.len() == 2 => num(a.first()).zip(num(a.get(1))),
            Value::Object(o) => num(o.get("x"))
                .zip(num(o.get("y")))
                .or_else(|| num(o.get("width")).zip(num(o.get("height")))),
            _ => None,
        };
        pair.map(|(a, b)| PairOrObject(a, b)).ok_or_else(|| serde::de::Error::custom("expected a pair of numbers"))
    }
}

impl LayerTransform {
    pub fn new(origin: Point, w: f64, h: f64) -> Self {
        Self { origin, size: (w, h), rotation: 0.0, flip_x: false, flip_y: false, sampling: Sampling::High }
    }
    pub fn center(&self) -> Point {
        Point::new(self.origin.x + self.size.0 / 2.0, self.origin.y + self.size.1 / 2.0)
    }
    pub fn radians(&self) -> f64 {
        (self.rotation % 360.0).to_radians()
    }
    pub fn is_valid(&self) -> bool {
        [self.origin.x, self.origin.y, self.size.0, self.size.1, self.rotation].iter().all(|v| v.is_finite())
            && (1.0..=300_000.0).contains(&self.size.0)
            && (1.0..=300_000.0).contains(&self.size.1)
            && self.origin.x.abs() <= 1_000_000.0
            && self.origin.y.abs() <= 1_000_000.0
    }
    /// A point of the unit square (0…1, y down) placed on the document.
    pub fn point(&self, unit: Point) -> Point {
        let c = self.center();
        let (s, co) = self.radians().sin_cos();
        let x = (unit.x - 0.5) * self.size.0;
        let y = (unit.y - 0.5) * self.size.1;
        Point::new(c.x + x * co - y * s, c.y + x * s + y * co)
    }
    pub fn contains(&self, p: Point) -> bool {
        let c = self.center();
        let (s, co) = self.radians().sin_cos();
        let (x, y) = (p.x - c.x, p.y - c.y);
        (x * co + y * s).abs() <= self.size.0 / 2.0 && (-x * s + y * co).abs() <= self.size.1 / 2.0
    }
    pub fn corners(&self) -> [Point; 4] {
        [
            self.point(Point::new(0.0, 0.0)),
            self.point(Point::new(1.0, 0.0)),
            self.point(Point::new(1.0, 1.0)),
            self.point(Point::new(0.0, 1.0)),
        ]
    }
    pub fn bounds(&self) -> Rect {
        Rect::bounding(&self.corners())
    }
    /// Maps a `width` × `height` pixel grid onto the document where this transform places it.
    pub fn pixel_to_document(&self, width: usize, height: usize) -> Affine {
        Affine::translation(self.center().x, self.center().y)
            .after(&Affine::rotation(self.radians()))
            .after(&Affine::scale(
                self.size.0 / width.max(1) as f64 * if self.flip_x { -1.0 } else { 1.0 },
                self.size.1 / height.max(1) as f64 * if self.flip_y { -1.0 } else { 1.0 },
            ))
            .after(&Affine::translation(-(width as f64) / 2.0, -(height as f64) / 2.0))
    }
    pub fn unit_to_document(&self) -> Affine {
        self.pixel_to_document(1, 1)
    }
    /// Whole pixels and whole degrees, what dragging leaves behind.
    pub fn rounded(&self) -> Self {
        let mut r = *self;
        r.origin = Point::new(self.origin.x.round(), self.origin.y.round());
        r.size = (self.size.0.round().max(1.0), self.size.1.round().max(1.0));
        r.rotation = self.rotation.round();
        r
    }
    /// A transform placing the unit square as `map` does (shear is dropped).
    pub fn placing(&self, map: &Affine) -> Self {
        let sign = if self.flip_x { -1.0 } else { 1.0 };
        let angle = (map.b * sign).atan2(map.a * sign);
        let along = -map.c * angle.sin() + map.d * angle.cos();
        let middle = map.apply(Point::new(0.5, 0.5));
        let mut r = *self;
        r.size = (map.a.hypot(map.b), along.abs());
        let degrees = angle.to_degrees();
        r.rotation = degrees + ((self.rotation - degrees) / 360.0).round() * 360.0;
        r.flip_y = along < 0.0;
        r.origin = Point::new(middle.x - r.size.0 / 2.0, middle.y - r.size.1 / 2.0);
        r
    }
    /// This placement carried along as a layer moves from `old` to `new`.
    pub fn following(&self, old: &LayerTransform, new: &LayerTransform) -> Self {
        if old == new {
            return *self;
        }
        if old.size == new.size && old.rotation == new.rotation && old.flip_x == new.flip_x && old.flip_y == new.flip_y {
            let mut moved = *self;
            moved.origin.x += new.origin.x - old.origin.x;
            moved.origin.y += new.origin.y - old.origin.y;
            return moved;
        }
        let Some(inv) = old.unit_to_document().inverted() else { return *self };
        self.placing(&self.unit_to_document().then(&inv).then(&new.unit_to_document()))
    }
    pub fn same_placement(&self, other: &LayerTransform) -> bool {
        let mut copy = *self;
        copy.sampling = other.sampling;
        copy == *other
    }
    pub const HANDLES: [Point; 8] = [
        Point::new(0.0, 0.0),
        Point::new(0.5, 0.0),
        Point::new(1.0, 0.0),
        Point::new(1.0, 0.5),
        Point::new(1.0, 1.0),
        Point::new(0.5, 1.0),
        Point::new(0.0, 1.0),
        Point::new(0.0, 0.5),
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum BlendMode {
    #[default]
    Normal,
    Darken,
    Multiply,
    #[serde(rename = "Color Burn")]
    ColorBurn,
    #[serde(rename = "Linear Burn")]
    LinearBurn,
    Lighten,
    Screen,
    #[serde(rename = "Color Dodge")]
    ColorDodge,
    #[serde(rename = "Linear Dodge (Add)")]
    LinearDodge,
    Overlay,
    #[serde(rename = "Soft Light")]
    SoftLight,
    #[serde(rename = "Hard Light")]
    HardLight,
    #[serde(rename = "Vivid Light")]
    VividLight,
    #[serde(rename = "Linear Light")]
    LinearLight,
    #[serde(rename = "Pin Light")]
    PinLight,
    #[serde(rename = "Hard Mix")]
    HardMix,
    Difference,
    Exclusion,
    Subtract,
    Divide,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl BlendMode {
    /// Photoshop's grouping, which the menu separates with lines.
    pub const GROUPS: [&'static [BlendMode]; 6] = [
        &[BlendMode::Normal],
        &[BlendMode::Darken, BlendMode::Multiply, BlendMode::ColorBurn, BlendMode::LinearBurn],
        &[BlendMode::Lighten, BlendMode::Screen, BlendMode::ColorDodge, BlendMode::LinearDodge],
        &[
            BlendMode::Overlay,
            BlendMode::SoftLight,
            BlendMode::HardLight,
            BlendMode::VividLight,
            BlendMode::LinearLight,
            BlendMode::PinLight,
            BlendMode::HardMix,
        ],
        &[BlendMode::Difference, BlendMode::Exclusion, BlendMode::Subtract, BlendMode::Divide],
        &[BlendMode::Hue, BlendMode::Saturation, BlendMode::Color, BlendMode::Luminosity],
    ];
    pub fn all() -> impl Iterator<Item = BlendMode> {
        Self::GROUPS.iter().flat_map(|g| g.iter().copied())
    }
    pub fn label(self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Darken => "Darken",
            BlendMode::Multiply => "Multiply",
            BlendMode::ColorBurn => "Color Burn",
            BlendMode::LinearBurn => "Linear Burn",
            BlendMode::Lighten => "Lighten",
            BlendMode::Screen => "Screen",
            BlendMode::ColorDodge => "Color Dodge",
            BlendMode::LinearDodge => "Linear Dodge (Add)",
            BlendMode::Overlay => "Overlay",
            BlendMode::SoftLight => "Soft Light",
            BlendMode::HardLight => "Hard Light",
            BlendMode::VividLight => "Vivid Light",
            BlendMode::LinearLight => "Linear Light",
            BlendMode::PinLight => "Pin Light",
            BlendMode::HardMix => "Hard Mix",
            BlendMode::Difference => "Difference",
            BlendMode::Exclusion => "Exclusion",
            BlendMode::Subtract => "Subtract",
            BlendMode::Divide => "Divide",
            BlendMode::Hue => "Hue",
            BlendMode::Saturation => "Saturation",
            BlendMode::Color => "Color",
            BlendMode::Luminosity => "Luminosity",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuideAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Guide {
    #[serde(with = "upper_uuid")]
    pub id: Uuid,
    pub axis: GuideAxis,
    pub position: f64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// UUIDs as Foundation writes them: upper case.
pub mod upper_uuid {
    use serde::{Deserialize, Deserializer, Serializer};
    use uuid::Uuid;
    pub fn serialize<S: Serializer>(id: &Uuid, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::uuid_string(id))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Uuid, D::Error> {
        let s = String::deserialize(d)?;
        Uuid::parse_str(&s).map_err(serde::de::Error::custom)
    }
    pub mod option {
        use serde::{Deserialize, Deserializer, Serializer};
        use uuid::Uuid;
        pub fn serialize<S: Serializer>(id: &Option<Uuid>, s: S) -> Result<S::Ok, S::Error> {
            match id {
                Some(id) => s.serialize_str(&super::super::uuid_string(id)),
                None => s.serialize_none(),
            }
        }
        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Uuid>, D::Error> {
            let s = Option::<String>::deserialize(d)?;
            s.map(|s| Uuid::parse_str(&s).map_err(serde::de::Error::custom)).transpose()
        }
    }
}

pub fn uuid_string(id: &Uuid) -> String {
    id.hyphenated().to_string().to_uppercase()
}

/// A layer's raster mask. Its pixels stretch over the layer's own rectangle unless it has been
/// moved apart (`placement`).
#[derive(Clone, Debug)]
pub struct LayerMask {
    pub asset: Arc<MaskAsset>,
    pub enabled: bool,
    pub placement: Option<LayerTransform>,
    pub linked: bool,
}

impl LayerMask {
    pub fn solid(reveal: bool) -> Self {
        Self {
            asset: Arc::new(MaskAsset::new(Gray::new(1, 1, if reveal { 255 } else { 0 }))),
            enabled: true,
            placement: None,
            linked: true,
        }
    }
    /// Where the mask sits once its layer moves from `old` to `new`.
    pub fn placement_moving(&self, old: &LayerTransform, new: &LayerTransform) -> Option<LayerTransform> {
        if self.asset.image.w <= 1 && self.asset.image.h <= 1 {
            return None;
        }
        let moved = if self.linked { self.placement.map(|p| p.following(old, new)) } else { Some(self.placement.unwrap_or(*old)) };
        moved.filter(|p| !p.same_placement(new))
    }
}

#[derive(Clone, Debug)]
pub struct Layer {
    pub id: Uuid,
    pub name: String,
    pub visible: bool,
    pub transform: LayerTransform,
    pub parent: Option<Uuid>,
    pub is_group: bool,
    pub opacity: f64,
    pub blend: BlendMode,
    pub pixels: Option<Arc<PixelAsset>>,
    pub mask: Option<LayerMask>,
    /// The clipping base: this layer shows only where that one has alpha.
    pub mask_source: Option<Uuid>,
    pub adjustment: Option<Adjustment>,
    /// Record fields this build doesn't edit (text, shape, effects, …), written back unchanged.
    pub extra: Map<String, Value>,
}

impl Layer {
    pub fn blank(name: &str, w: usize, h: usize) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.to_string(),
            visible: true,
            transform: LayerTransform::new(Point::default(), w as f64, h as f64),
            parent: None,
            is_group: false,
            opacity: 1.0,
            blend: BlendMode::Normal,
            pixels: None,
            mask: None,
            mask_source: None,
            adjustment: None,
            extra: Map::new(),
        }
    }
    pub fn kind_label(&self) -> &'static str {
        if self.is_group {
            "Folder"
        } else if self.adjustment.is_some() {
            "Adjustment"
        } else if self.extra.contains_key("text") {
            "Text"
        } else if self.extra.contains_key("shape") {
            "Shape"
        } else {
            "Layer"
        }
    }
    /// Pixel edits make text and shape metadata stale, as on the Mac: the layer becomes plain pixels.
    pub fn rasterize_metadata(&mut self) {
        self.extra.remove("text");
        self.extra.remove("shape");
    }
    pub fn has_effects(&self) -> bool {
        self.extra.get("effects").is_some_and(|v| v.as_object().is_some_and(|o| !o.is_empty()))
    }
}

#[derive(Clone, Debug)]
pub struct Document {
    pub id: Uuid,
    pub width: usize,
    pub height: usize,
    pub resolution: f64,
    /// Bottom to top.
    pub layers: Vec<Layer>,
    pub guides: Vec<Guide>,
    pub active: Option<Uuid>,
    pub selection: Option<Selection>,
    /// Manifest keys this build doesn't know, kept for the next save.
    pub extra: Map<String, Value>,
}

pub struct HierarchyEntry {
    pub index: usize,
    pub depth: usize,
    pub visible: bool,
}

impl Document {
    pub fn new(width: usize, height: usize, background: Option<[u8; 4]>) -> Self {
        let mut doc = Self {
            id: Uuid::new_v4(),
            width,
            height,
            resolution: 72.0,
            layers: Vec::new(),
            guides: Vec::new(),
            active: None,
            selection: None,
            extra: Map::new(),
        };
        let mut layer = Layer::blank("Background", width, height);
        if let Some(color) = background {
            layer.pixels = Some(Arc::new(PixelAsset::new(crate::raster::Rgba::filled(width, height, color))));
        }
        doc.active = Some(layer.id);
        doc.layers.push(layer);
        doc
    }
    pub fn bounds(&self) -> Rect {
        Rect::new(0.0, 0.0, self.width as f64, self.height as f64)
    }
    pub fn index(&self, id: Uuid) -> Option<usize> {
        self.layers.iter().position(|l| l.id == id)
    }
    pub fn layer(&self, id: Uuid) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id == id)
    }
    pub fn layer_mut(&mut self, id: Uuid) -> Option<&mut Layer> {
        self.layers.iter_mut().find(|l| l.id == id)
    }
    pub fn active_layer(&self) -> Option<&Layer> {
        self.active.and_then(|id| self.layer(id))
    }
    /// Every layer and folder in drawing order, each folder before what's inside it.
    pub fn entries(&self, top_first: bool, collapsed: &HashSet<Uuid>) -> Vec<HierarchyEntry> {
        let mut children: HashMap<Option<Uuid>, Vec<usize>> = HashMap::new();
        for (i, l) in self.layers.iter().enumerate() {
            children.entry(l.parent).or_default().push(i);
        }
        let mut out = Vec::new();
        fn visit(
            doc: &Document,
            children: &HashMap<Option<Uuid>, Vec<usize>>,
            parent: Option<Uuid>,
            depth: usize,
            visible: bool,
            top_first: bool,
            collapsed: &HashSet<Uuid>,
            out: &mut Vec<HierarchyEntry>,
        ) {
            if depth > 64 {
                return;
            }
            let Some(siblings) = children.get(&parent) else { return };
            let order: Vec<usize> = if top_first { siblings.iter().rev().copied().collect() } else { siblings.clone() };
            for i in order {
                let layer = &doc.layers[i];
                let effective = visible && layer.visible;
                out.push(HierarchyEntry { index: i, depth, visible: effective });
                if layer.is_group && !collapsed.contains(&layer.id) {
                    visit(doc, children, Some(layer.id), depth + 1, effective, top_first, collapsed, out);
                }
            }
        }
        visit(self, &children, None, 0, true, top_first, collapsed, &mut out);
        out
    }
    /// The layers that draw, bottom to top (folders are pass-through and draw nothing themselves).
    pub fn visible_layers(&self) -> Vec<usize> {
        self.entries(false, &HashSet::new())
            .into_iter()
            .filter(|e| e.visible && !self.layers[e.index].is_group)
            .map(|e| e.index)
            .collect()
    }
    /// A folder's opacity multiplies into everything inside it.
    pub fn effective_opacity(&self, index: usize) -> f64 {
        let mut opacity = self.layers[index].opacity;
        let mut parent = self.layers[index].parent;
        let mut depth = 0;
        while let (Some(id), true) = (parent, depth < 64) {
            let Some(folder) = self.layer(id) else { break };
            opacity *= folder.opacity;
            parent = folder.parent;
            depth += 1;
        }
        opacity
    }
    pub fn ancestors(&self, id: Uuid) -> Vec<Uuid> {
        let mut out = Vec::new();
        let mut parent = self.layer(id).and_then(|l| l.parent);
        while let Some(p) = parent {
            if out.contains(&p) || out.len() > 64 {
                break;
            }
            out.push(p);
            parent = self.layer(p).and_then(|l| l.parent);
        }
        out
    }
    pub fn descendants(&self, id: Uuid) -> Vec<Uuid> {
        self.layers.iter().filter(|l| self.ancestors(l.id).contains(&id)).map(|l| l.id).collect()
    }
    /// Whether the active layer shows (it and every folder above it are visible).
    pub fn is_effectively_visible(&self, id: Uuid) -> bool {
        self.layer(id).is_some_and(|l| l.visible) && self.ancestors(id).iter().all(|a| self.layer(*a).is_some_and(|l| l.visible))
    }
    pub fn unique_name(&self, base: &str) -> String {
        if !self.layers.iter().any(|l| l.name == base) {
            return base.to_string();
        }
        (2..).map(|n| format!("{base} {n}")).find(|n| !self.layers.iter().any(|l| &l.name == n)).unwrap()
    }
}
