//! The first serial port (COM1), polled.

use crate::arch::{inb, outb};

const BASE: u16 = 0x3f8;

#[derive(Clone, Copy)]
pub struct Serial {
    present: bool,
}

impl Serial {
    /// A port that is not there: writes are dropped, nothing is ever read.
    pub const fn absent() -> Serial {
        Serial { present: false }
    }

    /// Sets the port to 115200 baud, 8 bits, no parity, and checks that it
    /// exists: a byte written while the chip is looped back on itself
    /// must come back.
    ///
    /// This runs first of all, while the firmware still does: its console
    /// reads the same port at every tick of its timer, and would take the
    /// byte of the test for a key typed there. So nothing of the firmware
    /// may run between the writing of the byte and its reading: interrupts
    /// are off for the test. Without that, about one start in 250 in QEMU
    /// found no port, and the system ran without its serial shell.
    pub fn init() -> Serial {
        let were_on = crate::arch::interrupts_save();
        let present = unsafe {
            outb(BASE + 1, 0x00); // no interrupts
            outb(BASE + 3, 0x80); // divisor access
            outb(BASE, 0x01); // divisor 1: 115200 baud
            outb(BASE + 1, 0x00);
            outb(BASE + 3, 0x03); // 8 bits, no parity, one stop bit
            outb(BASE + 2, 0xc7); // queues on, emptied
            outb(BASE + 4, 0x1e); // loop back, to test the chip
            outb(BASE, 0xae);
            let present = inb(BASE) == 0xae;
            outb(BASE + 4, 0x0f); // normal operation
            present
        };
        crate::arch::interrupts_restore(were_on);
        Serial { present }
    }

    pub fn present(&self) -> bool {
        self.present
    }

    pub fn write_byte(&self, byte: u8) {
        if !self.present {
            return;
        }
        unsafe {
            for _ in 0..100_000 {
                if inb(BASE + 5) & 0x20 != 0 {
                    break;
                }
            }
            outb(BASE, byte);
        }
    }

    /// Writes text; a line feed is sent as carriage return and line feed, and
    /// characters outside ASCII as `?`.
    pub fn write(&self, text: &[u8]) {
        for &b in text {
            match b {
                b'\n' => {
                    self.write_byte(b'\r');
                    self.write_byte(b'\n');
                }
                0x20..=0x7e | b'\t' | b'\r' | 0x08 => self.write_byte(b),
                _ => self.write_byte(b'?'),
            }
        }
    }

    pub fn read(&self) -> Option<u8> {
        if !self.present {
            return None;
        }
        unsafe {
            if inb(BASE + 5) & 0x01 != 0 {
                Some(inb(BASE))
            } else {
                None
            }
        }
    }
}

impl core::fmt::Write for Serial {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.write(s.as_bytes());
        Ok(())
    }
}
