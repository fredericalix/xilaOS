//! The graphics chip inside Intel processors of 2011 and 2012: HD Graphics
//! 2000 and 3000 (Sandy Bridge), 2500 and 4000 (Ivy Bridge).
//!
//! This module first reads: it finds out what the firmware has set up,
//! the picture, the plane it comes from, the panel, and says it in a
//! table, so that what the chip does on a real machine is known before
//! anything is asked of it.
//!
//! It writes two registers. One is the address of the picture the main
//! plane shows (`Flip`): the chip takes a new address between two sweeps
//! of the screen, so a picture prepared out of sight replaces the one
//! shown all at once, and nothing is ever seen half drawn. The other is
//! how long the light behind the panel is on in each of its cycles
//! (`Light`), when the user asks for another brightness.
//!
//! How the chip shows a picture, in the order the picture travels:
//!
//! - a *plane* reads pixels from graphics memory: an address in the chip's
//!   own map of memory, which a table (one entry per page of 4 KB) turns
//!   into memory of the machine. There is a main plane, a second one that
//!   can lie over it, and a small one for a pointer;
//! - a *pipe* puts the planes together and gives the picture its timing:
//!   so many pixels and lines, so many times per second;
//! - the *panel fitter* may stretch the picture of a pipe to the size of
//!   the panel;
//! - an *output* sends it to a screen: the panel of a portable (LVDS), or
//!   a socket.
//!
//! The addresses of the registers and the meaning of their bits are those
//! of the Linux driver for these chips (`i915_reg.h`), checked against it.
//! Nothing here could be tried in QEMU, which imitates no such chip: the
//! tests work on values made up from that description.

use core::fmt::Write;

use crate::shell::{Table, Text};

/// Which of the two generations a chip is: some registers moved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Generation {
    /// Sandy Bridge, 2011.
    Six,
    /// Ivy Bridge, 2012.
    Seven,
}

/// The name of a chip this module knows and its generation, from what it
/// says on the PCI bus: `model << 16 | maker`.
pub fn known(id: u32) -> Option<(&'static str, Generation)> {
    if id & 0xffff != 0x8086 {
        return None;
    }
    Some(match id >> 16 {
        0x0102 | 0x0106 => ("Intel HD Graphics 2000", Generation::Six),
        0x0112 | 0x0122 | 0x0116 | 0x0126 => ("Intel HD Graphics 3000", Generation::Six),
        0x010a => ("Intel HD Graphics P3000", Generation::Six),
        0x0152 | 0x0156 | 0x015a => ("Intel HD Graphics 2500", Generation::Seven),
        0x0162 | 0x0166 => ("Intel HD Graphics 4000", Generation::Seven),
        0x016a => ("Intel HD Graphics P4000", Generation::Seven),
        _ => return None,
    })
}

/// What the kernel found of the graphics chip on the PCI bus, and of the
/// screen the firmware set up, to compare.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Card {
    /// `model << 16 | maker`.
    pub id: u32,
    pub revision: u8,
    /// Where its registers are in the memory of the machine; 0 if they
    /// cannot be reached.
    pub registers: u64,
    /// Where its graphics memory shows in the memory of the machine.
    pub window: u64,
    /// The memory of the machine the firmware set aside for the chip.
    pub stolen: u64,
    /// The screen of the firmware: where it is, and bytes per line.
    pub screen: u64,
    pub line_bytes: u32,
    pub width: u32,
    pub height: u32,
}

/// The registers of the chip, and the time: what reading its state takes.
pub trait Chip {
    /// A register, by its address from the first one. From `ENTRIES` on,
    /// the table that maps graphics memory.
    fn read(&mut self, register: u32) -> u32;
    /// Microseconds since some moment.
    fn micros(&mut self) -> u64;
    /// Writes a register. Only `Flip::show` and `Light::set` do.
    fn write(&mut self, register: u32, value: u32);
}

/// How many registers `look` keeps.
pub const COUNT: usize = 32;

// What each of them is: for the pipe in use, then for the whole chip.
const ACROSS: usize = 1;
const DOWN: usize = 2;
const SOURCE: usize = 3;
const PLANE: usize = 4;
const OFFSET: usize = 5;
const STRIDE: usize = 6;
const SURFACE: usize = 7;
const FITTER: usize = 9;
const FITTER_SIZE: usize = 10;
const FITTER_PLACE: usize = 11;
const SECOND: usize = 12;
const POINTER: usize = 13;
const FRAMES: usize = 14;
const LINE: usize = 15;
const PANEL: usize = 16;
const POWER: usize = 17;
const LIGHT_ON: usize = 19;
const LIGHT: usize = 20;
const LIGHT_PCH: usize = 21;
const LIGHT_MOST: usize = 22;
const SOCKETS: usize = 23;

/// Their names in the Linux driver, to look them up.
const NAMES: [&str; COUNT] = [
    "PIPECONF", "HTOTAL", "VTOTAL", "PIPESRC", "DSPCNTR", "DSPLINOFF", "DSPSTRIDE", "DSPSURF", "DSPTILEOFF", "PF_CTL", "PF_WIN_SZ",
    "PF_WIN_POS", "SPRCTL", "CURCNTR", "FRMCOUNT", "PIPEDSL", "PCH_LVDS", "PP_STATUS", "PP_CONTROL", "BLC_CPU_CTL2", "BLC_CPU_CTL",
    "BLC_PCH_CTL1", "BLC_PCH_CTL2", "PCH_ADPA", "PCH_HDMIB", "PCH_HDMIC", "PCH_HDMID", "PCH_DP_B", "PCH_DP_C", "PCH_DP_D", "DEIMR", "DEIER",
];

/// The sockets, in the order of their registers from `SOCKETS` on.
const SOCKET_NAMES: [&str; 7] = ["VGA", "HDMI B", "HDMI C", "HDMI D", "DisplayPort B", "DisplayPort C", "DisplayPort D"];

/// In all of these, the bit that says the thing is on.
const ON: u32 = 1 << 31;

/// Where the table that maps graphics memory starts, among the registers.
pub const ENTRIES: u32 = 0x20_0000;
/// The registers and that table take this much of the memory of the machine.
pub const REGISTERS_SIZE: u64 = 0x40_0000;
/// Most entries of that table there can be.
const MOST_ENTRIES: u32 = 0x20_0000 / 4;
/// Most entries `look` follows after the start of the plane: 128 MB.
const MOST_FOLLOWED: u32 = 128 << 8;

fn pipe_conf(pipe: usize) -> u32 {
    0x70008 + 0x1000 * pipe as u32
}

/// The addresses of the registers `look` keeps, for a pipe.
fn addresses(generation: Generation, pipe: usize) -> [u32; COUNT] {
    let p = 0x1000 * pipe as u32;
    let seven = generation == Generation::Seven;
    [
        pipe_conf(pipe),
        0x60000 + p,
        0x6000c + p,
        0x6001c + p,
        0x70180 + p,
        0x70184 + p,
        0x70188 + p,
        0x7019c + p,
        0x701a4 + p,
        0x68080 + 0x800 * pipe as u32,
        0x68074 + 0x800 * pipe as u32,
        0x68070 + 0x800 * pipe as u32,
        // the second plane and the pointer are where the generations differ
        if seven { 0x70280 + p } else { 0x72180 + p },
        if seven || pipe == 0 { 0x70080 + p } else { 0x700c0 },
        0x70040 + p,
        0x70000 + p,
        0xe1180,
        0xc7200,
        0xc7204,
        0x48250,
        0x48254,
        0xc8250,
        0xc8254,
        0xe1100,
        0xe1140,
        0xe1150,
        0xe1160,
        0xe4100,
        0xe4200,
        0xe4300,
        0x44004,
        0x4400c,
    ]
}

/// What the chip is doing, as read from it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct State {
    pub generation: Generation,
    /// The first pipe that is on: 0 is pipe A.
    pub pipe: Option<usize>,
    /// The registers, for that pipe (for pipe A when none is on).
    pub values: [u32; COUNT],
    /// Pictures per second, in thousandths; 0 if none could be counted.
    pub rate: u32,
    /// They were counted on the line being shown, because the chip's own
    /// count of pictures did not move.
    pub by_line: bool,
    /// The memory of the machine behind the first page of the plane, how
    /// many pages are mapped in a row from there, and whether they follow
    /// each other in the memory of the machine too.
    pub memory: u64,
    pub mapped: u32,
    pub one_piece: bool,
}

/// How long pictures are counted, and how long a count may stand still.
const COUNTED_US: u64 = 400_000;
const PATIENCE_US: u64 = 150_000;

/// Counts pictures per second, in thousandths. `passed` reads the chip
/// and tells how many pictures went by since it was last called.
fn rate(chip: &mut dyn Chip, passed: &mut dyn FnMut(&mut dyn Chip) -> u32) -> u32 {
    // from the start of a picture...
    let start = chip.micros();
    passed(chip);
    while passed(chip) == 0 {
        if chip.micros() - start > PATIENCE_US {
            return 0;
        }
    }
    let from = chip.micros();
    // ...to the start of another, some time later
    let mut pictures = 0u64;
    let mut last = from;
    loop {
        let now = chip.micros();
        let more = passed(chip);
        if more > 0 {
            pictures += more as u64;
            last = now;
            if now - from >= COUNTED_US {
                return (pictures * 1_000_000_000 / (now - from)) as u32;
            }
        } else if now - last > PATIENCE_US {
            return 0;
        }
    }
}

/// Reads the state of the chip, and counts its pictures for half a
/// second to tell how many it shows per second. Nothing is written to it.
pub fn look(generation: Generation, chip: &mut dyn Chip) -> State {
    let mut state = read(generation, chip);
    if state.pipe.is_none() {
        return state;
    }
    let at = addresses(generation, state.pipe.unwrap_or(0));
    // the chip counts its pictures...
    let mut seen = chip.read(at[FRAMES]);
    state.rate = rate(chip, &mut |chip| {
        let now = chip.read(at[FRAMES]);
        let passed = now.wrapping_sub(seen);
        seen = now;
        passed
    });
    if state.rate == 0 {
        // ...or else the line being shown starts again from the top
        state.by_line = true;
        let mut seen = chip.read(at[LINE]) & 0x1fff;
        state.rate = rate(chip, &mut |chip| {
            let now = chip.read(at[LINE]) & 0x1fff;
            let passed = (now < seen) as u32;
            seen = now;
            passed
        });
    }
    state
}

/// Reads the state of the chip, at once: `rate` is left at 0. Nothing is
/// written to it.
pub fn read(generation: Generation, chip: &mut dyn Chip) -> State {
    let pipes = if generation == Generation::Seven { 3 } else { 2 };
    let pipe = (0..pipes).find(|pipe| chip.read(pipe_conf(*pipe)) & ON != 0);
    let at = addresses(generation, pipe.unwrap_or(0));
    let mut values = [0; COUNT];
    for (value, address) in values.iter_mut().zip(at) {
        *value = chip.read(address);
    }

    // The memory behind the plane: the entries of the table from its
    // first page on, as long as they are in use.
    let (mut memory, mut mapped, mut one_piece) = (0, 0, true);
    if values[PLANE] & ON != 0 {
        let first = values[SURFACE] >> 12;
        while mapped < MOST_FOLLOWED && first + mapped < MOST_ENTRIES {
            let entry = chip.read(ENTRIES + 4 * (first + mapped));
            if entry & 1 == 0 {
                break;
            }
            // bits 4 to 11 of an entry are bits 32 to 39 of the address
            let page = (entry & 0xffff_f000) as u64 | ((entry >> 4) as u64 & 0xff) << 32;
            if mapped == 0 {
                memory = page;
            } else if page != memory + mapped as u64 * 4096 {
                one_piece = false;
            }
            mapped += 1;
        }
    }
    State { generation, pipe, values, rate: 0, by_line: false, memory, mapped, one_piece }
}

/// Whether the plane the chip shows is the screen the firmware gave the
/// kernel: at the same place, with lines as long, of the same size.
fn is_screen(card: &Card, v: &[u32; COUNT]) -> bool {
    let (width, height) = (halves(v[SOURCE]).0 + 1, halves(v[SOURCE]).1 + 1);
    card.window + v[SURFACE] as u64 + v[OFFSET] as u64 == card.screen && v[STRIDE] == card.line_bytes && (width, height) == (card.width, card.height)
}

/// Showing one of two pictures while the other is prepared.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Flip {
    /// The register that says which picture the plane shows, and the one
    /// that tells which it is showing at this moment.
    surface: u32,
    live: u32,
    /// The register that counts the sweeps of the screen.
    sweeps: u32,
    /// The graphics addresses of the two pictures. The first is the
    /// screen of the firmware.
    pub pictures: [u32; 2],
}

/// Why a chip cannot be given two pictures.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoFlip {
    /// No pipe is on, or its plane is off.
    NoPicture,
    /// The plane does not show plain pixels of 32 bits.
    Pixels,
    /// What the plane shows is not the screen the firmware gave.
    NotTheScreen,
    /// Not enough graphics memory is mapped behind the screen.
    NoRoom,
    /// The memory of the second picture could not be made quick to write.
    Slow,
    /// The chip did not show the second picture when told to.
    Deaf,
}

impl NoFlip {
    /// The reason in a few words, for the boot log.
    pub fn words(self) -> &'static str {
        match self {
            NoFlip::NoPicture => "the chip shows no picture",
            NoFlip::Pixels => "pixels of another kind",
            NoFlip::NotTheScreen => "the chip shows another screen",
            NoFlip::NoRoom => "no room for a second picture",
            NoFlip::Slow => "a second picture would be slow",
            NoFlip::Deaf => "the chip did not follow",
        }
    }
}

/// A second picture starts on a boundary of this many bytes: the largest
/// pages of the processor, so that its memory is marked with whole pages.
const BOUNDARY: u32 = 1 << 21;

impl Flip {
    /// Where a second picture can be, if the state of the chip allows
    /// two: right behind the screen, in graphics memory the firmware has
    /// mapped already. Nothing is written.
    pub fn new(card: &Card, state: &State) -> Result<Flip, NoFlip> {
        let v = &state.values;
        let Some(pipe) = state.pipe.filter(|_| v[PLANE] & ON != 0) else {
            return Err(NoFlip::NoPicture);
        };
        // 32 bits a pixel, blue first or red first, in lines and not in tiles
        if !matches!(v[PLANE] >> 26 & 0xf, 0x6 | 0xe) || v[PLANE] & 1 << 10 != 0 {
            return Err(NoFlip::Pixels);
        }
        if !is_screen(card, v) || v[OFFSET] != 0 || v[SURFACE] & 0xfff != 0 {
            return Err(NoFlip::NotTheScreen);
        }
        let bytes = v[STRIDE] as u64 * card.height as u64;
        let second = (v[SURFACE] as u64 + bytes).next_multiple_of(BOUNDARY as u64);
        let mapped = v[SURFACE] as u64 + state.mapped as u64 * 4096;
        if !state.one_piece || second + bytes > mapped || second + bytes > u32::MAX as u64 {
            return Err(NoFlip::NoRoom);
        }
        let p = 0x1000 * pipe as u32;
        Ok(Flip { surface: 0x7019c + p, live: 0x701ac + p, sweeps: 0x70040 + p, pictures: [v[SURFACE], second as u32] })
    }

    /// Tells the chip to show a picture, 0 or 1. It does at the start of
    /// the next sweep of the screen.
    pub fn show(&self, chip: &mut dyn Chip, picture: usize) {
        chip.write(self.surface, self.pictures[picture]);
    }

    /// How many times the screen was swept, as the chip counts: a number
    /// that goes round after 2^32.
    pub fn sweeps(&self, chip: &mut dyn Chip) -> u32 {
        chip.read(self.sweeps)
    }

    /// The picture the chip is showing at this moment; None if it is
    /// neither of the two.
    pub fn shown(&self, chip: &mut dyn Chip) -> Option<usize> {
        let address = chip.read(self.live) & !0xfff;
        self.pictures.iter().position(|picture| *picture == address)
    }
}

/// The light behind the panel. It is on for so many parts of a cycle that
/// has `most` of them; the eye sees the share as a brightness.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Light {
    /// The register that holds the lit part, in its low half.
    register: u32,
    pub most: u32,
}

/// The least the light is set to, in hundredths: below, the screen cannot
/// be read to set it back.
pub const LEAST_LIGHT: u32 = 5;

impl Light {
    /// The light of the panel, if this chip is what sets it.
    pub fn new(state: &State) -> Option<Light> {
        let v = &state.values;
        let most = v[LIGHT_MOST] >> 16;
        // both halves of the chip must have their part of the light on
        if most == 0 || v[LIGHT_ON] & ON == 0 || v[LIGHT_PCH] & ON == 0 {
            return None;
        }
        // the lit part is kept in one of two places, as a bit says
        let register = if v[LIGHT_PCH] & 1 << 30 != 0 { 0xc8254 } else { 0x48254 };
        Some(Light { register, most })
    }

    /// The brightness now, in hundredths.
    pub fn get(&self, chip: &mut dyn Chip) -> u32 {
        ((chip.read(self.register) & 0xffff).min(self.most) * 100 + self.most / 2) / self.most
    }

    /// Sets the brightness, in hundredths, between `LEAST_LIGHT` and 100.
    /// Returns what it is then.
    pub fn set(&self, chip: &mut dyn Chip, hundredths: u32) -> u32 {
        let lit = self.most * hundredths.clamp(LEAST_LIGHT, 100) / 100;
        let kept = chip.read(self.register) & !0xffff;
        chip.write(self.register, kept | lit);
        self.get(chip)
    }
}

/// A size kept in a register as two halves of 16 bits.
fn halves(value: u32) -> (u32, u32) {
    (value >> 16, value & 0xffff)
}

/// Says what the chip is doing, for someone who reads the screen.
pub fn describe(card: &Card, state: &State, t: &mut Table) {
    let v = &state.values;
    let row = |t: &mut Table, item: &[u8], value: core::fmt::Arguments| {
        let mut text = Text::new();
        let _ = text.write_fmt(value);
        t.row(&[item, text.bytes()]);
    };
    let name = known(card.id).map_or("unknown", |(name, _)| name);
    row(t, b"chip", format_args!("{name} ({:04x}:{:04x}), revision {}", card.id & 0xffff, card.id >> 16, card.revision));
    row(t, b"registers", format_args!("at {:x}; graphics memory at {:x}", card.registers, card.window));
    let Some(pipe) = state.pipe else {
        row(t, b"picture", format_args!("no pipe is on"));
        return;
    };
    let letter = (b'A' + pipe as u8) as char;
    // the picture a pipe is given, then what it sends: pixels, and pixels with the blanks
    let (width, height) = (halves(v[SOURCE]).0 + 1, halves(v[SOURCE]).1 + 1);
    let (across, down) = (halves(v[ACROSS]), halves(v[DOWN]));
    row(t, b"picture", format_args!("{width}x{height}, from pipe {letter}"));
    let by = if state.by_line { " (by the scan line)" } else { "" };
    row(t, b"pictures per second", format_args!("{}.{:02}{by}", state.rate / 1000, state.rate % 1000 / 10));
    row(t, b"sent to the screen", format_args!("{}x{}, {}x{} with the blanks", across.1 + 1, down.1 + 1, across.0 + 1, down.0 + 1));
    let (size, place) = (halves(v[FITTER_SIZE]), halves(v[FITTER_PLACE]));
    if v[FITTER] & ON != 0 && (size, place) == ((width, height), (0, 0)) {
        row(t, b"stretched", format_args!("no (the panel fitter is on at the same size)"));
    } else if v[FITTER] & ON != 0 {
        row(t, b"stretched", format_args!("yes, to {}x{} at {},{}", size.0, size.1, place.0, place.1));
    } else {
        row(t, b"stretched", format_args!("no"));
    }

    if v[PLANE] & ON == 0 {
        row(t, b"plane", format_args!("off"));
    } else {
        let bits = match v[PLANE] >> 26 & 0xf {
            0x2 => "8 bits per pixel",
            0x5 => "16 bits per pixel",
            0x6 => "32 bits per pixel",
            0xa | 0x8 => "30 bits of color per pixel",
            0xe => "32 bits per pixel, red first",
            _ => "a pixel XilaOS does not write",
        };
        let tiled = if v[PLANE] & 1 << 10 != 0 { ", in tiles" } else { "" };
        row(t, b"plane", format_args!("on, {bits}{tiled}"));
        row(t, b"plane memory", format_args!("graphics address {:x}, lines of {} bytes", v[SURFACE] as u64 + v[OFFSET] as u64, v[STRIDE]));
        row(t, b"the firmware's screen", format_args!("{}", if is_screen(card, v) { "is this plane" } else { "is NOT this plane" }));
        if state.mapped == 0 {
            row(t, b"memory behind it", format_args!("none: the plane shows nothing mapped"));
        } else {
            let pieces = if state.one_piece { "in one piece" } else { "in pieces" };
            row(t, b"memory behind it", format_args!("at {:x}, {} KB mapped {pieces}", state.memory, state.mapped * 4));
        }
        row(t, b"set aside for the chip", format_args!("memory from {:x}", card.stolen));
    }

    if v[PANEL] & ON != 0 {
        let from = (b'A' + (v[PANEL] >> 29 & 3) as u8) as char;
        let power = if v[POWER] & ON != 0 { "power on" } else { "power off" };
        let wires = if v[PANEL] >> 4 & 3 == 3 { ", two channels" } else { "" };
        row(t, b"panel", format_args!("LVDS, on, pipe {from}, {power}{wires}"));
    } else {
        row(t, b"panel", format_args!("LVDS, off"));
    }
    let mut sockets = Text::new();
    for (name, value) in SOCKET_NAMES.iter().zip(&v[SOCKETS..]) {
        if value & ON != 0 {
            let _ = write!(sockets, "{}{name}", if sockets.is_empty() { "" } else { ", " });
        }
    }
    row(t, b"sockets in use", format_args!("{}", if sockets.is_empty() { "none" } else { core::str::from_utf8(sockets.bytes()).unwrap_or("?") }));
    // The light behind the panel: so many parts of a cycle, kept in one
    // of two places.
    let most = v[LIGHT_MOST] >> 16;
    let lit = if v[LIGHT_PCH] & 1 << 30 != 0 { v[LIGHT_MOST] & 0xffff } else { v[LIGHT] & 0xffff };
    if most == 0 {
        row(t, b"brightness", format_args!("not set by this chip"));
    } else {
        row(t, b"brightness", format_args!("{lit} of {most}"));
    }
    row(t, b"second plane", format_args!("{}", if v[SECOND] & ON != 0 { "on" } else { "off" }));
    row(t, b"pointer plane", format_args!("{}", if v[POINTER] & 0x27 != 0 { "on" } else { "off" }));
    row(t, b"line being shown", format_args!("{}", v[LINE] & 0x1fff));
}

/// Lists the registers as they were read, for whoever wants to check what
/// `describe` says against the description of the chip.
pub fn registers(state: &State, t: &mut Table) {
    let at = addresses(state.generation, state.pipe.unwrap_or(0));
    let letter = (b'A' + state.pipe.unwrap_or(0) as u8) as char;
    for (i, name) in NAMES.iter().enumerate() {
        let (mut register, mut address, mut value) = (Text::new(), Text::new(), Text::new());
        // the first ones are those of a pipe
        let _ = if i < PANEL { write!(register, "{name} {letter}") } else { write!(register, "{name}") };
        let _ = write!(address, "{:05x}", at[i]);
        let _ = write!(value, "{:08x}", state.values[i]);
        t.row(&[register.bytes(), address.bytes(), value.bytes()]);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A chip made of the values its registers hold. It shows 60 pictures
    /// of 800 lines per second (or none), and time goes by as it is read.
    pub(crate) struct Made {
        pub registers: HashMap<u32, u32>,
        pub now: u64,
        pub counts: bool,
        pub runs: bool,
        pub reads: usize,
        /// What was written, and the picture that waits for the next sweep.
        pub written: Vec<(u32, u32)>,
        waiting: Option<(u32, u64)>,
    }

    impl Chip for Made {
        fn read(&mut self, register: u32) -> u32 {
            self.reads += 1;
            self.now += 3;
            let pictures = self.now * 60 / 1_000_000;
            if let Some((value, _)) = self.waiting.filter(|(_, since)| pictures > *since) {
                self.registers.insert(0x701ac, value);
                self.waiting = None;
            }
            match register {
                0x70040 if self.counts && self.runs => 1000 + pictures as u32,
                0x70000 if self.runs => ((self.now * 60 % 1_000_000) * 800 / 1_000_000) as u32,
                _ => self.registers.get(&register).copied().unwrap_or(0),
            }
        }

        fn micros(&mut self) -> u64 {
            self.now += 1;
            self.now
        }

        fn write(&mut self, register: u32, value: u32) {
            self.written.push((register, value));
            self.registers.insert(register, value);
            if register == 0x7019c && self.runs {
                self.waiting = Some((value, self.now * 60 / 1_000_000));
            }
        }
    }

    /// The portable of the author as its firmware could leave it: the
    /// panel of 1366x768 on pipe A, 64 MB set aside for the chip and all
    /// of it mapped from graphics address 0, where the screen is.
    pub(crate) fn t430() -> (Card, Made) {
        let card = Card {
            id: 0x0166_8086,
            revision: 9,
            registers: 0xf000_0000,
            window: 0xe000_0000,
            stolen: 0xdb20_0000,
            screen: 0xe000_0000,
            line_bytes: 5504,
            width: 1366,
            height: 768,
        };
        let mut registers = HashMap::from([
            (0x70008, 0xc000_0000),
            (0x60000, 1591 << 16 | 1365),
            (0x6000c, 797 << 16 | 767),
            (0x6001c, 1365 << 16 | 767),
            (0x70180, 0x9800_4000),
            (0x70188, 5504),
            (0xe1180, 0x8000_0302),
            (0xc7200, 0x8000_0008),
            (0x48254, 812),
            (0xc8250, 0x8000_0000),
            (0xc8254, 4882 << 16),
        ]);
        for page in 0..(64 << 8) {
            registers.insert(ENTRIES + 4 * page, (0xdb20_0000 + page * 4096) | 3 << 1 | 1);
        }
        (card, Made { registers, now: 0, counts: true, runs: true, reads: 0, written: Vec::new(), waiting: None })
    }

    fn rows(card: &Card, state: &State) -> Vec<String> {
        let mut t = Table::new(b"chip", &[b"item", b"value"]);
        describe(card, state, &mut t);
        (0..t.rows).map(|row| String::from_utf8_lossy(t.aligned(Some(row)).bytes()).trim().to_string()).collect()
    }

    #[test]
    fn the_chips_of_those_years_are_known() {
        assert_eq!(known(0x0166_8086), Some(("Intel HD Graphics 4000", Generation::Seven)));
        assert_eq!(known(0x0126_8086), Some(("Intel HD Graphics 3000", Generation::Six)));
        // the screen of QEMU, and a chip of another maker with the same number
        assert_eq!(known(0x1111_1234), None);
        assert_eq!(known(0x0166_10de), None);
        // a later chip of the same maker: its registers are not the same
        assert_eq!(known(0x0416_8086), None);
    }

    #[test]
    fn what_the_firmware_set_up_is_told() {
        let (card, mut chip) = t430();
        let state = look(Generation::Seven, &mut chip);
        assert_eq!(state.pipe, Some(0));
        assert!((59_900..=60_100).contains(&state.rate) && !state.by_line, "{}", state.rate);
        assert_eq!((state.memory, state.mapped, state.one_piece), (0xdb20_0000, 64 << 8, true));
        assert_eq!(
            rows(&card, &state)[..15],
            [
                "chip                    Intel HD Graphics 4000 (8086:0166), revision 9",
                "registers               at f0000000; graphics memory at e0000000",
                "picture                 1366x768, from pipe A",
                "pictures per second     60.00",
                "sent to the screen      1366x768, 1592x798 with the blanks",
                "stretched               no",
                "plane                   on, 32 bits per pixel",
                "plane memory            graphics address 0, lines of 5504 bytes",
                "the firmware's screen   is this plane",
                "memory behind it        at db200000, 65536 KB mapped in one piece",
                "set aside for the chip  memory from db200000",
                "panel                   LVDS, on, pipe A, power on",
                "sockets in use          none",
                "brightness              812 of 4882",
                "second plane            off",
            ]
        );
    }

    #[test]
    fn a_stretched_picture_and_other_states_are_told() {
        // as the T430 started before XilaOS chose the size of its screen:
        // 640x480 stretched to the panel, on a screen that is not at address 0
        let (mut card, mut chip) = t430();
        (card.width, card.height, card.line_bytes, card.screen) = (640, 480, 2560, 0xe010_0000);
        chip.registers.extend([
            (0x6001c, 639 << 16 | 479),
            (0x70188, 2560),
            (0x7019c, 0x0010_0000),
            (0x68080, 0x8080_0000),
            (0x68074, 1366 << 16 | 768),
            (0x70280, 0x8000_0000),
            (0x70080, 0x0400_0027),
            (0xe1100, 0x8000_0000),
            (0xe4200, 0x8000_0000),
            (0xc8250, 0xc000_0000),
            (0xc8254, 4882 << 16 | 100),
        ]);
        // the count of pictures stands still on this one
        chip.counts = false;
        let state = look(Generation::Seven, &mut chip);
        assert!((59_500..=60_500).contains(&state.rate) && state.by_line, "{}", state.rate);
        // the screen starts 1 MB into the 64 that are mapped
        assert_eq!((state.memory, state.mapped), (0xdb30_0000, 63 << 8));
        let told = rows(&card, &state);
        assert_eq!(told[2], "picture                 640x480, from pipe A");
        assert!(told[3].ends_with(" (by the scan line)"), "{}", told[3]);
        assert_eq!(told[5], "stretched               yes, to 1366x768 at 0,0");
        assert_eq!(told[7], "plane memory            graphics address 100000, lines of 2560 bytes");
        assert_eq!(told[8], "the firmware's screen   is this plane");
        assert_eq!(told[12], "sockets in use          VGA, DisplayPort C");
        assert_eq!(told[13], "brightness              100 of 4882");
        assert_eq!(told[14..16], ["second plane            on", "pointer plane           on"]);

        // a screen that is somewhere else is said, not guessed
        card.screen = 0xe000_0000;
        assert_eq!(rows(&card, &state)[8], "the firmware's screen   is NOT this plane");
    }

    /// The registers of the author's T430 as `display registers` listed
    /// them on 18 November 2026, read off its screen as a code: the first
    /// values that come from a chip and not from its description.
    const T430_READ: [u32; COUNT] = [
        0xc000_0050, 0x077f_063f, 0x03a7_0383, 0x063f_0383, 0x9800_0000, 0, 0x1900, 0, 0, 0x8080_0000, 0x0640_0384, 0, 0, 0, 0x407d, 0x224,
        0x8030_033e, 0xc000_0008, 3, 0x8000_0000, 0x1155, 0x8000_0000, 0x1155_1155, 0x0004_0000, 0x1c, 0x1c, 0x1c, 4, 4, 4, 0xffff_ffff, 0,
    ];

    #[test]
    fn the_t430_reads_as_it_did_on_the_machine() {
        let card = Card {
            id: 0x0166_8086,
            revision: 9,
            registers: 0xf000_0000,
            window: 0xe000_0000,
            stolen: 0xdba0_0000,
            screen: 0xe000_0000,
            line_bytes: 6400,
            width: 1600,
            height: 900,
        };
        let mut registers: HashMap<u32, u32> = addresses(Generation::Seven, 0).into_iter().zip(T430_READ).collect();
        // 64 MB less a page were mapped behind the screen
        for page in 0..(64 << 8) - 1 {
            registers.insert(ENTRIES + 4 * page, (0xdba0_0000 + page * 4096) | 1);
        }
        let mut chip = Made { registers, now: 0, counts: true, runs: true, reads: 0, written: Vec::new(), waiting: None };
        let state = look(Generation::Seven, &mut chip);
        let told = rows(&card, &state);
        // word for word what the machine said, but for the lines reworded since
        assert_eq!(told[2], "picture                 1600x900, from pipe A");
        assert_eq!(told[4], "sent to the screen      1600x900, 1920x936 with the blanks");
        assert_eq!(told[5], "stretched               no (the panel fitter is on at the same size)");
        assert_eq!(told[6], "plane                   on, 32 bits per pixel");
        assert_eq!(told[7], "plane memory            graphics address 0, lines of 6400 bytes");
        assert_eq!(told[8], "the firmware's screen   is this plane");
        assert_eq!(told[9], "memory behind it        at dba00000, 65532 KB mapped in one piece");
        assert_eq!(told[10], "set aside for the chip  memory from dba00000");
        assert_eq!(told[11], "panel                   LVDS, on, pipe A, power on, two channels");
        assert_eq!(told[12], "sockets in use          none");
        assert_eq!(told[13], "brightness              4437 of 4437");
        assert_eq!(told[14..16], ["second plane            off", "pointer plane           off"]);
        assert!(told.iter().all(|line| line.len() <= 72), "a value is cut at 48 characters");
        // and it can be given two pictures: the second one 6 MB into graphics memory
        assert_eq!(Flip::new(&card, &state).map(|flip| flip.pictures), Ok([0, 0x60_0000]));
        // its light is set here, and was at its brightest
        let light = Light::new(&state).unwrap();
        assert_eq!((light.most, light.get(&mut chip)), (4437, 100));
        assert!(chip.written.is_empty());
    }

    #[test]
    fn the_light_of_the_panel_is_set_and_never_put_out() {
        let (_, mut chip) = t430();
        chip.registers.insert(0x48250, 0x8000_0000);
        let state = read(Generation::Seven, &mut chip);
        assert_eq!(state.rate, 0);
        let light = Light::new(&state).unwrap();
        assert_eq!((light.most, light.get(&mut chip)), (4882, 17));
        assert_eq!(light.set(&mut chip, 50), 50);
        assert_eq!(chip.written, [(0x48254, 2441)]);
        // never dark, never more than the whole cycle
        assert_eq!(light.set(&mut chip, 0), 5);
        assert_eq!(light.set(&mut chip, 1000), 100);
        assert_eq!(chip.written[1..], [(0x48254, 244), (0x48254, 4882)]);
        // what else the register holds stays
        chip.registers.insert(0x48254, 0x1234_0000 | 4882);
        light.set(&mut chip, 20);
        assert_eq!(chip.written.last(), Some(&(0x48254, 0x1234_0000 | 976)));
        // the other place the lit part can be kept
        chip.registers.insert(0xc8250, 0xc000_0000);
        let state = read(Generation::Seven, &mut chip);
        let light = Light::new(&state).unwrap();
        light.set(&mut chip, 100);
        assert_eq!(chip.written.last(), Some(&(0xc8254, 4882 << 16 | 4882)));
        // a chip that does not set the light is left alone
        for (register, value) in [(0x48250, 0), (0xc8250, 0), (0xc8254, 812)] {
            let (_, mut chip) = t430();
            chip.registers.insert(0x48250, 0x8000_0000);
            chip.registers.insert(register, value);
            let state = read(Generation::Seven, &mut chip);
            assert_eq!(Light::new(&state), None, "register {register:x}");
        }
    }

    #[test]
    fn a_second_picture_takes_the_place_of_the_first_between_two_sweeps() {
        let (card, mut chip) = t430();
        let state = look(Generation::Seven, &mut chip);
        assert!(chip.written.is_empty(), "looking writes nothing");
        // 1366x768 in lines of 5504 bytes is 4.1 MB: the second picture starts at 6 MB
        let flip = Flip::new(&card, &state).unwrap();
        assert_eq!(flip.pictures, [0, 0x60_0000]);
        assert_eq!(flip.shown(&mut chip), Some(0));
        // the chip counts the sweeps, sixty per second here
        let before = flip.sweeps(&mut chip);
        chip.now += 100_000;
        assert_eq!(flip.sweeps(&mut chip) - before, 6);
        flip.show(&mut chip, 1);
        assert_eq!(chip.written, [(0x7019c, 0x60_0000)]);
        // the chip goes on with the first one until the sweep is over
        assert_eq!(flip.shown(&mut chip), Some(0));
        chip.now += 17_000;
        assert_eq!(flip.shown(&mut chip), Some(1));
        flip.show(&mut chip, 0);
        chip.now += 17_000;
        assert_eq!(flip.shown(&mut chip), Some(0));
        // a chip that shows something else altogether
        chip.registers.insert(0x701ac, 0x123_4000);
        assert_eq!(flip.shown(&mut chip), None);
    }

    #[test]
    fn two_pictures_are_refused_where_they_could_go_wrong() {
        let refused = |change: &dyn Fn(&mut Card, &mut Made)| {
            let (mut card, mut chip) = t430();
            change(&mut card, &mut chip);
            let state = look(Generation::Seven, &mut chip);
            let flip = Flip::new(&card, &state);
            assert!(chip.written.is_empty());
            flip.err()
        };
        assert_eq!(refused(&|_, _| {}), None);
        assert_eq!(refused(&|_, chip| { chip.registers.remove(&0x70008); }), Some(NoFlip::NoPicture));
        assert_eq!(refused(&|_, chip| { chip.registers.insert(0x70180, 0x1800_0000); }), Some(NoFlip::NoPicture));
        // 16 bits per pixel, or tiles
        assert_eq!(refused(&|_, chip| { chip.registers.insert(0x70180, 0x9400_0000); }), Some(NoFlip::Pixels));
        assert_eq!(refused(&|_, chip| { chip.registers.insert(0x70180, 0x9800_0400); }), Some(NoFlip::Pixels));
        // the kernel's screen is not what the chip shows
        assert_eq!(refused(&|card, _| card.screen += 4096), Some(NoFlip::NotTheScreen));
        assert_eq!(refused(&|card, _| card.line_bytes = 5464), Some(NoFlip::NotTheScreen));
        assert_eq!(refused(&|card, _| card.height = 600), Some(NoFlip::NotTheScreen));
        // 10 MB mapped is the screen and not a second picture; and memory in pieces is not trusted
        assert_eq!(refused(&|_, chip| { chip.registers.insert(ENTRIES + 4 * (10 << 8), 0); }), Some(NoFlip::NoRoom));
        assert_eq!(refused(&|_, chip| { chip.registers.insert(ENTRIES + 4 * 3, 0xd000_0001); }), Some(NoFlip::NoRoom));
        // just enough: the second picture ends at 6 MB + 4.1 MB
        let last = (0x60_0000 + 5504 * 768u32).div_ceil(4096);
        assert_eq!(refused(&|_, chip| { chip.registers.insert(ENTRIES + 4 * last, 0); }), None);
        assert_eq!(refused(&|_, chip| { chip.registers.insert(ENTRIES + 4 * (last - 1), 0); }), Some(NoFlip::NoRoom));
        // the T430 as it is: 1600x900, the second picture at 6 MB too
        let (mut card, mut chip) = t430();
        (card.width, card.height, card.line_bytes) = (1600, 900, 6400);
        chip.registers.extend([(0x6001c, 1599 << 16 | 899), (0x70188, 6400), (0x70180, 0x9800_0000)]);
        let state = look(Generation::Seven, &mut chip);
        assert_eq!(Flip::new(&card, &state).unwrap().pictures, [0, 0x60_0000]);
    }

    #[test]
    fn a_chip_that_shows_nothing_is_read_without_waiting_for_ever() {
        let (card, mut chip) = t430();
        chip.registers.remove(&0x70008);
        let state = look(Generation::Seven, &mut chip);
        assert_eq!((state.pipe, state.rate), (None, 0));
        assert_eq!(rows(&card, &state)[2..], ["picture    no pipe is on"]);

        // a pipe that is on and does not run: both counts give up
        let (_, mut chip) = t430();
        chip.runs = false;
        let state = look(Generation::Seven, &mut chip);
        assert_eq!((state.pipe, state.rate, state.by_line), (Some(0), 0, true));
        assert!(chip.now < 1_000_000);

        // a plane with nothing mapped behind it, or mapped in pieces
        let (card, mut chip) = t430();
        chip.registers.insert(ENTRIES + 4 * 10, 0xdb10_0000 | 1);
        chip.registers.insert(ENTRIES + 4 * 20, 0);
        let state = look(Generation::Seven, &mut chip);
        assert_eq!((state.mapped, state.one_piece), (20, false));
        assert_eq!(rows(&card, &state)[9], "memory behind it        at db200000, 80 KB mapped in pieces");
        chip.registers.insert(ENTRIES, 0);
        let state = look(Generation::Seven, &mut chip);
        assert_eq!(rows(&card, &state)[9], "memory behind it        none: the plane shows nothing mapped");
    }

    #[test]
    fn the_second_pipe_and_the_older_chip_have_their_own_registers() {
        // pipe B of the older chip: its second plane and its pointer are elsewhere
        let (card, mut chip) = t430();
        chip.registers.remove(&0x70008);
        chip.registers.extend([
            (0x71008, 0x8000_0000),
            (0x6101c, 1023 << 16 | 767),
            (0x61000, 1343 << 16 | 1023),
            (0x6100c, 805 << 16 | 767),
            (0x71180, 0x9800_0000),
            (0x73180, 0x8000_0000),
            (0x700c0, 0x27),
            (0xe1180, 0xa000_0302),
        ]);
        let state = look(Generation::Six, &mut chip);
        assert_eq!(state.pipe, Some(1));
        let told = rows(&card, &state);
        assert_eq!(told[2], "picture                 1024x768, from pipe B");
        assert_eq!(told[11], "panel                   LVDS, on, pipe B, power on");
        assert_eq!(told[14..16], ["second plane            on", "pointer plane           on"]);

        let mut t = Table::new(b"registers", &[b"register", b"address", b"value"]);
        registers(&state, &mut t);
        assert_eq!(t.rows, COUNT);
        let line = |row: usize| String::from_utf8_lossy(t.aligned(Some(row)).bytes()).trim().to_string();
        assert_eq!(line(0), "PIPECONF B    71008    80000000");
        assert_eq!(line(12), "SPRCTL B      73180    80000000");
        assert_eq!(line(13), "CURCNTR B     700c0    00000027");
        assert_eq!(line(16), "PCH_LVDS      e1180    a0000302");
        // the newer chip looks elsewhere for the same things, and has a third pipe
        assert_eq!(addresses(Generation::Seven, 1)[SECOND], 0x71280);
        assert_eq!(addresses(Generation::Seven, 1)[POINTER], 0x71080);
        assert_eq!(addresses(Generation::Seven, 2)[POINTER], 0x72080);
        assert_eq!(addresses(Generation::Seven, 2)[FITTER], 0x69080);
        assert_eq!(addresses(Generation::Seven, 0)[POINTER], 0x70080);
    }
}
