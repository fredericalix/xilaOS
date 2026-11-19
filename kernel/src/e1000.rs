//! Intel Ethernet cards of the family that started with the 82540: the
//! 82574 that QEMU imitates, and the 82579 soldered in a ThinkPad T430.
//!
//! A card is driven through registers in memory and two rings it shares
//! with the driver, each a list of descriptors: an address in memory, a
//! length and a status. The card fills the buffers of the receive ring
//! with the frames that arrive and marks their descriptors done; the
//! driver writes frames in the buffers of the transmit ring and moves a
//! register to say how far the card may send.
//!
//! No interrupt is used: a task asks again and again whether a frame came.
//!
//! The 82579 is two chips, one in the machine's main chip that counts
//! frames and one near the socket that speaks on the cable (the PHY), and
//! it is shared with the part of the machine that manages it from a
//! distance (the ME), which has its say in its registers. What Linux's
//! driver does for it, this does too, as far as it goes:
//! - it tells the ME that a driver has taken the card (`DRIVER_LOADED`);
//! - it waits, before writing a register, for the ME not to be writing;
//! - it sets the bits of the transmit side that the card's maker asks for;
//! - when the link comes up at 100 or 1000 megabits, it turns off a way
//!   of saving power between the two chips that loses packets (K1).
//!
//! A register is changed bit by bit, never written whole, unless every
//! one of its bits is known. Five trials on a T430 received every frame
//! and sent none because the transmit control register was written whole:
//! its top bits, which no table of this family names, say how the card
//! reads a frame out of its own memory to send it, and with them at 0
//! frames went into that memory and never left it. The firmware's driver
//! and iPXE's change only their own bits there, and so does this one now.
//! What found it was not reading registers of settings but measuring: a
//! frame sent to oneself, then where the card says the frame is (`TDFH`
//! and `TDFT` in `net registers`: where it reads and writes in its send
//! memory), in several ways of setting the card up.
//!
//! The first chip is reset before it is set up, at start and by
//! `restart`, as Linux and iPXE do each time they take a card. Taking it
//! as the firmware left it worked when the firmware had not used it, and
//! not after a start from the network: the firmware had then run the card
//! with rings of its own, and the card went on with what it remembered of
//! them until it was reset. The chip near the socket is never reset, so
//! the link stays as it is.
//!
//! Register numbers, bits and sequences are those of Linux's e1000e driver
//! (`regs.h`, `defines.h`, `ich8lan.c`, `phy.c`, `netdev.c`), read for the
//! occasion. What is particular to the 82579 cannot be tried in QEMU.

use core::ptr::{read_volatile, write_volatile};

use xila_core::net::{Mac, Wire, FRAME};

use crate::arch;
use crate::memory;
use crate::pci;

/// What such a device says it is: a network card, Ethernet.
pub const CLASS: u32 = 0x02_00_00;

/// The cards this drives, by what they say on the PCI bus.
pub fn known(id: u32) -> Option<&'static str> {
    Some(match id {
        0x100e_8086 => "Intel 82540EM",
        0x10d3_8086 => "Intel 82574L",
        0x1502_8086 => "Intel 82579LM",
        0x1503_8086 => "Intel 82579V",
        _ => return None,
    })
}

// Registers.
const CONTROL: usize = 0x0000;
const STATUS: usize = 0x0008;
const CONTROL_MORE: usize = 0x0018;
const PHY_ACCESS: usize = 0x0020;
const BEACON: usize = 0x0024;
/// The registers `net registers` shows first, with their name in Linux's
/// driver.
const SHOWN: [(&str, usize); 19] = [
    ("CTRL", CONTROL),
    ("STATUS", STATUS),
    ("CTRL_EXT", CONTROL_MORE),
    ("RCTL", RECEIVE),
    ("TCTL", TRANSMIT),
    ("FEXTNVM", 0x0028),
    ("PBA", PACKET_ROOM),
    ("ICR", INTERRUPT_CAUSE),
    ("RDH", RECEIVE_HEAD),
    ("RDT", RECEIVE_TAIL),
    ("TDH", TRANSMIT_HEAD),
    ("TDT", TRANSMIT_TAIL),
    ("TDBAL", TRANSMIT_RING),
    ("TDLEN", TRANSMIT_BYTES),
    ("TXDCTL", TRANSMIT_HOW[0]),
    ("TARC0", TRANSMIT_TURNS[0]),
    ("EXTCNF_CTRL", SHARED),
    ("PHY_CTRL", 0x0f10),
    ("FWSM", FIRMWARE),
];
const PACKET_ROOM: usize = 0x1000;
const PACKET_SIZE: usize = 0x1008;
/// What wakes the machine from the network while it sleeps.
const WAKE_UP: usize = 0x5800;
const INTERRUPT_CAUSE: usize = 0x00c0;
const INTERRUPTS_OFF: usize = 0x00d8;
const RECEIVE: usize = 0x0100;
const TRANSMIT: usize = 0x0400;
const SHARED: usize = 0x0f00;
const RECEIVE_RING: usize = 0x2800;
const RECEIVE_BYTES: usize = 0x2808;
const RECEIVE_HEAD: usize = 0x2810;
const RECEIVE_TAIL: usize = 0x2818;
const SQUELCH: usize = 0x3004;
const TRANSMIT_RING: usize = 0x3800;
const TRANSMIT_BYTES: usize = 0x3808;
const TRANSMIT_HEAD: usize = 0x3810;
const TRANSMIT_TAIL: usize = 0x3818;
const TRANSMIT_HOW: [usize; 2] = [0x3828, 0x3928];
const TRANSMIT_TURNS: [usize; 2] = [0x3840, 0x3940];
const ADDRESS_LOW: usize = 0x5400;
const ADDRESS_HIGH: usize = 0x5404;
const FIRMWARE: usize = 0x5b54;
// counters, which go back to 0 when they are read
const BAD_FRAMES: usize = 0x4000;
const MISSED: usize = 0x4010;

const CONTROL_LINK_UP: u32 = 0x40;
const CONTROL_FORCED: u32 = 0x800 | 0x1000;
const CONTROL_RESET: u32 = 0x0400_0000;
const STATUS_FULL: u32 = 0x01;
const STATUS_LINK: u32 = 0x02;
/// The chip near the socket was reset since this was last cleared.
const STATUS_PHY_WAS_RESET: u32 = 0x400;
/// "A driver has taken the card", for the ME; and two bits Linux sets.
const DRIVER_LOADED: u32 = 0x1000_0000;
const MORE_ASKED: u32 = 1 << 22 | 0x0010_0000;
/// The ME is writing to the registers: a write now may be lost.
const FIRMWARE_WRITES: u32 = 0x0100_0000;
/// The lock on the chip near the socket.
const SHARED_OURS: u32 = 0x20;
const RECEIVE_ON: u32 = 0x02;
/// Frames for everyone are taken too, and the four bytes that check a
/// frame are not given with it.
const RECEIVE_ALL_CARDS: u32 = 0x8000;
const RECEIVE_NO_CHECK: u32 = 0x0400_0000;
const TRANSMIT_ON: u32 = 0x02;
/// Short frames are filled; what to do when two cards talk at once, and
/// to send again a frame that met another one late.
const TRANSMIT_FILL: u32 = 0x08;
const TRANSMIT_COLLISIONS: u32 = 15 << 4 | 63 << 12 | 0x0100_0000;
const TRANSMIT_TWO_AT_ONCE: u32 = 0x1000_0000;
const ADDRESS_VALID: u32 = 0x8000_0000;

// To read or write a register of the chip near the socket.
const PHY_WRITE: u32 = 0x0400_0000;
const PHY_READ: u32 = 0x0800_0000;
const PHY_READY: u32 = 0x1000_0000;
const PHY_ERROR: u32 = 0x4000_0000;
/// Its register that picks a page of registers, and its registers that
/// tell the link and that hold the K1 bit: (page, register).
const PHY_PAGE: u32 = 31;
const PHY_LINK: (u32, u32) = (0, 26);
const PHY_POWER: (u32, u32) = (770, 17);
const PHY_LINK_UP: u16 = 0x0040 | 0x1000;
const PHY_FAST: u16 = 0x0200 | 0x0100;
const PHY_K1: u16 = 0x4000;

const DONE: u8 = 0x01;
/// In a transmit descriptor: the end of a frame, add its check, report.
const SEND: u8 = 0x01 | 0x02 | 0x08;

const RECEIVERS: usize = 32;
const SENDERS: usize = 16;
const BUFFER: usize = 2048;
const PAGE: usize = 4096;

/// A descriptor of either ring: sixteen bytes.
#[repr(C)]
#[derive(Clone, Copy)]
struct Descriptor {
    address: u64,
    length: u16,
    /// Receive: a checksum. Transmit: a byte of nothing, and the command.
    extra: [u8; 2],
    status: u8,
    errors: u8,
    special: u16,
}

/// Waits so many microseconds.
fn pause(micros: u64) {
    let start = arch::micros();
    while arch::micros() - start < micros {
        core::hint::spin_loop();
    }
}

pub struct Nic {
    base: usize,
    pub name: &'static str,
    pub mac: Mac,
    /// The 82579 and its ways.
    shared: bool,
    rings: usize,
    receive: *mut Descriptor,
    transmit: *mut Descriptor,
    receive_buffers: usize,
    transmit_buffers: usize,
    /// The next receive descriptor to look at.
    next_in: usize,
    /// How many frames were given to the card since it was set up, and
    /// how many of them it has sent.
    given: usize,
    done: usize,
    /// Whether the link was up when last looked at.
    up: bool,
    /// What was done about K1: 0 not looked at, 1 turned off, 2 the chip
    /// near the socket did not answer, 3 not needed.
    k1: u32,
    /// Frames the card saw damaged, and frames it had no room for.
    pub damaged: u64,
    pub missed: u64,
    /// Frames given to the card to send, frames it had no room for, and
    /// times it did not take the register that says how far to send.
    pub sent: u64,
    pub refused: u64,
    lost: u64,
}

// The memory of the rings belongs to whoever holds the `Nic`.
unsafe impl Send for Nic {}

impl Nic {
    unsafe fn get(&self, register: usize) -> u32 {
        read_volatile((self.base + register) as *const u32)
    }

    unsafe fn set(&self, register: usize, value: u32) {
        if self.shared {
            // not while the ME writes: a tenth of a second at most
            for _ in 0..2000 {
                if self.get(FIRMWARE) & FIRMWARE_WRITES == 0 {
                    break;
                }
                pause(50);
            }
        }
        write_volatile((self.base + register) as *mut u32, value);
    }

    /// Takes a card found on the PCI bus. None if its registers cannot be
    /// reached, if it has no address, or if there is no memory.
    pub fn start(function: pci::Function, id: u32) -> Option<Nic> {
        let name = known(id)?;
        let low = function.read(0x10);
        if low & 1 != 0 {
            return None;
        }
        let high = if low & 6 == 4 { function.read(0x14) } else { 0 };
        let base = ((high as u64) << 32 | (low & !0xf) as u64) as usize;
        if base == 0 || base as u64 + 0x2_0000 > arch::mapped() {
            return None;
        }
        // registers in memory, and the card may read and write the memory of the machine
        function.enable(0x06);
        let rings = memory::pages(2)?;
        let receive_buffers = memory::pages(RECEIVERS * BUFFER / PAGE)?;
        let transmit_buffers = memory::pages(SENDERS * BUFFER / PAGE)?;
        let mut nic = Nic {
            base,
            name,
            mac: [0; 6],
            shared: matches!(id >> 16, 0x1502 | 0x1503),
            rings,
            receive: rings as *mut Descriptor,
            transmit: (rings + PAGE) as *mut Descriptor,
            receive_buffers,
            transmit_buffers,
            next_in: 0,
            given: 0,
            done: 0,
            up: false,
            k1: 0,
            damaged: 0,
            missed: 0,
            sent: 0,
            refused: 0,
            lost: 0,
        };
        unsafe { nic.reset().then_some(nic) }
    }

    /// Resets the card, but not the chip near the socket, and sets it up
    /// again.
    pub fn restart(&mut self) -> bool {
        (self.up, self.k1) = (false, 0);
        unsafe { self.reset() }
    }

    /// Resets the card and sets it up: whatever it was doing for someone
    /// else, the firmware for one, is forgotten.
    unsafe fn reset(&mut self) -> bool {
        // no interrupt, and nothing in or out meanwhile
        self.set(INTERRUPTS_OFF, !0);
        self.set(RECEIVE, 0);
        self.set(TRANSMIT, self.get(TRANSMIT) & !TRANSMIT_ON);
        pause(10_000);
        if self.shared {
            // the reset must not fall in the middle of a word with the other chip
            self.lock();
        }
        if self.shared {
            // the room for frames in the memory of the card, as it is when
            // the machine is turned on: it counts from the reset on
            self.set(PACKET_ROOM, 18);
            self.set(PACKET_SIZE, 32);
        }
        let control = self.get(CONTROL);
        self.set(CONTROL, control | CONTROL_RESET);
        // the card is not to be read while it resets; the lock goes with the reset
        pause(20_000);
        self.set(INTERRUPTS_OFF, !0);
        self.get(INTERRUPT_CAUSE);
        if self.shared {
            self.set(SQUELCH, self.get(SQUELCH) | 0x0005_0000);
        }
        self.set_up()
    }

    /// Gives the card its rings and turns it on. False if it has no address.
    unsafe fn set_up(&mut self) -> bool {
        core::ptr::write_bytes(self.rings as *mut u8, 0, 2 * PAGE);
        (self.next_in, self.given, self.done) = (0, 0, 0);
        if self.shared {
            self.set(CONTROL_MORE, self.get(CONTROL_MORE) | DRIVER_LOADED | MORE_ASKED);
            // the card is not to wake the machine: the machine is awake
            self.set(WAKE_UP, 0);
            self.set(WAKE_UP + 8, 0);
        }
        // the address the maker gave the card, which the firmware put here
        let (low, high) = (self.get(ADDRESS_LOW), self.get(ADDRESS_HIGH));
        self.mac = [low as u8, (low >> 8) as u8, (low >> 16) as u8, (low >> 24) as u8, high as u8, (high >> 8) as u8];
        if high & ADDRESS_VALID == 0 || self.mac == [0; 6] || self.mac == [0xff; 6] {
            return false;
        }
        for at in 0..RECEIVERS {
            self.receive.add(at).write_volatile(Descriptor { address: (self.receive_buffers + at * BUFFER) as u64, length: 0, extra: [0; 2], status: 0, errors: 0, special: 0 });
        }
        let (receive, transmit) = (self.rings as u64, (self.rings + PAGE) as u64);
        self.set(RECEIVE_RING, receive as u32);
        self.set(RECEIVE_RING + 4, (receive >> 32) as u32);
        self.set(RECEIVE_BYTES, (RECEIVERS * 16) as u32);
        self.set(RECEIVE_HEAD, 0);
        // all the buffers but one are the card's to fill
        self.set(RECEIVE_TAIL, RECEIVERS as u32 - 1);
        self.set(TRANSMIT_RING, transmit as u32);
        self.set(TRANSMIT_RING + 4, (transmit >> 32) as u32);
        self.set(TRANSMIT_BYTES, (SENDERS * 16) as u32);
        self.set(TRANSMIT_HEAD, 0);
        self.set(TRANSMIT_TAIL, 0);
        if self.shared {
            // What the maker asks of the transmit side of this card: a bit
            // in each of its two queues, bits in how they take turns, and
            // descriptors reported one at a time.
            let how = self.get(TRANSMIT_HOW[0]) | 1 << 22;
            let how = (how & !0x003f_0000 | 0x0101_0000) & !0x3f | 0x0100_001f;
            self.set(TRANSMIT_HOW[0], how);
            self.set(TRANSMIT_HOW[1], how);
            self.set(TRANSMIT_TURNS[0], self.get(TRANSMIT_TURNS[0]) | 1 << 23 | 1 << 24 | 1 << 26 | 1 << 27);
            let turns = self.get(TRANSMIT_TURNS[1]) | 1 << 24 | 1 << 26 | 1 << 30;
            let two = self.get(TRANSMIT) & TRANSMIT_TWO_AT_ONCE != 0;
            self.set(TRANSMIT_TURNS[1], if two { turns & !(1 << 28) } else { turns | 1 << 28 });
            // the reset of the other chip is acknowledged
            let status = self.get(STATUS);
            if status & STATUS_PHY_WAS_RESET != 0 {
                self.set(STATUS, status & !STATUS_PHY_WAS_RESET);
            }
        }
        // the link: up, at the speed the two ends agree on
        self.set(CONTROL, (self.get(CONTROL) | CONTROL_LINK_UP) & !CONTROL_FORCED);
        // only the bits this driver means: see the top of this file
        let kept = self.get(TRANSMIT) & !(0xff << 4 | 0x3ff << 12);
        self.set(TRANSMIT, kept | TRANSMIT_ON | TRANSMIT_FILL | TRANSMIT_COLLISIONS);
        self.set(RECEIVE, RECEIVE_ON | RECEIVE_ALL_CARDS | RECEIVE_NO_CHECK);
        // the counters start from now
        self.get(BAD_FRAMES);
        self.get(MISSED);
        true
    }

    /// Takes the lock on the chip near the socket. False if the ME keeps it.
    unsafe fn lock(&self) -> bool {
        // somebody has it: a tenth of a second for it to be given back
        for _ in 0..100 {
            if self.get(SHARED) & SHARED_OURS == 0 {
                break;
            }
            pause(1000);
        }
        self.set(SHARED, self.get(SHARED) | SHARED_OURS);
        for _ in 0..200 {
            if self.get(SHARED) & SHARED_OURS != 0 {
                return true;
            }
            pause(1000);
        }
        self.set(SHARED, self.get(SHARED) & !SHARED_OURS);
        false
    }

    unsafe fn unlock(&self) {
        self.set(SHARED, self.get(SHARED) & !SHARED_OURS);
    }

    /// One word with the chip near the socket: reads a register of the
    /// chip at an address, or writes it. None if it does not answer.
    unsafe fn word(&self, address: u32, register: u32, write: Option<u16>) -> Option<u16> {
        let what = match write {
            Some(value) => value as u32 | PHY_WRITE,
            None => PHY_READ,
        };
        self.set(PHY_ACCESS, what | register << 16 | address << 21);
        let mut answer = 0;
        for _ in 0..1920 {
            pause(50);
            answer = self.get(PHY_ACCESS);
            if answer & PHY_READY != 0 {
                break;
            }
        }
        if answer & PHY_READY == 0 || answer & PHY_ERROR != 0 || answer >> 16 & 0x1f != register {
            return None;
        }
        // this chip needs a moment between two words
        pause(100);
        Some(answer as u16)
    }

    /// Reads or writes a register of a page of the chip near the socket.
    /// The lock must be held.
    unsafe fn phy(&self, (page, register): (u32, u32), write: Option<u16>) -> Option<u16> {
        // the pages from 768 on answer at address 1, the others at 2
        let address = if page >= 768 { 1 } else { 2 };
        if register > 15 {
            // the page is picked first, always at address 1
            self.word(1, PHY_PAGE, Some(((if page == 768 { 0 } else { page }) << 5) as u16))?;
        }
        self.word(address, register, write)
    }

    /// Looks at the link, and does what a link that just came up asks for.
    /// Returns the speed in megabits per second and whether both ends may
    /// talk at once; None if the cable is out or the other end is silent.
    pub fn link(&mut self) -> Option<(u32, bool)> {
        let status = unsafe { self.get(STATUS) };
        let up = status & STATUS_LINK != 0;
        if up && !self.up {
            unsafe { self.came_up() };
        }
        self.up = up;
        if !up {
            return None;
        }
        let speed = match status >> 6 & 3 {
            0 => 10,
            1 => 100,
            _ => 1000,
        };
        Some((speed, status & STATUS_FULL != 0))
    }

    unsafe fn came_up(&mut self) {
        self.k1_off();
    }

    /// Between the two chips of this card, a way of saving power (K1)
    /// loses packets at 100 and 1000 megabits: off. At 10, the signal that
    /// wakes the link is made longer instead.
    unsafe fn k1_off(&mut self) {
        if !self.shared {
            return;
        }
        self.k1 = 2;
        if self.lock() {
            if let Some(link) = self.phy(PHY_LINK, None) {
                self.k1 = 3;
                if link & PHY_LINK_UP == PHY_LINK_UP {
                    if link & PHY_FAST != 0 {
                        if let Some(power) = self.phy(PHY_POWER, None) {
                            self.k1 = if self.phy(PHY_POWER, Some(power & !PHY_K1)).is_some() { 1 } else { 2 };
                        } else {
                            self.k1 = 2;
                        }
                    } else {
                        self.set(BEACON, self.get(BEACON) & !7 | 3);
                    }
                }
            }
            self.unlock();
        }
    }

    /// The registers that tell what state the card is in, as they are
    /// now, with their name in Linux's driver and their address; then what
    /// the driver itself knows. Reading them changes nothing but, on the
    /// 82579, the page the chip near the socket is turned to.
    pub fn registers(&self) -> [(&'static str, u32, u32); xila_core::shell::NET_REGISTERS] {
        let mut all = [("", 0, 0); xila_core::shell::NET_REGISTERS];
        for (slot, (name, register)) in all.iter_mut().zip(SHOWN) {
            *slot = (name, register as u32, unsafe { self.get(register) });
        }
        // the chip near the socket: ffffffff if it does not answer
        let (mut link, mut power) = (!0, !0);
        if self.shared && unsafe { self.lock() } {
            unsafe {
                link = self.phy(PHY_LINK, None).map_or(!0, |value| value as u32);
                power = self.phy(PHY_POWER, None).map_or(!0, |value| value as u32);
                self.unlock();
            }
        }
        let first = unsafe { self.transmit.read_volatile() };
        let more = [("TIPG", 0x0410), ("RXDCTL", 0x2828), ("WUC", WAKE_UP), ("TDFH", 0x3410), ("TDFT", 0x3418), ("FEXTNVM4", BEACON), ("MANC", 0x5820)];
        for (slot, (name, register)) in all[SHOWN.len()..].iter_mut().zip(more) {
            *slot = (name, register as u32, unsafe { self.get(register) });
        }
        let rest = [
            ("phy link", 26, link),
            ("phy power", 17, power),
            ("k1", 0, self.k1),
            ("given to the card", 0, self.given as u32),
            ("first, its status", 0, first.status as u32),
            ("words lost", 0, self.lost as u32),
        ];
        all[SHOWN.len() + more.len()..].copy_from_slice(&rest);
        all
    }

    /// Adds to the counters what the card counted since the last time.
    pub fn count(&mut self) {
        unsafe {
            self.damaged += self.get(BAD_FRAMES) as u64;
            self.missed += self.get(MISSED) as u64;
        }
    }

    /// The next frame that came, copied into `into`. None if none did.
    pub fn receive(&mut self, into: &mut [u8; BUFFER]) -> Option<usize> {
        loop {
            let slot = unsafe { self.receive.add(self.next_in) };
            let found = unsafe { slot.read_volatile() };
            if found.status & DONE == 0 {
                return None;
            }
            let length = (found.length as usize).min(BUFFER);
            unsafe {
                core::ptr::copy_nonoverlapping((self.receive_buffers + self.next_in * BUFFER) as *const u8, into.as_mut_ptr(), length);
                // the buffer is the card's again
                slot.write_volatile(Descriptor { status: 0, errors: 0, length: 0, ..found });
                self.set(RECEIVE_TAIL, self.next_in as u32);
            }
            self.next_in = (self.next_in + 1) % RECEIVERS;
            // a frame the card saw damaged is not one
            if found.errors == 0 && length > 0 {
                return Some(length);
            }
        }
    }
}

impl Wire for Nic {
    /// Gives a frame to the card. It is dropped if the card has not sent
    /// the ones before: the network makes no promise, and nor does this.
    fn send(&mut self, frame: &[u8]) {
        if frame.len() > FRAME {
            return;
        }
        unsafe {
            // the frames the card says it is done with free their buffers
            while self.done != self.given && self.transmit.add(self.done % SENDERS).read_volatile().status & DONE != 0 {
                self.done += 1;
            }
            // one stays free: a full ring looks, to the card, like an empty one
            if self.given - self.done >= SENDERS - 1 {
                self.refused += 1;
                return;
            }
            let buffer = self.transmit_buffers + self.given % SENDERS * BUFFER;
            core::ptr::copy_nonoverlapping(frame.as_ptr(), buffer as *mut u8, frame.len());
            let slot = self.transmit.add(self.given % SENDERS);
            slot.write_volatile(Descriptor { address: buffer as u64, length: frame.len() as u16, extra: [0, SEND], status: 0, errors: 0, special: 0 });
            self.given += 1;
            let tail = (self.given % SENDERS) as u32;
            self.set(TRANSMIT_TAIL, tail);
            if self.get(TRANSMIT_TAIL) != tail {
                // the ME wrote at the same moment and the card took its word: again
                self.lost += 1;
                self.set(TRANSMIT_TAIL, tail);
            }
        }
        self.sent += 1;
    }
}
