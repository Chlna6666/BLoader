use core::ops::{Add, AddAssign, Sub, SubAssign};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn splat(value: f32) -> Self {
        Self { x: value, y: value }
    }
}

impl Add for Vec2 {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self::new(self.x + rhs.x, self.y + rhs.y)
    }
}

impl AddAssign for Vec2 {
    fn add_assign(&mut self, rhs: Self) {
        self.x += rhs.x;
        self.y += rhs.y;
    }
}

impl Sub for Vec2 {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self::new(self.x - rhs.x, self.y - rhs.y)
    }
}

impl SubAssign for Vec2 {
    fn sub_assign(&mut self, rhs: Self) {
        self.x -= rhs.x;
        self.y -= rhs.y;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub min: Vec2,
    pub max: Vec2,
}

impl Rect {
    pub const fn from_min_max(min: Vec2, max: Vec2) -> Self {
        Self { min, max }
    }

    pub fn from_min_size(min: Vec2, size: Vec2) -> Self {
        Self {
            min,
            max: min + size,
        }
    }

    pub fn width(self) -> f32 {
        self.max.x - self.min.x
    }

    pub fn height(self) -> f32 {
        self.max.y - self.min.y
    }

    pub fn size(self) -> Vec2 {
        Vec2::new(self.width(), self.height())
    }

    pub fn contains(self, point: Vec2) -> bool {
        point.x >= self.min.x
            && point.y >= self.min.y
            && point.x <= self.max.x
            && point.y <= self.max.y
    }

    pub fn intersects(self, other: Rect) -> bool {
        self.min.x < other.max.x
            && self.max.x > other.min.x
            && self.min.y < other.max.y
            && self.max.y > other.min.y
    }

    pub fn translate(self, delta: Vec2) -> Self {
        Self::from_min_max(self.min + delta, self.max + delta)
    }

    pub fn shrink(self, value: f32) -> Self {
        Self::from_min_max(
            Vec2::new(self.min.x + value, self.min.y + value),
            Vec2::new(self.max.x - value, self.max.y - value),
        )
    }
}
