//! The disk of the database: a virtio block device, as QEMU provides one.
//!
//! This is the old ("legacy") virtio interface: the device is found on the
//! PCI bus, it is driven through input/output ports, and requests go
//! through one queue in memory that both sides read and write. A request
//! is a chain of three buffers: what to do and where, the data, and one
//! byte for the device to say how it went.
//!
//! One request runs at a time and the driver waits for it by looking at
//! the queue: no interrupt is used. The caller holds the lock of the
//! shared state, so no two tasks are here at once.
//!
//! This is the disk of development: the database takes the whole of it.

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_bytes, write_volatile};
use core::sync::atomic::{fence, Ordering};

use xila_db::{Disk, DiskError, Page, PAGE};

use crate::arch::{inb, inl, inw, outb, outl, outw};
use crate::disk::wait_for;
use crate::memory;
use crate::pci;

/// What such a device says it is: the model, then the maker. A block
/// device that still speaks the old interface.
pub const ID: u32 = 0x1001_1af4;

// Ports of the device, from the base its first address register gives.
const FEATURES: u16 = 0x00;
const DRIVER_FEATURES: u16 = 0x04;
const QUEUE_ADDRESS: u16 = 0x08;
const QUEUE_SIZE: u16 = 0x0c;
const QUEUE_SELECT: u16 = 0x0e;
const QUEUE_NOTIFY: u16 = 0x10;
const STATUS: u16 = 0x12;
const INTERRUPT_STATUS: u16 = 0x13;
/// The number of 512 byte sectors, 64 bits.
const CAPACITY: u16 = 0x14;

const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FAILED: u8 = 0x80;

/// The device can be asked to put what it holds in memory on the disk.
const FEATURE_FLUSH: u32 = 1 << 9;

const REQUEST_READ: u32 = 0;
const REQUEST_WRITE: u32 = 1;
const REQUEST_FLUSH: u32 = 4;

/// The buffer is followed by another one.
const NEXT: u16 = 1;
/// The device writes in the buffer.
const DEVICE_WRITES: u16 = 2;
/// In the flags of the driver's ring: no interrupt is wanted.
const NO_INTERRUPT: u16 = 1;

const SECTORS_PER_PAGE: u64 = (PAGE / 512) as u64;
/// How long a request may take, in ticks: half a minute.
const PATIENCE: u64 = 3000;

#[repr(C)]
struct Descriptor {
    address: u64,
    length: u32,
    flags: u16,
    next: u16,
}

#[repr(C)]
struct Request {
    kind: u32,
    reserved: u32,
    sector: u64,
}

pub struct VirtioBlk {
    base: u16,
    /// The memory of the queue: the descriptors, then the ring of the
    /// driver, then on the next page boundary the ring of the device.
    queue: usize,
    size: u16,
    /// How many requests were given to the device so far.
    given: u16,
    sectors: u64,
    can_flush: bool,
    /// A request was given up: the device may still write in its buffers,
    /// so it is not used any more.
    lost: bool,
}

impl VirtioBlk {
    /// Prepares the device found at a function of the PCI bus.
    pub fn start(function: pci::Function) -> Option<VirtioBlk> {
        let bar = function.read(0x10);
        if bar & 1 == 0 {
            // not ports: this driver does not know that form
            return None;
        }
        let base = (bar & 0xfffc) as u16;
        // answer on its ports, and let it read and write memory
        function.enable(0x05);

        unsafe {
            outb(base + STATUS, 0);
            outb(base + STATUS, STATUS_ACKNOWLEDGE);
            outb(base + STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);
            let can_flush = inl(base + FEATURES) & FEATURE_FLUSH != 0;
            outl(base + DRIVER_FEATURES, if can_flush { FEATURE_FLUSH } else { 0 });

            outw(base + QUEUE_SELECT, 0);
            let size = inw(base + QUEUE_SIZE);
            if size < 4 || !size.is_power_of_two() {
                outb(base + STATUS, STATUS_FAILED);
                return None;
            }
            let (_, bytes) = layout(size);
            let pages = bytes.div_ceil(PAGE);
            let Some(queue) = memory::pages(pages) else {
                outb(base + STATUS, STATUS_FAILED);
                return None;
            };
            write_bytes(queue as *mut u8, 0, pages * PAGE);
            let disk = VirtioBlk {
                base,
                queue,
                size,
                given: 0,
                sectors: inl(base + CAPACITY) as u64 | (inl(base + CAPACITY + 4) as u64) << 32,
                can_flush,
                lost: false,
            };
            write_volatile(disk.driver_ring(), NO_INTERRUPT);
            outl(base + QUEUE_ADDRESS, (queue / PAGE) as u32);
            outb(base + STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_DRIVER_OK);
            Some(disk)
        }
    }

    fn descriptor(&self, index: u16) -> *mut Descriptor {
        (self.queue + index as usize * 16) as *mut Descriptor
    }

    /// The ring of the driver: flags, how many requests were given, then
    /// the first descriptor of each.
    fn driver_ring(&self) -> *mut u16 {
        (self.queue + self.size as usize * 16) as *mut u16
    }

    /// The ring of the device: flags, then how many requests are done.
    fn device_ring(&self) -> *const u16 {
        (self.queue + layout(self.size).0) as *const u16
    }

    /// Gives the device one request and waits until it is done.
    fn request(&mut self, kind: u32, sector: u64, data: Option<(*mut u8, bool)>) -> Result<(), DiskError> {
        if self.lost {
            return Err(DiskError);
        }
        let header = Request { kind, reserved: 0, sector };
        let mut status = 0xffu8;
        let last = if data.is_some() { 2 } else { 1 };
        unsafe {
            self.descriptor(0).write_volatile(Descriptor { address: addr_of!(header) as u64, length: 16, flags: NEXT, next: 1 });
            if let Some((bytes, device_writes)) = data {
                let flags = NEXT | if device_writes { DEVICE_WRITES } else { 0 };
                self.descriptor(1).write_volatile(Descriptor { address: bytes as u64, length: PAGE as u32, flags, next: 2 });
            }
            self.descriptor(last).write_volatile(Descriptor { address: addr_of_mut!(status) as u64, length: 1, flags: DEVICE_WRITES, next: 0 });

            let ring = self.driver_ring();
            write_volatile(ring.add(2 + (self.given % self.size) as usize), 0);
            fence(Ordering::SeqCst);
            self.given = self.given.wrapping_add(1);
            write_volatile(ring.add(1), self.given);
            fence(Ordering::SeqCst);
            outw(self.base + QUEUE_NOTIFY, 0);

            let (ring, given, base) = (self.device_ring(), self.given, self.base);
            let done = wait_for(PATIENCE, || {
                // reading this port also takes back the interrupt request
                inb(base + INTERRUPT_STATUS);
                read_volatile(ring.add(1)) == given
            });
            if !done {
                self.lost = true;
                outb(self.base + STATUS, 0);
                return Err(DiskError);
            }
            fence(Ordering::SeqCst);
            if read_volatile(addr_of!(status)) == 0 {
                Ok(())
            } else {
                Err(DiskError)
            }
        }
    }
}

/// Where the ring of the device starts in the memory of a queue, and how
/// many bytes the queue takes.
fn layout(size: u16) -> (usize, usize) {
    let size = size as usize;
    let device_ring = (size * 16 + 6 + 2 * size).next_multiple_of(PAGE);
    (device_ring, device_ring + 6 + 8 * size)
}

impl Disk for VirtioBlk {
    fn name(&self) -> &'static str {
        "virtio"
    }

    fn pages(&self) -> u64 {
        self.sectors / SECTORS_PER_PAGE
    }

    fn read(&mut self, page: u64, into: &mut Page) -> Result<(), DiskError> {
        self.request(REQUEST_READ, page * SECTORS_PER_PAGE, Some((into.as_mut_ptr(), true)))
    }

    fn write(&mut self, page: u64, from: &Page) -> Result<(), DiskError> {
        self.request(REQUEST_WRITE, page * SECTORS_PER_PAGE, Some((from.as_ptr() as *mut u8, false)))
    }

    fn flush(&mut self) -> Result<(), DiskError> {
        if !self.can_flush {
            // the device keeps nothing in memory
            return Ok(());
        }
        self.request(REQUEST_FLUSH, 0, None)
    }
}
