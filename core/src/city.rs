//! The city: the state of the system drawn as blocks on six plots.
//!
//! Each plot is a library of the system, each block one thing in it, and the
//! height of a block follows what that thing does: a task grows with the
//! share of the processor it gets, the serial port with its traffic, the mood
//! with the typing. `scene` works the heights out from the system state;
//! `City` animates them and draws them, in pixels, into a buffer that the
//! kernel copies to the screen.

use crate::font::FONT;
use crate::shell::System;
use crate::text::FixedStr;
use crate::theme::{mix, role, Theme};

/// Size of the picture, in its own pixels. The kernel shows each as a square
/// of screen pixels, the same size as the dots of the characters.
pub const WIDTH: usize = 640;
pub const HEIGHT: usize = 176;

pub const PLOTS: usize = 6;
/// Most blocks on a plot: three by three.
pub const SLOTS: usize = 9;

#[derive(Clone, Copy)]
pub struct Block {
    pub name: FixedStr<16>,
    /// Height in pixels, 4 to 40.
    pub height: u16,
    /// Lit blocks are the ones doing something right now.
    pub lit: bool,
    pub used: bool,
}

impl Block {
    const NONE: Block = Block { name: FixedStr::new(), height: 0, lit: false, used: false };
}

pub struct Plot {
    pub name: &'static [u8],
    pub blocks: [Block; SLOTS],
}

impl Plot {
    fn new(name: &'static [u8]) -> Plot {
        Plot { name, blocks: [Block::NONE; SLOTS] }
    }

    fn add(&mut self, name: &[u8], height: u64, lit: bool) {
        if let Some(block) = self.blocks.iter_mut().find(|block| !block.used) {
            *block = Block { name: FixedStr::from_bytes(name), height: height.clamp(4, 40) as u16, lit, used: true };
        }
    }
}

// The plots, in the order of their hues.
const HOME: usize = 0;
const SETTINGS: usize = 1;
const LOGS: usize = 2;
const DATA: usize = 3;
const RUNNING: usize = 4;
const ELSEWHERE: usize = 5;

/// What the city shows for a state of the system.
pub fn scene(sys: &System) -> [Plot; PLOTS] {
    let mut plots = [
        Plot::new(b"home"),
        Plot::new(b"settings"),
        Plot::new(b"logs"),
        Plot::new(b"data"),
        Plot::new(b"running"),
        Plot::new(b"elsewhere"),
    ];
    // Home: the tables the user made. A block grows with the rows of its
    // table and is lit for a few seconds when the table is read or changed.
    for table in sys.db.tables().iter().filter(|table| !crate::catalog::kept_apart(&table.name)) {
        let touched = table.read_at.max(table.written_at);
        plots[HOME].add(&table.name, 5 + (table.rows() as u64 * 2).min(35), touched != 0 && sys.ticks.saturating_sub(touched) < 300);
    }

    // The data: one block per table, as tall as it has columns, lit for a
    // few seconds when a query reads it.
    for (index, table) in crate::catalog::TABLES.iter().enumerate().filter(|(_, table)| table.name != crate::catalog::JOBS) {
        let read = sys.queried[index];
        plots[DATA].add(table.name, 4 + 4 * table.columns.len() as u64, read != 0 && sys.ticks.saturating_sub(read) < 300);
    }

    let settings = &mut plots[SETTINGS];
    settings.add(b"mood", 6 + sys.energy as u64 * 30 / 1000, sys.energy > 0);
    settings.add(b"theme", 6 + 10 * sys.theme as u64, false);
    settings.add(b"keymap", if sys.layout == crate::keys::Layout::Us { 6 } else { 16 }, false);
    settings.add(b"screen", 6 + 10 * sys.screen as u64, false);

    let logs = &mut plots[LOGS];
    logs.add(b"boot log", 6 + (sys.log_rows as u64 * 2).min(34), false);
    logs.add(b"terminal", 6 + (sys.term_rows as u64 / 3).min(34), false);
    // events: as tall as there are listeners, lit when one was just sent
    if sys.events.listeners() > 0 {
        let sent = sys.events.sent_at;
        logs.add(b"events", 6 + 4 * sys.events.listeners() as u64, sent != 0 && sys.ticks.saturating_sub(sent) < 300);
    }

    for (id, task) in sys.tasks.iter().enumerate().filter(|(_, task)| task.used) {
        let share = sys.cpu_share[id] as u64;
        plots[RUNNING].add(task.name.bytes(), 5 + share * 35 / 1000, share >= 20);
    }

    // a job that has not ended is among what runs: as tall as what it has
    // run, lit while it runs
    for job in sys.jobs.list().iter().filter(|job| job.number != 0 && !job.ended()) {
        plots[RUNNING].add(job.program.bytes(), 5 + (job.statements / 500).min(35), job.state == crate::jobs::State::Ready);
    }

    if sys.machine.serial {
        plots[ELSEWHERE].add(b"serial port", 6 + (sys.serial_rate as u64 / 8).min(34), sys.serial_rate > 0);
    }
    // the disk the tables are kept on: as tall as it is full, lit for a
    // few seconds when a change is written to it
    let storage = sys.db.storage();
    if storage.keeps {
        let wrote = sys.stored_at;
        plots[ELSEWHERE].add(b"disk", 6 + storage.used * 34 / storage.pages.max(1), wrote != 0 && sys.ticks.saturating_sub(wrote) < 300);
    }
    plots
}

// Where things are. The ground is a grid of tiles seen from above at an
// angle: a tile is 16 pixels wide and 8 high, and going one tile along x
// moves right and down, one tile along y moves left and down.
const HORIZON: i32 = 40;
const ORIGIN_X: i32 = 292;
const ORIGIN_Y: i32 = 46;
/// Tile of the far corner of each plot. A plot is 6 tiles wide and deep.
const PLOT_AT: [(i32, i32); PLOTS] = [(0, 0), (0, 7), (14, 0), (7, 0), (7, 7), (14, 7)];
const PLOT_TILES: i32 = 6;
/// Where the name of each plot goes, so that no name covers a block: a
/// corner of the plot (in tiles from its far corner), which side of that
/// point the text is on (-1 ends there, 0 is centered, 1 starts there), and
/// how far below it.
const NAME_AT: [((i32, i32), i32, i32); PLOTS] =
    [((0, 0), 0, -16), ((0, 5), -1, -4), ((5, 0), 1, -4), ((0, 0), 0, -16), ((5, 5), 0, 10), ((5, 0), 1, -4)];

fn project(x: i32, y: i32) -> (i32, i32) {
    (ORIGIN_X + (x - y) * 8, ORIGIN_Y + (x + y) * 4)
}

/// Tile of a slot of a plot: three rows of three, one tile apart.
fn slot_tile(plot: usize, slot: usize) -> (i32, i32) {
    let (px, py) = PLOT_AT[plot];
    (px + 1 + 2 * (slot as i32 % 3), py + 1 + 2 * (slot as i32 / 3))
}

/// A grain of light going from one plot to another.
#[derive(Clone, Copy)]
struct Grain {
    from: u8,
    to: u8,
    /// Progress, 0 to `ARRIVED`.
    at: u16,
    speed: u8,
}

const GRAINS: usize = 28;
/// Where a grain's progress ends.
const ARRIVED: u16 = 511;

pub struct City {
    /// Heights as drawn: they follow the heights of the scene step by step.
    shown: [[u16; SLOTS]; PLOTS],
    grains: [Grain; GRAINS],
    frame: u32,
    random: u32,
}

/// A drawing surface: `WIDTH` by `HEIGHT` colors as 0xRRGGBB.
struct Canvas<'a> {
    pixels: &'a mut [u32],
}

impl Canvas<'_> {
    fn put(&mut self, x: i32, y: i32, color: u32) {
        if x >= 0 && y >= 0 && (x as usize) < WIDTH && (y as usize) < HEIGHT {
            self.pixels[y as usize * WIDTH + x as usize] = color;
        }
    }

    fn row(&mut self, x0: i32, x1: i32, y: i32, color: u32) {
        for x in x0..=x1 {
            self.put(x, y, color);
        }
    }

    fn column(&mut self, x: i32, y0: i32, y1: i32, color: u32) {
        for y in y0..=y1 {
            self.put(x, y, color);
        }
    }

    /// The top of a tile whose center is at (x, y): a flat diamond.
    fn diamond(&mut self, x: i32, y: i32, color: u32) {
        for dy in -3i32..=4 {
            let half = 8 - 2 * dy.abs() + if dy > 0 { 1 } else { -1 };
            self.row(x - half - 1, x + half, y + dy, color);
        }
    }

    /// A block standing on the tile centered at (x, y).
    fn block(&mut self, x: i32, y: i32, height: i32, top: u32, left: u32, right: u32) {
        for dx in 0..8 {
            // the two visible walls, one column of pixels at a time
            let edge = y + dx / 2;
            self.column(x - 8 + dx, edge - height, edge, left);
            let edge = y + 3 - dx / 2;
            self.column(x + dx, edge - height, edge, right);
        }
        self.diamond(x, y - height, top);
    }

    fn line(&mut self, from: (i32, i32), to: (i32, i32), color: u32) {
        let steps = (to.0 - from.0).abs().max((to.1 - from.1).abs()).max(1);
        for i in 0..=steps {
            self.put(from.0 + (to.0 - from.0) * i / steps, from.1 + (to.1 - from.1) * i / steps, color);
        }
    }

    /// Text in the system font, on a plate.
    fn label(&mut self, x: i32, y: i32, text: &[u8], ink: u32, plate: u32) {
        for py in -1..9 {
            self.row(x - 2, x + text.len() as i32 * 8, y + py, plate);
        }
        for (i, &ch) in text.iter().enumerate() {
            for (row, bits) in FONT[ch as usize].iter().enumerate() {
                for col in 0..8 {
                    if bits & (0x80 >> col) != 0 {
                        self.put(x + i as i32 * 8 + col, y + row as i32, ink);
                    }
                }
            }
        }
    }
}

impl City {
    pub const fn new() -> City {
        City { shown: [[0; SLOTS]; PLOTS], grains: [Grain { from: 0, to: 0, at: ARRIVED + 1, speed: 1 }; GRAINS], frame: 0, random: 0x2545_f491 }
    }

    fn next_random(&mut self) -> u32 {
        // xorshift: enough to scatter grains
        self.random ^= self.random << 13;
        self.random ^= self.random >> 17;
        self.random ^= self.random << 5;
        self.random
    }

    /// Advances the animation by one picture. The speeds are those of 25
    /// pictures per second (`shell::FRAME_TICKS`).
    pub fn step(&mut self, sys: &System) {
        self.frame = self.frame.wrapping_add(1);
        let plots = scene(sys);
        for (p, plot) in plots.iter().enumerate() {
            for (s, block) in plot.blocks.iter().enumerate() {
                let target = if block.used { block.height } else { 0 };
                let shown = &mut self.shown[p][s];
                // a sixth of the way each picture, at least one pixel
                if *shown < target {
                    *shown += ((target - *shown) / 6).max(1);
                } else if *shown > target {
                    *shown -= ((*shown - target) / 6).max(1);
                }
            }
        }
        // Grains travel between the plots that hold something; the busier the
        // system, the more of them and the faster. Serial traffic sends grains
        // between "elsewhere" and "running".
        let busy = sys.energy as u32 / 40 + sys.serial_rate.min(400) as u32 / 20;
        let wanted = (4 + busy as usize).min(GRAINS);
        let lively: [u8; 4] = [SETTINGS as u8, LOGS as u8, RUNNING as u8, ELSEWHERE as u8];
        for i in 0..GRAINS {
            if self.grains[i].at <= ARRIVED {
                self.grains[i].at += self.grains[i].speed as u16;
                continue;
            }
            if i >= wanted {
                continue;
            }
            let r = self.next_random();
            let (from, to) = if sys.serial_rate > 0 && r & 1 == 0 {
                if r & 2 == 0 { (ELSEWHERE as u8, RUNNING as u8) } else { (RUNNING as u8, ELSEWHERE as u8) }
            } else {
                let from = lively[(r >> 2) as usize % 4];
                let to = lively[(r >> 5) as usize % 4];
                (from, if to == from { RUNNING as u8 } else { to })
            };
            if from != to {
                self.grains[i] = Grain { from, to, at: 0, speed: 3 + (r >> 9) as u8 % 4 + (busy / 6) as u8 };
            }
        }
    }

    /// Draws the city into `pixels`, `WIDTH` * `HEIGHT` colors.
    pub fn draw(&self, sys: &System, theme: &Theme, pixels: &mut [u32]) {
        let sky = &theme.sky;
        let mut canvas = Canvas { pixels };

        // Sky down to the horizon, warmer when the mood is intense; then the
        // ground with the lines of its grid, dotted.
        let horizon = mix(sky.horizon_calm, sky.horizon_hot, (sys.energy as u32 * 255 / 1000).min(255));
        for y in 0..HEIGHT as i32 {
            if y < HORIZON {
                let color = mix(sky.top, horizon, (y * 255 / HORIZON) as u32);
                canvas.row(0, WIDTH as i32 - 1, y, color);
                continue;
            }
            // lines fade towards the horizon
            let line = mix(sky.ground, sky.grid, (60 + (y - HORIZON) * 2).min(255) as u32);
            for x in 0..WIDTH as i32 {
                let (dx, dy) = (x - ORIGIN_X, 2 * (y - ORIGIN_Y));
                let on_line = (dx + dy).rem_euclid(16) == 0 || (dy - dx).rem_euclid(16) == 0;
                canvas.put(x, y, if on_line && (x + y) % 2 == 0 { line } else { sky.ground });
            }
        }

        // The floor of each plot, tinted with its hue.
        for plot in 0..PLOTS {
            let hue = theme.colors[role::HUE as usize + plot];
            let floor = mix(sky.ground, hue, if sky.night { 70 } else { 55 });
            let (px, py) = PLOT_AT[plot];
            for ty in 0..PLOT_TILES {
                for tx in 0..PLOT_TILES {
                    let (x, y) = project(px + tx, py + ty);
                    canvas.diamond(x, y, floor);
                }
            }
        }

        // The lines that leave for elsewhere, to the right edge.
        let (ex, ey) = project(PLOT_AT[ELSEWHERE].0 + 3, PLOT_AT[ELSEWHERE].1 + 3);
        let away = theme.colors[role::HUE as usize + ELSEWHERE];
        for (i, end) in [58, 84, 116].iter().enumerate() {
            canvas.line((ex + 8 * i as i32, ey - 4), (WIDTH as i32 - 1, *end), mix(sky.ground, away, 150));
        }

        // Blocks, far ones first so that near ones cover them.
        let pulse = {
            let phase = self.frame / 2 % 16;
            (if phase < 8 { phase } else { 15 - phase }) * 12
        };
        let plots = scene(sys);
        for depth in 0..=40 {
            for plot in 0..PLOTS {
                for slot in 0..SLOTS {
                    let (tx, ty) = slot_tile(plot, slot);
                    let height = self.shown[plot][slot] as i32;
                    if tx + ty != depth || height == 0 {
                        continue;
                    }
                    let hue = theme.colors[role::HUE as usize + plot];
                    let lit = plots[plot].blocks[slot].lit;
                    let (top, left, right) = if sky.night {
                        let glow = if lit { mix(hue, 0xffffff, 60 + pulse) } else { hue };
                        (glow, mix(sky.ground, hue, 150), mix(sky.ground, hue, 90))
                    } else {
                        let light = mix(hue, 0xffffff, if lit { 150 - pulse } else { 110 });
                        (light, mix(hue, 0xffffff, 40), mix(hue, 0x000000, 50))
                    };
                    let (x, y) = project(tx, ty);
                    canvas.block(x, y, height, top, left, right);
                }
            }
        }

        // Grains of light.
        for grain in self.grains.iter().filter(|grain| grain.at <= ARRIVED) {
            let center = |plot: u8| project(PLOT_AT[plot as usize].0 + 3, PLOT_AT[plot as usize].1 + 3);
            let (a, b) = (center(grain.from), center(grain.to));
            let (t, whole) = (grain.at as i32, ARRIVED as i32);
            let (x, y) = (a.0 + (b.0 - a.0) * t / whole, a.1 + (b.1 - a.1) * t / whole);
            // a small arc, so that grains fly over the blocks
            let lift = 10 * t * (whole - t) / (256 * 256);
            let color = mix(theme.colors[role::HUE as usize + grain.from as usize], 0xffffff, 120);
            canvas.put(x, y - lift, color);
            canvas.put(x + 1, y - lift, color);
        }

        // Names, each beside a corner of its plot.
        for (plot, described) in plots.iter().enumerate() {
            let ((cx, cy), side, below) = NAME_AT[plot];
            let (x, y) = project(PLOT_AT[plot].0 + cx, PLOT_AT[plot].1 + cy);
            let width = described.name.len() as i32 * 8;
            let x = match side {
                -1 => x - 14 - width,
                0 => x - width / 2,
                _ => x + 14,
            };
            let hue = theme.colors[role::HUE as usize + plot];
            let plate = theme.colors[role::TERM_BASE as usize];
            canvas.label(x, y + below, described.name, hue, plate);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::{TaskInfo, MAX_TASKS};
    use crate::theme::DAY;

    fn system() -> System {
        let mut sys = System::new();
        sys.machine.serial = true;
        for (id, name) in [&b"interface"[..], b"idle", b"serial shell"].iter().enumerate() {
            sys.tasks[id] = TaskInfo { name: FixedStr::from_bytes(name), state: b"ready", ticks: 0, system: true, used: true };
        }
        sys
    }

    fn height(plots: &[Plot; PLOTS], plot: usize, name: &[u8]) -> Option<u16> {
        plots[plot].blocks.iter().find(|b| b.used && b.name.bytes() == name).map(|b| b.height)
    }

    #[test]
    fn a_busy_task_stands_taller_and_is_lit() {
        let mut sys = system();
        sys.cpu_share[1] = 1000; // the idle task has all the time
        let plots = scene(&sys);
        assert_eq!(height(&plots, RUNNING, b"idle"), Some(40));
        assert_eq!(height(&plots, RUNNING, b"interface"), Some(5));
        assert!(plots[RUNNING].blocks[1].lit && !plots[RUNNING].blocks[0].lit);
        assert_eq!(plots[RUNNING].blocks.iter().filter(|b| b.used).count(), 3);
    }

    /// Memory that says it is a disk.
    struct Platter(xila_db::Memory);

    impl xila_db::Disk for Platter {
        fn name(&self) -> &'static str {
            "platter"
        }
        fn pages(&self) -> u64 {
            self.0.pages()
        }
        fn read(&mut self, page: u64, into: &mut xila_db::Page) -> Result<(), xila_db::DiskError> {
            self.0.read(page, into)
        }
        fn write(&mut self, page: u64, from: &xila_db::Page) -> Result<(), xila_db::DiskError> {
            self.0.write(page, from)
        }
        fn flush(&mut self) -> Result<(), xila_db::DiskError> {
            Ok(())
        }
    }

    #[test]
    fn settings_logs_and_the_serial_port_follow_the_state() {
        let mut sys = system();
        let calm = scene(&sys);
        sys.energy = 1000;
        sys.log_rows = 9;
        sys.serial_rate = 80;
        let busy = scene(&sys);
        assert!(height(&busy, SETTINGS, b"mood") > height(&calm, SETTINGS, b"mood"));
        assert_eq!(height(&busy, LOGS, b"boot log"), Some(24));
        assert_eq!(height(&busy, ELSEWHERE, b"serial port"), Some(16));
        assert!(busy[HOME].blocks.iter().all(|b| !b.used));
        // a stored table lives at home and grows with its rows
        let notes = sys.db.create(b"notes", &[(b"title", xila_db::Kind::Text)]).unwrap();
        for _ in 0..4 {
            sys.db.insert(notes, &[xila_db::Field::Text(b"x")]).unwrap();
        }
        sys.db.table_mut(notes).written_at = 1;
        assert_eq!(height(&scene(&sys), HOME, b"notes"), Some(13));
        assert!(scene(&sys)[HOME].blocks[0].lit);
        // tables in memory have no disk to show; on a disk they have one
        assert_eq!(height(&scene(&sys), ELSEWHERE, b"disk"), None);
        let mut on_disk = system();
        on_disk.db = xila_db::Database::open(xila_db::Volume::single(Box::new(Platter(xila_db::Memory::new(64))))).ok().unwrap();
        on_disk.db.create(b"notes", &[(b"title", xila_db::Kind::Text)]).unwrap();
        on_disk.db.commit(&mut |_, _, _| {}).unwrap();
        let before = scene(&on_disk);
        assert_eq!(height(&before, ELSEWHERE, b"disk"), Some(6));
        assert!(!before[ELSEWHERE].blocks.iter().any(|b| b.lit));
        on_disk.stored_at = on_disk.ticks.max(1);
        assert!(scene(&on_disk)[ELSEWHERE].blocks.iter().any(|b| b.lit && b.name.bytes() == b"disk"));
        // a table that was just read is lit, for three seconds
        let tasks = crate::catalog::find(b"tasks").unwrap();
        assert_eq!(height(&busy, DATA, b"tasks"), Some(24));
        assert!(!busy[DATA].blocks[tasks].lit);
        sys.ticks = 1000;
        sys.queried[tasks] = 900;
        assert!(scene(&sys)[DATA].blocks[tasks].lit);
        sys.ticks = 1300;
        assert!(!scene(&sys)[DATA].blocks[tasks].lit);
        sys.machine.serial = false;
        assert_eq!(height(&scene(&sys), ELSEWHERE, b"serial port"), None);
    }

    #[test]
    fn shown_heights_move_towards_the_scene() {
        let mut sys = system();
        sys.cpu_share[2] = 1000;
        let mut city = City::new();
        city.step(&sys);
        let first = city.shown[RUNNING][2];
        assert!(first > 0 && first < 40);
        for _ in 0..40 {
            city.step(&sys);
        }
        assert_eq!(city.shown[RUNNING][2], 40);
        sys.tasks[2] = TaskInfo::NONE;
        for _ in 0..40 {
            city.step(&sys);
        }
        assert_eq!(city.shown[RUNNING][2], 0);
        assert_eq!(MAX_TASKS, 8);
    }

    #[test]
    fn drawing_fills_the_picture_and_stays_inside() {
        let mut sys = system();
        sys.cpu_share = [300, 500, 200, 0, 0, 0, 0, 0];
        let mut city = City::new();
        for _ in 0..30 {
            city.step(&sys);
        }
        let mut pixels = vec![0xdead_beef_u32; WIDTH * HEIGHT];
        city.draw(&sys, &DAY, &mut pixels);
        assert!(pixels.iter().all(|&p| p != 0xdead_beef));
        // sky at the top, ground at the bottom corners
        assert_eq!(pixels[0], DAY.sky.top);
        assert_eq!(pixels[(HEIGHT - 1) * WIDTH + 1], DAY.sky.ground);
        // the hue of the running plot is somewhere in the picture
        let green = mix(DAY.colors[role::HUE as usize + RUNNING], 0xffffff, 40);
        assert!(pixels.contains(&green));
    }

    #[test]
    fn a_job_that_runs_is_a_block_among_what_runs() {
        let mut sys = Box::new(System::new());
        crate::machine::tests::rooms(&mut sys, 3);
        let blocks = |sys: &System, plot: usize| scene(sys)[plot].blocks.iter().filter(|block| block.used).map(|block| String::from_utf8_lossy(block.name.bytes()).into_owned()).collect::<Vec<_>>();
        let before = blocks(&sys, RUNNING);
        for line in ["program spins", "while 1 = 1", "end", "end", "submit spins"] {
            crate::shell::exec(&mut sys, line.as_bytes(), &mut crate::shell::tests::Lines::default());
        }
        let mut with = before.clone();
        with.push("spins".to_string());
        assert_eq!(blocks(&sys, RUNNING), with);
        // it grows with what it runs, and is lit while it does
        let low = scene(&sys)[RUNNING].blocks[before.len()];
        for _ in 0..20_000 {
            crate::jobs::work(&mut sys, &mut crate::shell::tests::Lines::default(), &mut || true);
        }
        let high = scene(&sys)[RUNNING].blocks[before.len()];
        assert!(high.height > low.height && high.lit);
        // ended, it is no longer there
        crate::shell::exec(&mut sys, b"job 1 end", &mut crate::shell::tests::Lines::default());
        assert_eq!(blocks(&sys, RUNNING), before);
        // the view of the jobs takes no block among the data: nine places, nine blocks
        assert_eq!(blocks(&sys, DATA).len(), 9);
        assert!(!blocks(&sys, DATA).contains(&"jobs".to_string()));
    }
}
