//! The graphics chip: finding it on the PCI bus, and reading its
//! registers for `xila_core::intel`, which knows what they mean.
//!
//! Nothing here writes to the entry of the chip on the bus. One register
//! of the chip is written, by `Flipper` and only once the user has asked
//! for it: which of two pictures the chip shows.

use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};

use xila_core::intel::{self, Card, Chip, Flip, Generation, NoFlip, State, REGISTERS_SIZE};

use crate::arch;
use crate::pci;
use crate::uefi::Framebuffer;

/// Where a device says a block of its registers or of its memory is: the
/// entry at `offset` of its table, which may take two entries. 0 if the
/// block is on ports instead.
fn block(function: pci::Function, offset: u8) -> u64 {
    let low = function.read(offset);
    if low & 1 != 0 {
        return 0;
    }
    let high = if low & 6 == 4 { function.read(offset + 4) } else { 0 };
    (high as u64) << 32 | (low & !0xf) as u64
}

/// The first graphics chip on the bus, with the screen of the firmware to
/// compare with what the chip says it shows.
pub fn find(screen: &Framebuffer) -> Option<Card> {
    let mut found = None;
    pci::scan(&mut |function, id, class| {
        if class >> 16 == 0x03 && found.is_none() {
            found = Some((function, id));
        }
    });
    let (function, id) = found?;
    let mut card = Card {
        id,
        revision: function.read(0x08) as u8,
        registers: 0,
        window: 0,
        stolen: 0,
        screen: screen.base as u64,
        line_bytes: (screen.stride * 4) as u32,
        width: screen.width as u32,
        height: screen.height as u32,
    };
    if intel::known(id).is_some() {
        let registers = block(function, 0x10);
        // they must answer in memory, and be inside what the kernel maps
        let answers = function.read(0x04) & 2 != 0;
        if answers && registers != 0 && registers + REGISTERS_SIZE <= arch::mapped() {
            card.registers = registers;
        }
        card.window = block(function, 0x18);
        // the memory the firmware set aside for the chip, to the megabyte
        card.stolen = (function.read(0x5c) & 0xfff0_0000) as u64;
    }
    Some(card)
}

struct Registers(u64);

impl Chip for Registers {
    fn read(&mut self, register: u32) -> u32 {
        assert!((register as u64) < REGISTERS_SIZE && register % 4 == 0);
        unsafe { read_volatile((self.0 + register as u64) as *const u32) }
    }

    fn micros(&mut self) -> u64 {
        arch::micros()
    }

    fn write(&mut self, register: u32, value: u32) {
        assert!((register as u64) < REGISTERS_SIZE && register % 4 == 0);
        unsafe { write_volatile((self.0 + register as u64) as *mut u32, value) }
    }
}

/// Reads what the chip is doing. `card.registers` must not be 0.
pub fn look(card: &Card, generation: Generation) -> State {
    intel::look(generation, &mut Registers(card.registers))
}

/// Two pictures, one of which is on the screen while the other is
/// prepared (`fb::Screen` does that part).
pub struct Flipper {
    kind: Kind,
    /// Where each picture is written.
    pictures: [*mut u32; 2],
}

enum Kind {
    /// The chip shows either picture, and changes between two sweeps.
    Chip(Registers, Flip),
    /// No chip: the pictures are in ordinary memory and showing one is
    /// copying it whole to the screen. This is to try the rest on any
    /// machine. Like a chip, it takes a moment to change: until the tick
    /// after next.
    Trial { _memory: [Vec<u32>; 2], screen: *mut u32, words: usize, shown: usize, asked: usize, at: u64 },
}

// The pictures belong to whoever holds the `Flipper`.
unsafe impl Send for Flipper {}

impl Flipper {
    /// Two pictures in the graphics memory of the chip: the screen as it
    /// is, and a second one behind it. Refused, with nothing changed, if
    /// what the chip is doing is not exactly what this expects.
    pub fn chip(card: &Card, generation: Generation) -> Result<Flipper, NoFlip> {
        let mut registers = Registers(card.registers);
        let state = intel::read(generation, &mut registers);
        let flip = Flip::new(card, &state)?;
        // the chip must agree, before anything is asked, that it shows the first one
        if flip.shown(&mut registers) != Some(0) {
            return Err(NoFlip::NotTheScreen);
        }
        let bytes = card.line_bytes as u64 * card.height as u64;
        let second = card.window + flip.pictures[1] as u64;
        // written like the screen, or the second picture would take thirty times longer
        if !arch::combine_more(second, second + bytes) || !arch::combines(second) || !arch::combines(second + bytes - 1) {
            return Err(NoFlip::Slow);
        }
        Ok(Flipper { kind: Kind::Chip(registers, flip), pictures: [card.screen as *mut u32, second as *mut u32] })
    }

    /// Two pictures in ordinary memory, for a screen of `words` pixels
    /// (those between the lines included) at `screen`.
    pub fn trial(screen: *mut u32, words: usize) -> Flipper {
        let mut memory = [alloc::vec![0u32; words], alloc::vec![0u32; words]];
        let pictures = [memory[0].as_mut_ptr(), memory[1].as_mut_ptr()];
        Flipper { kind: Kind::Trial { _memory: memory, screen, words, shown: 0, asked: 0, at: 0 }, pictures }
    }

    /// Where a picture, 0 or 1, is written.
    pub fn picture(&self, picture: usize) -> *mut u32 {
        self.pictures[picture]
    }

    /// Asks for a picture to be the one on the screen.
    pub fn show(&mut self, picture: usize) {
        match &mut self.kind {
            Kind::Chip(registers, flip) => flip.show(registers, picture),
            Kind::Trial { screen, words, asked, at, .. } => {
                unsafe {
                    core::arch::asm!(
                        "rep movsd",
                        inout("rcx") *words => _,
                        inout("rsi") self.pictures[picture] => _,
                        inout("rdi") *screen => _,
                        options(nostack, preserves_flags)
                    );
                }
                (*asked, *at) = (picture, arch::ticks());
            }
        }
    }

    /// The picture that is on the screen at this moment.
    pub fn shown(&mut self) -> Option<usize> {
        match &mut self.kind {
            Kind::Chip(registers, flip) => flip.shown(registers),
            Kind::Trial { shown, asked, at, .. } => {
                if arch::ticks() > *at + 1 {
                    *shown = *asked;
                }
                Some(*shown)
            }
        }
    }

    /// How many times the screen was swept, by the count of the chip. The
    /// trial kind makes a count up from the timer, as a screen swept sixty
    /// times per second would give: what follows the count can then be
    /// tried without a chip.
    pub fn sweeps(&mut self) -> Option<u32> {
        match &mut self.kind {
            Kind::Chip(registers, flip) => Some(flip.sweeps(registers)),
            Kind::Trial { .. } => Some((arch::ticks() * 6 / 10) as u32),
        }
    }

    /// Whether the pictures are those of the chip.
    pub fn of_chip(&self) -> bool {
        matches!(self.kind, Kind::Chip(..))
    }
}

/// The brightness of the screen in hundredths, after setting it if `wish`
/// says to what. None if this chip does not set it.
pub fn light(card: &Card, generation: Generation, wish: Option<u32>) -> Option<u32> {
    let mut registers = Registers(card.registers);
    let light = intel::Light::new(&intel::read(generation, &mut registers))?;
    Some(match wish {
        Some(hundredths) => light.set(&mut registers, hundredths),
        None => light.get(&mut registers),
    })
}
