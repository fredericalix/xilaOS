//! The PCI bus: finding the devices of the machine.
//!
//! Every device answers at a bus, a device and a function number with a
//! small table that says what it is and where its registers are. The
//! table is read through two ports that every PC has.

use crate::arch::{inl, outl};

const ADDRESS: u16 = 0xcf8;
const DATA: u16 = 0xcfc;

/// One function of one device.
#[derive(Clone, Copy)]
pub struct Function {
    bus: u8,
    device: u8,
    function: u8,
}

impl Function {
    fn address(&self, offset: u8) -> u32 {
        0x8000_0000 | (self.bus as u32) << 16 | (self.device as u32) << 11 | (self.function as u32) << 8 | (offset as u32 & 0xfc)
    }

    /// Where the function is: bus, device, function.
    pub fn place(&self) -> (u8, u8, u8) {
        (self.bus, self.device, self.function)
    }

    pub fn read(&self, offset: u8) -> u32 {
        unsafe {
            outl(ADDRESS, self.address(offset));
            inl(DATA)
        }
    }

    pub fn write(&self, offset: u8, value: u32) {
        unsafe {
            outl(ADDRESS, self.address(offset));
            outl(DATA, value);
        }
    }

    /// Lets the device answer on its ports (1), on its memory registers
    /// (2), and read and write the memory of the machine (4).
    pub fn enable(&self, what: u32) {
        self.write(0x04, self.read(0x04) | what);
    }
}

/// Calls `each` with every function there is: the function, its maker and
/// model (`model << 16 | maker`), and its class (`class << 16 | subclass
/// << 8 | interface`).
pub fn scan(each: &mut dyn FnMut(Function, u32, u32)) {
    for bus in 0..=255u8 {
        for device in 0..32u8 {
            for function in 0..8u8 {
                let found = Function { bus, device, function };
                let id = found.read(0);
                if id as u16 == 0xffff {
                    if function == 0 {
                        // no device here at all
                        break;
                    }
                    continue;
                }
                each(found, id, found.read(0x08) >> 8);
                // most devices have a single function
                if function == 0 && found.read(0x0c) & 0x0080_0000 == 0 {
                    break;
                }
            }
        }
    }
}
