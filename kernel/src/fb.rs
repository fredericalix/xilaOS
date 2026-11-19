//! Draws the character grid on the screen.
//!
//! Nothing is drawn in the memory of the screen itself. The picture is
//! prepared in ordinary memory, in the screen's own colors, and `show`
//! copies what changed, whole runs of pixels at a time and in order. That is
//! what the memory of a screen is good at: it is slow to write one pixel at
//! a time, and what is being drawn never shows half done.
//!
//! With a graphics chip that can show either of two pictures
//! (`chip::Flipper`), what changed is copied to the one that is not shown,
//! and the two change places between two sweeps of the screen: then not
//! even the copy is seen under way.

use alloc::vec::Vec;

use xila_core::font::FONT;
use xila_core::grid::{self, fx, Cell, Changes, Grid, COLS, MAX_ROWS};
use xila_core::theme::{mix, role, Theme};

use crate::arch;
use crate::chip::Flipper;
use crate::uefi::Framebuffer;

pub struct Screen {
    fb: Framebuffer,
    /// Screen pixels per font pixel.
    pub scale: usize,
    /// Rows of the grid that fit.
    pub rows: usize,
    /// Top left corner of the grid, which is centered.
    left: usize,
    top: usize,
    /// The picture being prepared: `width` colors per line, as the screen
    /// stores them.
    back: Vec<u32>,
    /// What changed in it since the last `show`.
    changes: Changes,
    /// Two pictures shown in turn, when the screen is not written directly;
    /// which of them is on the screen, or soon will be; and how many times
    /// in a row `show` found the chip not done with the last change.
    flip: Option<Flipper>,
    front: usize,
    waited: u32,
}

/// How long the chip is given to show a picture: six sweeps of a slow screen.
const PATIENCE_US: u64 = 200_000;
/// How many times `show` may find the chip late before it counts as stuck.
const STUCK: u32 = 50;

// The memory of the screen belongs to whoever holds the `Screen`.
unsafe impl Send for Screen {}

impl Screen {
    pub fn new(fb: Framebuffer) -> Screen {
        let (scale, rows) = grid::fit(fb.width, fb.height);
        let left = (fb.width - (COLS * 8 * scale).min(fb.width)) / 2;
        let top = (fb.height - rows * 8 * scale) / 2;
        let back = alloc::vec![0; fb.width * fb.height];
        Screen { fb, scale, rows, left, top, back, changes: Changes::new(), flip: None, front: 0, waited: 0 }
    }

    pub fn width(&self) -> usize {
        self.fb.width
    }

    pub fn height(&self) -> usize {
        self.fb.height
    }

    /// The memory of the screen: where it is, and how many pixels it
    /// holds, those between the lines included.
    pub fn memory(&self) -> (*mut u32, usize) {
        (self.fb.base, self.fb.stride * self.fb.height)
    }

    fn pixel(&self, rgb: u32) -> u32 {
        if self.fb.blue_first {
            rgb
        } else {
            (rgb & 0x00ff00) | ((rgb & 0xff) << 16) | ((rgb >> 16) & 0xff)
        }
    }

    /// Fills the whole picture, margins included.
    pub fn clear(&mut self, rgb: u32) {
        let value = self.pixel(rgb);
        self.back.fill(value);
        self.changes.all();
    }

    /// Asks `show` for the whole picture, whatever changed.
    pub fn show_all(&mut self) {
        self.changes.all();
    }

    fn touch(&mut self, row: usize, from: usize, to: usize) {
        self.changes.touch(row, from, to);
    }

    /// From now on pictures are shown through the two of a `Flipper`, of
    /// which the first must be the one on the screen now.
    pub fn flip(&mut self, flipper: Flipper) {
        self.direct();
        (self.flip, self.front, self.waited) = (Some(flipper), 0, 0);
        // both pictures are to be filled: one now, the other the next time
        self.changes.all();
    }

    /// From now on the screen is written directly again.
    pub fn direct(&mut self) {
        self.settle();
        if let Some(mut flipper) = self.flip.take() {
            if flipper.of_chip() && self.front != 0 {
                // back to the picture that is the screen for everyone else
                flipper.show(0);
                self.front = 0;
                self.flip = Some(flipper);
                self.settle();
                self.flip = None;
            }
        }
        self.changes.take();
        self.changes.all();
    }

    /// Waits until the picture asked for is on the screen, for a while.
    /// Says whether it is.
    pub fn settle(&mut self) -> bool {
        let Some(flipper) = &mut self.flip else {
            return true;
        };
        let start = arch::micros();
        while flipper.shown() != Some(self.front) {
            if arch::micros() - start > PATIENCE_US {
                return false;
            }
            core::hint::spin_loop();
        }
        true
    }

    /// How many times the screen was swept, when a chip that counts shows
    /// the pictures.
    pub fn sweeps(&mut self) -> Option<u32> {
        self.flip.as_mut()?.sweeps()
    }

    /// Whether the chip has stopped changing pictures.
    pub fn stuck(&self) -> bool {
        self.waited > STUCK
    }

    /// Draws the cells that changed into the picture. Rows `skip.0..skip.1`
    /// are left alone: a picture is shown there.
    pub fn compose(&mut self, grid: &mut Grid, theme: &Theme, skip: (usize, usize)) {
        for row in 0..grid.rows.min(self.rows) {
            for col in 0..COLS {
                if grid.take_dirty(col, row) && !(skip.0..skip.1).contains(&row) {
                    self.draw(col, row, grid.get(col, row), grid.pulse, theme);
                }
            }
        }
    }

    /// Puts a picture of `width` colors per row into the prepared one,
    /// starting at a row of the grid. One color fills as many screen pixels
    /// as a dot of a character.
    pub fn blit(&mut self, pixels: &[u32], width: usize, top_row: usize) {
        let height = pixels.len() / width;
        let scale = self.scale;
        let y0 = self.top + top_row * 8 * scale;
        let across = width * scale;
        if self.left + across > self.fb.width || y0 + height * scale > self.fb.height {
            return;
        }
        let blue_first = self.fb.blue_first;
        for y in 0..height {
            let source = &pixels[y * width..(y + 1) * width];
            // one line enlarged across, then repeated below itself
            let first = (y0 + y * scale) * self.fb.width + self.left;
            let line = &mut self.back[first..first + across];
            match scale {
                1 => enlarge(line, source, 1, blue_first),
                2 => enlarge(line, source, 2, blue_first),
                3 => enlarge(line, source, 3, blue_first),
                _ => enlarge(line, source, scale, blue_first),
            }
            for repeat in 1..scale {
                self.back.copy_within(first..first + across, first + repeat * self.fb.width);
            }
        }
        for row in top_row..(top_row + height.div_ceil(8)).min(MAX_ROWS) {
            self.touch(row, self.left, self.left + across);
        }
    }

    /// Copies to the screen what changed in the picture since the last
    /// time. Returns how many pixels that was.
    ///
    /// With two pictures it returns 0, and keeps what changed for the next
    /// time, while the chip is not done with the last change: the picture
    /// to prepare would be the one still on the screen.
    pub fn show(&mut self) -> usize {
        if !self.changes.any() {
            return 0;
        }
        let (spans, target) = match &mut self.flip {
            None => (self.changes.take(), self.fb.base),
            Some(flipper) => {
                if flipper.shown() != Some(self.front) {
                    self.waited += 1;
                    return 0;
                }
                self.waited = 0;
                (self.changes.take_for_other(), flipper.picture(1 - self.front))
            }
        };
        let (width, size) = (self.fb.width, 8 * self.scale);
        let mut copied = 0;
        if spans.all {
            for y in 0..self.fb.height {
                self.copy(target, y, 0, width);
            }
            copied = width * self.fb.height;
        } else {
            for row in 0..self.rows {
                let (from, to) = spans.rows[row];
                if from < to {
                    for y in self.top + row * size..self.top + (row + 1) * size {
                        self.copy(target, y, from, to);
                    }
                    copied += (to - from) * size;
                }
            }
        }
        // what the processor still holds back goes to the screen now
        unsafe { core::arch::asm!("sfence", options(nostack, preserves_flags)) };
        if let Some(flipper) = &mut self.flip {
            self.front = 1 - self.front;
            flipper.show(self.front);
        }
        copied
    }

    /// Copies pixels `from..to` of a line of the picture to the screen, or
    /// to one of two pictures, at `base`: with the processor's own
    /// instruction for copying a block, in order and without a gap, which
    /// is how the memory of a screen takes them best.
    fn copy(&self, base: *mut u32, y: usize, from: usize, to: usize) {
        let source = self.back[y * self.fb.width + from..y * self.fb.width + to].as_ptr();
        unsafe {
            let target = base.add(y * self.fb.stride + from);
            core::arch::asm!(
                // two pixels at a time, then the odd one
                "rep movsq",
                "mov ecx, {odd:e}",
                "rep movsd",
                odd = in(reg) (to - from) & 1,
                inout("rcx") (to - from) / 2 => _,
                inout("rsi") source => _,
                inout("rdi") target => _,
                options(nostack, preserves_flags)
            );
        }
    }

    fn draw(&mut self, col: usize, row: usize, cell: Cell, pulse: u8, theme: &Theme) {
        let mut ink = theme.color(cell.fg, col);
        let mut paper = theme.color(cell.bg, col);
        if cell.fx & fx::PULSE != 0 {
            let (i, p) = (ink, paper);
            paper = mix(p, i, pulse as u32);
            ink = mix(i, p, pulse as u32);
        }
        let outside = self.pixel(theme.color(role::PAGE, col));
        let (ink, paper) = (self.pixel(ink), self.pixel(paper));
        let glyph = &FONT[cell.ch as usize];
        let (scale, size) = (self.scale, 8 * self.scale);
        let (x0, y0) = (self.left + col * size, self.top + row * size);
        if x0 + size > self.fb.width || y0 + size > self.fb.height {
            return;
        }
        // For big characters: which quarter of the enlarged glyph this cell shows.
        let big = cell.fx & fx::BIG != 0;
        let (qx, qy) = ((cell.fx & 1) as usize * 8, ((cell.fx >> 1) & 1) as usize * 8);
        let round = cell.fx & (fx::ROUND_L | fx::ROUND_R) != 0;
        let radius = (size / 2) as i32;
        for fy in 0..8 {
            // one row of dots of the character...
            let bits = if big {
                let wide = glyph[(fy + qy) / 2];
                (0..8).fold(0, |bits, dot| if wide & (0x80 >> ((dot + qx) / 2)) != 0 { bits | 0x80 >> dot } else { bits })
            } else {
                glyph[fy]
            };
            // ...written as many times as it is tall
            for py in fy * scale..(fy + 1) * scale {
                let first = (y0 + py) * self.fb.width + x0;
                let target = &mut self.back[first..first + size];
                match scale {
                    1 => dots(target, 1, bits, ink, paper),
                    2 => dots(target, 2, bits, ink, paper),
                    3 => dots(target, 3, bits, ink, paper),
                    _ => dots(target, scale, bits, ink, paper),
                }
                if round {
                    for (px, value) in target.iter_mut().enumerate() {
                        // Distance, in half pixels, from the center of the half
                        // circle that ends the bar.
                        let x = if cell.fx & fx::ROUND_L != 0 { px } else { size - 1 - px } as i32;
                        let dx = 2 * radius - 2 * x - 1;
                        let dy = 2 * py as i32 - 2 * radius + 1;
                        if dx > 0 && dx * dx + dy * dy > 4 * radius * radius {
                            *value = outside;
                        }
                    }
                }
            }
        }
        self.touch(row, x0, x0 + size);
    }
}

/// Writes a line of a cell: each of the eight dots of `bits` as `scale`
/// pixels. Called with the usual scales as constants, so that the compiler
/// writes one version for each, without a loop inside a dot.
#[inline(always)]
fn dots(target: &mut [u32], scale: usize, bits: u8, ink: u32, paper: u32) {
    for (dot, run) in target.chunks_exact_mut(scale).enumerate() {
        let value = if bits & (0x80 >> dot) != 0 { ink } else { paper };
        for pixel in run {
            *pixel = value;
        }
    }
}

/// Writes a line of a picture, each color as `scale` pixels. As for `dots`.
#[inline(always)]
fn enlarge(line: &mut [u32], source: &[u32], scale: usize, blue_first: bool) {
    for (run, &color) in line.chunks_exact_mut(scale).zip(source) {
        let value = if blue_first { color } else { (color & 0x00ff00) | ((color & 0xff) << 16) | ((color >> 16) & 0xff) };
        for pixel in run {
            *pixel = value;
        }
    }
}
