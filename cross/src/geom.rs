//! Points, rectangles and affine maps in document space (y down, as on the Mac).

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
    pub fn distance(self, other: Point) -> f64 {
        (self.x - other.x).hypot(self.y - other.y)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }
    pub fn max_x(&self) -> f64 {
        self.x + self.w
    }
    pub fn max_y(&self) -> f64 {
        self.y + self.h
    }
    pub fn is_empty(&self) -> bool {
        !(self.w > 0.0 && self.h > 0.0)
    }
    pub fn from_points(a: Point, b: Point) -> Self {
        Self::new(a.x.min(b.x), a.y.min(b.y), (a.x - b.x).abs(), (a.y - b.y).abs())
    }
    pub fn bounding(points: &[Point]) -> Self {
        let mut min = Point::new(f64::INFINITY, f64::INFINITY);
        let mut max = Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
        for p in points {
            min.x = min.x.min(p.x);
            min.y = min.y.min(p.y);
            max.x = max.x.max(p.x);
            max.y = max.y.max(p.y);
        }
        Self::from_points(min, max)
    }
    pub fn intersection(&self, other: &Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let w = self.max_x().min(other.max_x()) - x;
        let h = self.max_y().min(other.max_y()) - y;
        if w <= 0.0 || h <= 0.0 {
            Rect::new(x, y, 0.0, 0.0)
        } else {
            Rect::new(x, y, w, h)
        }
    }
    pub fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Rect::new(x, y, self.max_x().max(other.max_x()) - x, self.max_y().max(other.max_y()) - y)
    }
    pub fn inset(&self, by: f64) -> Rect {
        Rect::new(self.x + by, self.y + by, self.w - 2.0 * by, self.h - 2.0 * by)
    }
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.y >= self.y && p.x <= self.max_x() && p.y <= self.max_y()
    }
    pub fn center(&self) -> Point {
        Point::new(self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// An integer pixel rectangle, half open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IRect {
    pub x0: i64,
    pub y0: i64,
    pub x1: i64,
    pub y1: i64,
}

impl IRect {
    pub fn new(x0: i64, y0: i64, x1: i64, y1: i64) -> Self {
        Self { x0, y0, x1, y1 }
    }
    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }
    pub fn width(&self) -> i64 {
        (self.x1 - self.x0).max(0)
    }
    pub fn height(&self) -> i64 {
        (self.y1 - self.y0).max(0)
    }
    /// The pixels a rectangle touches.
    pub fn covering(r: &Rect) -> Self {
        Self::new(r.x.floor() as i64, r.y.floor() as i64, r.max_x().ceil() as i64, r.max_y().ceil() as i64)
    }
    pub fn clamp(&self, w: i64, h: i64) -> Self {
        Self::new(self.x0.clamp(0, w), self.y0.clamp(0, h), self.x1.clamp(0, w), self.y1.clamp(0, h))
    }
    pub fn union(&self, other: &IRect) -> IRect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        IRect::new(self.x0.min(other.x0), self.y0.min(other.y0), self.x1.max(other.x1), self.y1.max(other.y1))
    }
    pub fn outset(&self, by: i64) -> IRect {
        IRect::new(self.x0 - by, self.y0 - by, self.x1 + by, self.y1 + by)
    }
}

/// `x' = a·x + c·y + tx`, `y' = b·x + d·y + ty`, as Core Graphics lays it out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub tx: f64,
    pub ty: f64,
}

impl Default for Affine {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Affine {
    pub const IDENTITY: Affine = Affine { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 0.0, ty: 0.0 };

    pub fn translation(x: f64, y: f64) -> Self {
        Affine { tx: x, ty: y, ..Self::IDENTITY }
    }
    pub fn scale(x: f64, y: f64) -> Self {
        Affine { a: x, d: y, ..Self::IDENTITY }
    }
    pub fn rotation(radians: f64) -> Self {
        let (s, c) = radians.sin_cos();
        Affine { a: c, b: s, c: -s, d: c, tx: 0.0, ty: 0.0 }
    }
    /// `self` after `first`: points go through `first`, then through `self`.
    pub fn after(&self, first: &Affine) -> Affine {
        Affine {
            a: self.a * first.a + self.c * first.b,
            b: self.b * first.a + self.d * first.b,
            c: self.a * first.c + self.c * first.d,
            d: self.b * first.c + self.d * first.d,
            tx: self.a * first.tx + self.c * first.ty + self.tx,
            ty: self.b * first.tx + self.d * first.ty + self.ty,
        }
    }
    /// Core Graphics' `concatenating`: `self` first, then `then`.
    pub fn then(&self, then: &Affine) -> Affine {
        then.after(self)
    }
    pub fn apply(&self, p: Point) -> Point {
        Point::new(self.a * p.x + self.c * p.y + self.tx, self.b * p.x + self.d * p.y + self.ty)
    }
    pub fn determinant(&self) -> f64 {
        self.a * self.d - self.b * self.c
    }
    pub fn inverted(&self) -> Option<Affine> {
        let det = self.determinant();
        if det.abs() < 1e-12 || !det.is_finite() {
            return None;
        }
        let a = self.d / det;
        let b = -self.b / det;
        let c = -self.c / det;
        let d = self.a / det;
        Some(Affine { a, b, c, d, tx: -(a * self.tx + c * self.ty), ty: -(b * self.tx + d * self.ty) })
    }
    /// Average linear scale (for brush radii and mip levels).
    pub fn mean_scale(&self) -> f64 {
        self.determinant().abs().sqrt()
    }
    pub fn map_rect(&self, r: &Rect) -> Rect {
        Rect::bounding(&[
            self.apply(Point::new(r.x, r.y)),
            self.apply(Point::new(r.max_x(), r.y)),
            self.apply(Point::new(r.max_x(), r.max_y())),
            self.apply(Point::new(r.x, r.max_y())),
        ])
    }
}
