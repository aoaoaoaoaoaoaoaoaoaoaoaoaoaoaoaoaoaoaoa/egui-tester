use std::time::Duration;

/// X11 mouse button.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Button {
    Primary = 1,
    Middle = 2,
    Secondary = 3,
}

bitflags::bitflags! {
    /// Keyboard modifiers held around one atomic input gesture.
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct Modifiers: u8 {
        const SHIFT = 1 << 0;
        const CTRL = 1 << 1;
        const ALT = 1 << 2;
        const SUPER = 1 << 3;
    }
}

/// Portable key subset plus Latin-1 characters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Key {
    Character(char),
    Return,
    Escape,
    Tab,
    Backspace,
    Delete,
    Home,
    End,
    Left,
    Right,
    Up,
    Down,
    Function(u8),
    Shift,
    Control,
    Alt,
    Super,
}

/// Human-like pointer drag policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Drag {
    pub button: Button,
    /// Time allowed for the application to acquire the pressed target before motion.
    pub press_duration: Duration,
    pub steps: u16,
    /// Total time spent transporting the pointer after acquisition.
    pub duration: Duration,
}

/// Timed polyline traversed while one pointer button remains held.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Stroke {
    pub button: Button,
    /// Time allowed for the application to acquire the pressed target.
    pub press_duration: Duration,
    pub steps_per_leg: u16,
    pub leg_duration: Duration,
    /// Dwell at each knot before beginning the next leg.
    ///
    /// Use this when the product must observe polyline corners despite native
    /// motion coalescing. The final dwell precedes button release.
    pub knot_dwell: Duration,
}

/// Timed wheel gesture; each tick reaches the product as a distinct input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Wheel {
    pub tick_duration: Duration,
}

/// Timed pointer transport without a held button.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Motion {
    pub steps: u16,
    pub duration: Duration,
}

impl Default for Drag {
    fn default() -> Self {
        Self {
            button: Button::Primary,
            press_duration: Duration::from_millis(32),
            steps: 8,
            duration: Duration::from_millis(120),
        }
    }
}

impl Default for Motion {
    fn default() -> Self {
        Self {
            steps: 8,
            duration: Duration::from_millis(120),
        }
    }
}

impl Default for Stroke {
    fn default() -> Self {
        Self {
            button: Button::Primary,
            press_duration: Duration::from_millis(32),
            steps_per_leg: 8,
            leg_duration: Duration::from_millis(120),
            knot_dwell: Duration::ZERO,
        }
    }
}

impl Default for Wheel {
    fn default() -> Self {
        Self {
            tick_duration: Duration::from_millis(24),
        }
    }
}
