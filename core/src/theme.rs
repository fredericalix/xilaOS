//! Color themes. The values come from the Trame 0.3 mockup.

/// What a color is used for. Cells store roles, not colors, so changing the
/// theme only needs a redraw.
pub mod role {
    pub const PAGE: u8 = 0;
    /// Header frame, text and version.
    pub const MAST_INK: u8 = 1;
    pub const MAST_LOGO: u8 = 2;
    /// Title bar.
    pub const BAR_BG: u8 = 3;
    pub const BAR_INK: u8 = 4;
    pub const BAR_ICON: u8 = 5;
    /// Terminal: background, output, prompt, typed command.
    pub const TERM_BASE: u8 = 6;
    pub const TERM_OUT: u8 = 7;
    pub const TERM_PROMPT: u8 = 8;
    pub const TERM_CMD: u8 = 9;
    /// Boot log screen.
    pub const BOOT_BASE: u8 = 10;
    pub const BOOT_OUT: u8 = 11;
    /// The six hues of the libraries; HUE + 3 doubles as the error color.
    pub const HUE: u8 = 12;
    pub const ERROR: u8 = HUE + 3;
    pub const COUNT: usize = 18;
    /// Background only: the horizontal gradient of the header.
    pub const MAST_GRADIENT: u8 = 0xF0;
}

/// Colors of the city view: the sky from its top to the horizon, which
/// warms up with the mood, and the ground with its grid.
pub struct Sky {
    pub top: u32,
    pub horizon_calm: u32,
    pub horizon_hot: u32,
    pub ground: u32,
    pub grid: u32,
    /// At night the blocks glow instead of being shaded by daylight.
    pub night: bool,
}

pub struct Theme {
    pub name: &'static [u8],
    /// 0xRRGGBB for every role.
    pub colors: [u32; role::COUNT],
    /// Left and right ends of the header gradient.
    pub mast: (u32, u32),
    pub sky: Sky,
}

pub const DAY: Theme = Theme {
    name: b"day",
    colors: [
        0xffe0a3, 0x4a2a0c, 0xe8702a, 0x4a2a0c, 0xfff3d1, 0xffcf8a, 0xfff6dc, 0x6b4416, 0x2f1a06, 0xc2410c,
        0xfdeeb8, 0x8a5a00, 0x128f84, 0x6a4fd0, 0xc77700, 0xd1366f, 0x5a8f14, 0x2a73c9,
    ],
    mast: (0xffeaa6, 0xffcf99),
    sky: Sky { top: 0xfff3c4, horizon_calm: 0xfff0b8, horizon_hot: 0xffa56b, ground: 0xfbefd2, grid: 0xe3b877, night: false },
};

pub const NIGHT: Theme = Theme {
    name: b"night",
    colors: [
        0xffd392, 0x3a2008, 0xe0661f, 0xfff3d1, 0x2a1706, 0xe0661f, 0x241505, 0xffd9a0, 0xfff6dc, 0xffef7a,
        0x2b2204, 0xffd24d, 0x5fe3c9, 0xb9a2ff, 0xffc24d, 0xff7fa8, 0xc9f06a, 0x7cc0ff,
    ],
    mast: (0xffe19a, 0xffbf85),
    sky: Sky { top: 0x120a04, horizon_calm: 0xf0b24a, horizon_hot: 0xff7436, ground: 0x1a1007, grid: 0x8a5a22, night: true },
};

pub const THEMES: [&Theme; 2] = [&DAY, &NIGHT];

/// Mixes two colors: `t` = 0 gives `a`, `t` = 255 gives `b`.
pub fn mix(a: u32, b: u32, t: u32) -> u32 {
    let ch = |shift: u32| {
        let x = (a >> shift) & 0xff;
        let y = (b >> shift) & 0xff;
        ((x * (255 - t) + y * t) / 255) << shift
    };
    ch(16) | ch(8) | ch(0)
}

impl Theme {
    /// The color of a role in a given column (the column matters for the gradient).
    pub fn color(&self, role: u8, col: usize) -> u32 {
        if role == role::MAST_GRADIENT {
            mix(self.mast.0, self.mast.1, (col as u32 * 255) / 79)
        } else {
            self.colors[(role as usize).min(role::COUNT - 1)]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_reaches_both_ends() {
        assert_eq!(mix(0x102030, 0xfffefd, 0), 0x102030);
        assert_eq!(mix(0x102030, 0xfffefd, 255), 0xfffefd);
    }

    #[test]
    fn gradient_runs_across_the_header() {
        assert_eq!(DAY.color(role::MAST_GRADIENT, 0), 0xffeaa6);
        assert_eq!(DAY.color(role::MAST_GRADIENT, 79), 0xffcf99);
        assert_eq!(DAY.color(role::TERM_CMD, 5), 0xc2410c);
    }
}
