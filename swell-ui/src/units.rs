/// An abstract unit used for dialog dimensions, independent of HiDPI and stuff.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct DialogUnits(pub u32);

impl DialogUnits {
    pub fn get(self) -> u32 {
        self.0
    }

    pub fn as_raw(self) -> i32 {
        self.0 as _
    }
}

/// Pixels on a screen.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct Pixels(pub u32);

impl Pixels {
    pub fn get(self) -> u32 {
        self.0
    }

    pub fn as_raw(self) -> i32 {
        self.0 as _
    }
}

/// Point in a coordinate system.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct Point<T> {
    pub x: T,
    pub y: T,
}

impl<T> Point<T> {
    pub const fn new(x: T, y: T) -> Self {
        Self { x, y }
    }
}

fn effective_scale_factors() -> ScaleFactors {
    #[cfg(target_os = "linux")]
    {
        let scaling_256 = reaper_low::Swell::get().SWELL_GetScaling256();
        let hidpi_factor = scaling_256 as f64 / 256.0;
        ScaleFactors {
            main: 1.9 * hidpi_factor,
            y: 1.0,
        }
    }
    #[cfg(target_os = "macos")]
    {
        ScaleFactors { main: 1.6, y: 0.95 }
    }
    #[cfg(target_os = "windows")]
    {
        ScaleFactors { main: 1.7, y: 1.0 }
    }
}

struct ScaleFactors {
    /// The main scale factor which affects both x and y coordinates.
    ///
    /// Corresponds to `SWELL_DLG_SCALE_AUTOGEN` in `dialogs.cpp`.
    main: f64,
    /// An additional scale factor which is applied to y coordinates.
    ///
    /// Set to 1.0 if you want to use the main factor only.
    ///
    /// Corresponds to `SWELL_DLG_SCALE_AUTOGEN_YADJ` in `dialogs.cpp`.
    y: f64,
}

impl ScaleFactors {
    pub fn x_factor(&self) -> f64 {
        self.main
    }

    pub fn y_factor(&self) -> f64 {
        self.main * self.y
    }
}

impl Point<DialogUnits> {
    /// Converts this dialog unit point to pixels.
    ///
    /// The Window struct contains a method which can do this including Windows HiDPI information.
    pub fn in_pixels(&self) -> Point<Pixels> {
        // TODO-low On Windows this works differently. See original ReaLearn. But on the other hand
        //  ... this is only for the first short render before the optimal size is calculated.
        //  So as long as it works, this heuristic is okay.
        let scale_factors = effective_scale_factors();
        Point {
            x: Pixels((scale_factors.x_factor() * self.x.get() as f64) as _),
            y: Pixels((scale_factors.y_factor() * self.y.get() as f64) as _),
        }
    }
}

impl<T: Copy> Point<T> {
    pub fn to_dimensions(self) -> Dimensions<T> {
        Dimensions::new(self.x, self.y)
    }
}

impl<T: Copy> From<Dimensions<T>> for Point<T> {
    fn from(d: Dimensions<T>) -> Self {
        d.to_point()
    }
}

/// Dimensions of a rectangle.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct Dimensions<T> {
    pub width: T,
    pub height: T,
}

impl<T> Dimensions<T> {
    pub const fn new(width: T, height: T) -> Self {
        Self { width, height }
    }
}

impl<T: Copy> Dimensions<T> {
    pub fn to_point(self) -> Point<T> {
        Point::new(self.width, self.height)
    }
}

impl Dimensions<Pixels> {
    pub fn to_vst(self) -> (i32, i32) {
        (self.width.get() as _, self.height.get() as _)
    }
}

impl Dimensions<DialogUnits> {
    /// Converts the given dialog unit dimensions to pixels.
    pub fn in_pixels(&self) -> Dimensions<Pixels> {
        self.to_point().in_pixels().to_dimensions()
    }
}

impl<T: Copy> From<Point<T>> for Dimensions<T> {
    fn from(p: Point<T>) -> Self {
        p.to_dimensions()
    }
}
