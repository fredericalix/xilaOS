//! SATA disks, through an AHCI controller: the disks of a real PC.
//!
//! The controller is found on the PCI bus and driven through registers in
//! memory. Each of its ports may have a disk. A command for a disk is
//! written in memory the port was told about: an entry in its list of
//! commands, which points at a table with the command itself and at the
//! buffer for the data. A bit in a register of the port starts it; the bit
//! goes back to 0 when the disk is done.
//!
//! As with the virtio disk, one command runs at a time and the driver
//! waits for it by looking: no interrupt is used.
//!
//! The firmware has found the disks and brought them up before the kernel
//! starts. This driver takes the ports as they are: it does not reset the
//! controller, and it ignores a port whose disk does not answer.

use alloc::vec::Vec;
use core::ptr::{copy_nonoverlapping, read_volatile, write_bytes, write_volatile};

use xila_core::gpt::SECTOR;
use xila_core::text::FixedStr;
use xila_db::{DiskError, PAGE};

use crate::disk::wait_for;
use crate::memory;
use crate::pci;

/// What such a controller says it is: storage, SATA, AHCI.
pub const CLASS: u32 = 0x01_06_01;

// Registers of the controller.
const CAPABILITIES: usize = 0x00;
const CONTROL: usize = 0x04;
const PORTS: usize = 0x0c;
const CAPABILITIES_2: usize = 0x24;
const HANDOFF: usize = 0x28;

const CAN_64_BITS: u32 = 1 << 31;
const CONTROL_AHCI: u32 = 1 << 31;
const CONTROL_INTERRUPTS: u32 = 1 << 1;
const CAN_HAND_OFF: u32 = 1;
const HANDOFF_FIRMWARE: u32 = 1;
const HANDOFF_SYSTEM: u32 = 2;

// Registers of a port, from 0x100 + 0x80 * its number.
const COMMAND_LIST: usize = 0x00;
const RECEIVED: usize = 0x08;
const INTERRUPTS: usize = 0x10;
const INTERRUPTS_WANTED: usize = 0x14;
const COMMAND: usize = 0x18;
const TASK_FILE: usize = 0x20;
const SIGNATURE: usize = 0x24;
const LINK: usize = 0x28;
const LINK_ERRORS: usize = 0x30;
const ISSUE: usize = 0x38;

const COMMAND_START: u32 = 1;
const COMMAND_SPIN_UP: u32 = 1 << 1;
const COMMAND_POWER: u32 = 1 << 2;
const COMMAND_RECEIVE: u32 = 1 << 4;
const COMMAND_RECEIVING: u32 = 1 << 14;
const COMMAND_RUNNING: u32 = 1 << 15;
/// The disk has an error, or wants data, or is busy.
const TASK_FILE_ERROR: u32 = 1;
const TASK_FILE_NOT_READY: u32 = 0x88;
/// The interrupt bits that mean a command failed.
const FAILED: u32 = 0x7800_0000;

const SIGNATURE_PACKET: u32 = 0xeb14_0101;
const SIGNATURE_MULTIPLIER: u32 = 0x9669_0101;
const SIGNATURE_ENCLOSURE: u32 = 0xc33c_0101;

// Commands of a disk.
const READ: u8 = 0x25;
const WRITE: u8 = 0x35;
const IDENTIFY: u8 = 0xec;
const FLUSH: u8 = 0xe7;
const FLUSH_48: u8 = 0xea;

// Where things are in the page of memory of a port.
const RECEIVED_AT: usize = 0x400;
const TABLE_AT: usize = 0x800;
/// In a command table, where the list of buffers starts.
const BUFFERS_AT: usize = 0x80;

/// How long things may take, in ticks.
const STOP_PATIENCE: u64 = 50;
const COMMAND_PATIENCE: u64 = 3000;

unsafe fn get(base: usize, register: usize) -> u32 {
    read_volatile((base + register) as *const u32)
}

unsafe fn set(base: usize, register: usize, value: u32) {
    write_volatile((base + register) as *mut u32, value);
}

/// A disk on a port.
///
/// A copy is another hand on the same disk, with the same memory for its
/// commands: the database holds one and `install` another. Only one of them
/// may be at work at a time, which the lock on what the tasks share sees to.
#[derive(Clone)]
pub struct Sata {
    port: usize,
    /// One page: the list of commands, then where the port puts what the
    /// disk answers, then the table of the one command.
    memory: usize,
    /// One page of data, which the controller reads and writes.
    buffer: usize,
    pub sectors: u64,
    /// What the disk says it is.
    pub model: FixedStr<40>,
    /// The command that makes the disk write what it holds in memory; 0
    /// if it has none.
    flush: u8,
}

/// Prepares a controller found at a function of the PCI bus and adds its
/// disks to `disks`.
pub fn start(function: pci::Function, disks: &mut Vec<Sata>) {
    function.enable(0x06);
    let base = (function.read(0x24) & !0xfff) as usize;
    if base == 0 {
        return;
    }
    unsafe {
        // a firmware that still uses the controller is asked to let go
        if get(base, CAPABILITIES_2) & CAN_HAND_OFF != 0 {
            set(base, HANDOFF, get(base, HANDOFF) | HANDOFF_SYSTEM);
            wait_for(200, || get(base, HANDOFF) & HANDOFF_FIRMWARE == 0);
        }
        set(base, CONTROL, get(base, CONTROL) | CONTROL_AHCI);
        set(base, CONTROL, get(base, CONTROL) & !CONTROL_INTERRUPTS);
        let wide = get(base, CAPABILITIES) & CAN_64_BITS != 0;
        let ports = get(base, PORTS);
        for number in 0..32 {
            if ports & 1 << number != 0 {
                if let Some(disk) = Sata::start(base + 0x100 + number * 0x80, wide) {
                    disks.push(disk);
                }
            }
        }
    }
}

impl Sata {
    unsafe fn start(port: usize, wide: bool) -> Option<Sata> {
        // a device is there and the link to it is up
        if get(port, LINK) & 0x0f != 3 {
            return None;
        }
        if matches!(get(port, SIGNATURE), SIGNATURE_PACKET | SIGNATURE_MULTIPLIER | SIGNATURE_ENCLOSURE) {
            return None;
        }
        // the port must be at rest while it is given its memory
        set(port, COMMAND, get(port, COMMAND) & !COMMAND_START);
        if !wait_for(STOP_PATIENCE, || get(port, COMMAND) & COMMAND_RUNNING == 0) {
            return None;
        }
        set(port, COMMAND, get(port, COMMAND) & !COMMAND_RECEIVE);
        if !wait_for(STOP_PATIENCE, || get(port, COMMAND) & COMMAND_RECEIVING == 0) {
            return None;
        }
        let memory = memory::pages(2)?;
        let buffer = memory + PAGE;
        if !wide && (buffer + PAGE) as u64 > 1 << 32 {
            // this controller only reaches the first four gigabytes
            memory::free_pages(memory, 2);
            return None;
        }
        write_bytes(memory as *mut u8, 0, 2 * PAGE);
        set(port, COMMAND_LIST, memory as u32);
        set(port, COMMAND_LIST + 4, (memory as u64 >> 32) as u32);
        set(port, RECEIVED, (memory + RECEIVED_AT) as u32);
        set(port, RECEIVED + 4, ((memory + RECEIVED_AT) as u64 >> 32) as u32);
        set(port, LINK_ERRORS, !0);
        set(port, INTERRUPTS, !0);
        set(port, INTERRUPTS_WANTED, 0);
        set(port, COMMAND, get(port, COMMAND) | COMMAND_POWER | COMMAND_SPIN_UP | COMMAND_RECEIVE);
        wait_for(STOP_PATIENCE, || get(port, TASK_FILE) & TASK_FILE_NOT_READY == 0);
        set(port, COMMAND, get(port, COMMAND) | COMMAND_START);

        let mut disk = Sata { port, memory, buffer, sectors: 0, model: FixedStr::new(), flush: 0 };
        if disk.identify().is_none() {
            set(port, COMMAND, get(port, COMMAND) & !COMMAND_START);
            wait_for(STOP_PATIENCE, || get(port, COMMAND) & COMMAND_RUNNING == 0);
            set(port, COMMAND, get(port, COMMAND) & !COMMAND_RECEIVE);
            wait_for(STOP_PATIENCE, || get(port, COMMAND) & COMMAND_RECEIVING == 0);
            memory::free_pages(memory, 2);
            return None;
        }
        Some(disk)
    }

    /// Asks the disk what it is. None if it is not a disk this driver can use.
    unsafe fn identify(&mut self) -> Option<()> {
        self.issue(IDENTIFY, 0, 0, SECTOR, false).ok()?;
        let data = core::slice::from_raw_parts(self.buffer as *const u8, SECTOR);
        let word = |n: usize| u16::from_le_bytes([data[2 * n], data[2 * n + 1]]) as u64;
        // a disk, with addresses of 48 bits
        if word(0) & 0x8000 != 0 || word(83) & 1 << 10 == 0 {
            return None;
        }
        // sectors of 512 bytes
        let sizes = word(106);
        if sizes & 0xc000 == 0x4000 && sizes & 1 << 12 != 0 && (word(117) | word(118) << 16) * 2 != SECTOR as u64 {
            return None;
        }
        self.sectors = word(100) | word(101) << 16 | word(102) << 32 | word(103) << 48;
        self.flush = match word(86) {
            can if can & 1 << 13 != 0 => FLUSH_48,
            can if can & 1 << 12 != 0 => FLUSH,
            _ => 0,
        };
        // the name: 40 characters, two to a word, the first in the high byte
        for n in 27..47 {
            for byte in [(word(n) >> 8) as u8, word(n) as u8] {
                self.model.push(if (0x20..0x7f).contains(&byte) { byte } else { b' ' });
            }
        }
        while self.model.bytes().last() == Some(&b' ') {
            self.model.remove(self.model.len() - 1);
        }
        (self.sectors > 0).then_some(())
    }

    /// Runs one command with `bytes` bytes of data in the buffer.
    unsafe fn issue(&mut self, command: u8, sector: u64, count: u16, bytes: usize, write: bool) -> Result<(), DiskError> {
        let port = self.port;
        let table = self.memory + TABLE_AT;
        write_bytes(table as *mut u8, 0, BUFFERS_AT + 16);
        // the command, as the controller sends it to the disk
        let s = sector.to_le_bytes();
        let c = count.to_le_bytes();
        let frame: [u8; 16] = [0x27, 0x80, command, 0, s[0], s[1], s[2], 0x40, s[3], s[4], s[5], 0, c[0], c[1], 0, 0];
        copy_nonoverlapping(frame.as_ptr(), table as *mut u8, frame.len());
        if bytes > 0 {
            let buffers = (table + BUFFERS_AT) as *mut u32;
            buffers.write_volatile(self.buffer as u32);
            buffers.add(1).write_volatile((self.buffer as u64 >> 32) as u32);
            buffers.add(3).write_volatile(bytes as u32 - 1);
        }
        // entry 0 of the list: a frame of five words, one buffer or none
        let entry = self.memory as *mut u32;
        entry.write_volatile(5 | (write as u32) << 6 | ((bytes > 0) as u32) << 16);
        entry.add(1).write_volatile(0);
        entry.add(2).write_volatile(table as u32);
        entry.add(3).write_volatile((table as u64 >> 32) as u32);

        set(port, INTERRUPTS, !0);
        let ready = wait_for(COMMAND_PATIENCE, || get(port, TASK_FILE) & TASK_FILE_NOT_READY == 0);
        if ready {
            set(port, ISSUE, 1);
        }
        let done = ready && wait_for(COMMAND_PATIENCE, || get(port, ISSUE) & 1 == 0 || get(port, INTERRUPTS) & FAILED != 0);
        if !done || get(port, INTERRUPTS) & FAILED != 0 || get(port, TASK_FILE) & TASK_FILE_ERROR != 0 {
            // stop and start the port, so that the next command finds it clean
            set(port, COMMAND, get(port, COMMAND) & !COMMAND_START);
            wait_for(STOP_PATIENCE, || get(port, COMMAND) & COMMAND_RUNNING == 0);
            set(port, LINK_ERRORS, !0);
            set(port, INTERRUPTS, !0);
            set(port, COMMAND, get(port, COMMAND) | COMMAND_START);
            return Err(DiskError);
        }
        Ok(())
    }

    fn inside(&self, sector: u64, bytes: usize) -> Result<u16, DiskError> {
        let count = (bytes / SECTOR) as u64;
        let fits = bytes > 0 && bytes <= PAGE && bytes % SECTOR == 0 && sector.checked_add(count).is_some_and(|end| end <= self.sectors);
        if fits {
            Ok(count as u16)
        } else {
            Err(DiskError)
        }
    }

    /// Reads whole sectors, a page at most.
    pub fn read(&mut self, sector: u64, into: &mut [u8]) -> Result<(), DiskError> {
        let count = self.inside(sector, into.len())?;
        unsafe {
            self.issue(READ, sector, count, into.len(), false)?;
            copy_nonoverlapping(self.buffer as *const u8, into.as_mut_ptr(), into.len());
        }
        Ok(())
    }

    /// Writes whole sectors, a page at most.
    pub fn write(&mut self, sector: u64, from: &[u8]) -> Result<(), DiskError> {
        let count = self.inside(sector, from.len())?;
        unsafe {
            copy_nonoverlapping(from.as_ptr(), self.buffer as *mut u8, from.len());
            self.issue(WRITE, sector, count, from.len(), true)
        }
    }

    /// Returns once the disk has written everything it was given.
    pub fn flush(&mut self) -> Result<(), DiskError> {
        if self.flush == 0 {
            return Ok(());
        }
        unsafe { self.issue(self.flush, 0, 0, 0, false) }
    }
}
