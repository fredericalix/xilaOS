//! The character grid that the kernel copies to the screen.

/// Columns of the grid. Fixed, like V2_OS.
pub const COLS: usize = 80;
/// Most rows a grid can have. The actual count depends on the screen height.
pub const MAX_ROWS: usize = 50;

/// Most screen pixels per font pixel.
pub const MAX_SCALE: usize = 16;
/// Fewest rows the interface is made for.
const LEAST_ROWS: usize = 40;

/// How the grid fits a screen of that many pixels: screen pixels per pixel
/// of the font, and rows. The grid is always 80 columns wide: the largest
/// whole scale that leaves 40 rows is taken, then as many rows as fit.
pub fn fit(width: usize, height: usize) -> (usize, usize) {
    let mut scale = (width / (COLS * 8)).clamp(1, MAX_SCALE);
    while scale > 1 && height / (8 * scale) < LEAST_ROWS {
        scale -= 1;
    }
    (scale, (height / (8 * scale)).min(MAX_ROWS))
}

/// The screen the interface was designed on: 80 by 50 cells of 16 pixels.
pub const DESIGNED: (usize, usize) = (1280, 800);

/// Picks, among the sizes of screen a firmware offers, the one the grid
/// looks best on. `panel` is the size of the screen itself, when it tells:
/// a flat screen is only sharp at its own size.
///
/// 1. The size of the panel, if characters get at least two pixels per
///    pixel of the font there.
/// 2. The size the interface was designed on.
/// 3. The sharpest characters: the largest scale. Among equals, the size
///    the grid fills most, since the screen stretches the rest.
///
/// None if no size holds the grid: the screen is then left as it is.
pub fn choose(sizes: &[(usize, usize)], panel: Option<(usize, usize)>) -> Option<usize> {
    let holds = |size: &(usize, usize)| size.0 >= COLS * 8 && size.1 >= LEAST_ROWS * 8;
    if let Some(at) = sizes.iter().position(|size| Some(*size) == panel && holds(size) && fit(size.0, size.1).0 >= 2) {
        return Some(at);
    }
    if let Some(at) = sizes.iter().position(|size| *size == DESIGNED) {
        return Some(at);
    }
    // the part of the screen the grid covers: its pixels over those of the screen
    let covered = |size: &(usize, usize)| {
        let (scale, rows) = fit(size.0, size.1);
        ((COLS * 8 * scale * rows * 8 * scale) as u64, (size.0 * size.1) as u64)
    };
    let mut best: Option<usize> = None;
    for (at, size) in sizes.iter().enumerate().filter(|(_, size)| holds(size)) {
        let better = best.is_none_or(|best| {
            let (old, new) = (&sizes[best], size);
            let (old_scale, new_scale) = (fit(old.0, old.1).0, fit(new.0, new.1).0);
            let ((old_grid, old_all), (new_grid, new_all)) = (covered(old), covered(new));
            new_scale > old_scale || (new_scale == old_scale && new_grid * old_all > old_grid * new_all)
        });
        if better {
            best = Some(at);
        }
    }
    best
}

/// What changed in the picture of the screen since it was last shown: for
/// each row of the grid, the pixels across, from one to another.
///
/// A screen can be shown in two ways. Written directly, what changed is
/// copied to it and forgotten (`take`). Or the chip is given two pictures
/// and shows one while the other is prepared, then they change places
/// between two sweeps of the screen: the picture being prepared then
/// missed what was copied to the other one the time before, and needs
/// that too (`take_for_other`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Changes {
    now: Spans,
    before: Spans,
}

/// Pixels to copy: everything, the margins around the grid included, or
/// for each row of the grid those from `.0` up to `.1`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Spans {
    pub all: bool,
    pub rows: [(usize, usize); MAX_ROWS],
}

impl Spans {
    const NONE: Spans = Spans { all: false, rows: [(0, 0); MAX_ROWS] };

    pub fn any(&self) -> bool {
        self.all || self.rows.iter().any(|(from, to)| from < to)
    }

    fn add(&mut self, row: usize, from: usize, to: usize) {
        let span = &mut self.rows[row];
        if from < to {
            *span = if span.0 >= span.1 { (from, to) } else { (span.0.min(from), span.1.max(to)) };
        }
    }

    fn join(&mut self, other: &Spans) {
        self.all |= other.all;
        for (row, (from, to)) in other.rows.iter().enumerate() {
            self.add(row, *from, *to);
        }
    }
}

impl Changes {
    /// Nothing was ever shown: everything is to copy.
    pub const fn new() -> Changes {
        Changes { now: Spans { all: true, ..Spans::NONE }, before: Spans::NONE }
    }

    /// Pixels `from..to` of a row changed.
    pub fn touch(&mut self, row: usize, from: usize, to: usize) {
        self.now.add(row, from, to);
    }

    /// Everything changed.
    pub fn all(&mut self) {
        self.now.all = true;
    }

    pub fn any(&self) -> bool {
        self.now.any()
    }

    /// What to copy to a screen that is written directly.
    pub fn take(&mut self) -> Spans {
        self.before = Spans::NONE;
        core::mem::replace(&mut self.now, Spans::NONE)
    }

    /// What to copy to the one of two pictures that is not shown, before
    /// it takes the place of the other.
    pub fn take_for_other(&mut self) -> Spans {
        let now = core::mem::replace(&mut self.now, Spans::NONE);
        let mut both = now;
        both.join(&self.before);
        self.before = now;
        both
    }
}

/// Effects a cell can carry besides its character and colors.
pub mod fx {
    /// The cell shows one quarter of its character drawn four times larger.
    /// The low two bits say which quarter: bit 0 = right half, bit 1 = lower half.
    pub const BIG: u8 = 0x10;
    /// Round the left corners of the cell (start of the title bar).
    pub const ROUND_L: u8 = 0x20;
    /// Round the right corners of the cell (end of the title bar).
    pub const ROUND_R: u8 = 0x40;
    /// The background fades between `bg` and `fg` with the grid's pulse (cursor).
    pub const PULSE: u8 = 0x80;
}

/// One character cell. `fg` and `bg` are color roles of the theme.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub ch: u8,
    pub fg: u8,
    pub bg: u8,
    pub fx: u8,
}

impl Cell {
    pub const BLANK: Cell = Cell { ch: b' ', fg: 0, bg: 0, fx: 0 };

    pub const fn new(ch: u8, fg: u8, bg: u8) -> Cell {
        Cell { ch, fg, bg, fx: 0 }
    }
}

/// A grid of cells that remembers which cells changed since they were last drawn.
pub struct Grid {
    pub rows: usize,
    /// Phase of the pulsing cells, 0-255. Changing it makes them dirty.
    pub pulse: u8,
    cells: [[Cell; COLS]; MAX_ROWS],
    dirty: [[bool; COLS]; MAX_ROWS],
}

impl Grid {
    pub const fn new() -> Grid {
        Grid { rows: MAX_ROWS, pulse: 0, cells: [[Cell::BLANK; COLS]; MAX_ROWS], dirty: [[true; COLS]; MAX_ROWS] }
    }

    /// Sets a cell. Cells outside the grid are ignored.
    pub fn put(&mut self, col: usize, row: usize, cell: Cell) {
        if col < COLS && row < self.rows && self.cells[row][col] != cell {
            self.cells[row][col] = cell;
            self.dirty[row][col] = true;
        }
    }

    pub fn get(&self, col: usize, row: usize) -> Cell {
        self.cells[row][col]
    }

    /// Writes text from `col` on, cut at the right edge. Returns the next column.
    pub fn text(&mut self, col: usize, row: usize, text: &[u8], fg: u8, bg: u8) -> usize {
        let mut c = col;
        for &ch in text {
            if c >= COLS {
                break;
            }
            self.put(c, row, Cell::new(ch, fg, bg));
            c += 1;
        }
        c
    }

    /// Fills columns `from..to` of a row with one cell.
    pub fn fill(&mut self, row: usize, from: usize, to: usize, cell: Cell) {
        for col in from..to.min(COLS) {
            self.put(col, row, cell);
        }
    }

    /// Tells whether a cell must be redrawn, and forgets it.
    pub fn take_dirty(&mut self, col: usize, row: usize) -> bool {
        let d = self.dirty[row][col];
        self.dirty[row][col] = false;
        d
    }

    /// Marks every cell for redrawing (after a theme change, for instance).
    pub fn mark_all(&mut self) {
        self.dirty = [[true; COLS]; MAX_ROWS];
    }

    /// Advances the pulse and marks the pulsing cells.
    pub fn set_pulse(&mut self, pulse: u8) {
        if pulse == self.pulse {
            return;
        }
        self.pulse = pulse;
        for row in 0..self.rows {
            for col in 0..COLS {
                if self.cells[row][col].fx & fx::PULSE != 0 {
                    self.dirty[row][col] = true;
                }
            }
        }
    }

    /// The characters of a row, for tests and for the `peek` command.
    pub fn row_text(&self, row: usize) -> [u8; COLS] {
        let mut out = [b' '; COLS];
        for col in 0..COLS {
            let cell = self.cells[row][col];
            out[col] = if cell.fx & fx::BIG != 0 { b'#' } else { cell.ch };
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_pictures_shown_in_turn_both_get_every_change() {
        // Two pictures and what the user should see: one number per row
        // and per pixel across, here four pixels wide. Each step changes
        // some pixels, copies what `take_for_other` says to the hidden
        // picture, and shows it: it must then be what the user should see.
        const WIDE: usize = 4;
        let mut wanted = [[0u32; WIDE]; MAX_ROWS];
        let mut pictures = [[[9u32; WIDE]; MAX_ROWS]; 2];
        let mut changes = Changes::new();
        let mut shown = 0;
        let mut seed = 12345u32;
        let mut next = |most: u32| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (seed >> 16) % most
        };
        for step in 1..400u32 {
            // a few changes, or none, or everything
            match next(10) {
                0 => {}
                1 => {
                    wanted = [[step; WIDE]; MAX_ROWS];
                    changes.all();
                }
                _ => {
                    for _ in 0..next(4) + 1 {
                        let (row, from) = (next(MAX_ROWS as u32) as usize, next(WIDE as u32) as usize);
                        let to = from + 1 + next((WIDE - from) as u32) as usize;
                        wanted[row][from..to].fill(step);
                        changes.touch(row, from, to);
                    }
                }
            }
            // the chip is sometimes not ready: the changes wait and add up
            if next(4) == 0 || !changes.any() {
                continue;
            }
            let spans = changes.take_for_other();
            let hidden = 1 - shown;
            for row in 0..MAX_ROWS {
                let (from, to) = if spans.all { (0, WIDE) } else { spans.rows[row] };
                if from < to {
                    pictures[hidden][row][from..to].copy_from_slice(&wanted[row][from..to]);
                }
            }
            shown = hidden;
            assert!(pictures[shown] == wanted, "step {step}");
            assert!(!changes.any());
        }
    }

    #[test]
    fn a_screen_written_directly_gets_each_change_once() {
        let mut changes = Changes::new();
        assert!(changes.take().all && !changes.any());
        changes.touch(3, 16, 32);
        changes.touch(3, 64, 80);
        changes.touch(7, 0, 8);
        changes.touch(9, 5, 5);
        let spans = changes.take();
        assert_eq!((spans.all, spans.rows[3], spans.rows[7], spans.rows[9]), (false, (16, 80), (0, 8), (0, 0)));
        assert!(!changes.take().any());
        // going back from two pictures to one forgets what the other one missed
        changes.touch(1, 0, 8);
        changes.take_for_other();
        assert!(!changes.take().any());
        assert!(!changes.take_for_other().any());
    }

    #[test]
    fn the_grid_fits_the_screen() {
        assert_eq!(fit(1280, 800), (2, 50));
        assert_eq!(fit(1366, 768), (2, 48));
        assert_eq!(fit(1600, 900), (2, 50));
        assert_eq!(fit(1920, 1080), (3, 45));
        assert_eq!(fit(640, 480), (1, 50));
        assert_eq!(fit(1024, 768), (1, 50));
        // three pixels per pixel would leave 32 rows: two, then
        assert_eq!(fit(1920, 768), (2, 48));
        assert_eq!(fit(320, 200), (1, 25));
    }

    #[test]
    fn the_best_size_of_screen_is_chosen() {
        // a ThinkPad T430: the firmware starts in 640x480 and offers the panel's own size
        let t430 = [(640, 480), (800, 600), (1024, 768), (1366, 768)];
        assert_eq!(choose(&t430, Some((1366, 768))), Some(3));
        // the same without a word from the panel: the sharpest
        assert_eq!(choose(&t430, None), Some(3));
        // a firmware that offers nothing sharp: the size the grid fills most
        assert_eq!(choose(&t430[..3], Some((1366, 768))), Some(0));
        // QEMU: many sizes, and the one the interface was designed on
        let qemu = [(640, 480), (1024, 768), (1280, 800), (1920, 1080), (2560, 1600)];
        assert_eq!(choose(&qemu, None), Some(2));
        assert_eq!(choose(&qemu, Some((1280, 800))), Some(2));
        assert_eq!(choose(&qemu, Some((1024, 768))), Some(2));
        // a large panel is used at its own size
        assert_eq!(choose(&qemu, Some((1920, 1080))), Some(3));
        // among sizes as sharp, the one that is not stretched wins by the panel...
        let wide = [(1280, 1024), (1600, 900)];
        assert_eq!(choose(&wide, Some((1600, 900))), Some(1));
        // ...and without it, the one the grid fills most
        assert_eq!(choose(&wide, None), Some(0));
        // nothing that holds the grid
        assert_eq!(choose(&[(320, 200), (640, 200)], None), None);
        assert_eq!(choose(&[], Some((1366, 768))), None);
    }

    #[test]
    fn put_marks_only_changes() {
        let mut g = Grid::new();
        for r in 0..MAX_ROWS {
            for c in 0..COLS {
                g.take_dirty(c, r);
            }
        }
        g.put(3, 2, Cell::new(b'x', 1, 2));
        assert!(g.take_dirty(3, 2));
        g.put(3, 2, Cell::new(b'x', 1, 2));
        assert!(!g.take_dirty(3, 2));
        g.put(200, 2, Cell::BLANK); // outside: ignored
    }

    #[test]
    fn text_is_cut_at_the_edge() {
        let mut g = Grid::new();
        assert_eq!(g.text(78, 0, b"abcd", 0, 0), COLS);
        assert_eq!(&g.row_text(0)[76..], b"  ab");
    }

    #[test]
    fn pulse_marks_pulsing_cells() {
        let mut g = Grid::new();
        g.put(1, 1, Cell { ch: b' ', fg: 1, bg: 2, fx: fx::PULSE });
        g.take_dirty(1, 1);
        g.take_dirty(2, 1);
        g.set_pulse(9);
        assert!(g.take_dirty(1, 1));
        assert!(!g.take_dirty(2, 1));
    }
}
