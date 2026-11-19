//! The few parts of UEFI the kernel needs, written against the specification.
//!
//! The kernel is started by the firmware as a UEFI application. It asks for a
//! graphics mode and the memory map, then leaves the boot services: from that
//! point the firmware is out of the way and the machine belongs to the kernel.

use core::ffi::c_void;
use core::ptr;

use xila_core::text::FixedStr;

pub type Handle = *mut c_void;
type Status = usize;

const ERROR: usize = 1 << (usize::BITS - 1);

#[repr(C)]
struct TableHeader {
    signature: u64,
    revision: u32,
    header_size: u32,
    crc32: u32,
    reserved: u32,
}

#[repr(C)]
pub struct SystemTable {
    header: TableHeader,
    firmware_vendor: *const u16,
    firmware_revision: u32,
    console_in_handle: Handle,
    console_in: *mut c_void,
    console_out_handle: Handle,
    console_out: *mut c_void,
    standard_error_handle: Handle,
    standard_error: *mut c_void,
    runtime_services: *mut c_void,
    boot_services: *mut BootServices,
    number_of_table_entries: usize,
    configuration_table: *mut c_void,
}

/// EFI_BOOT_SERVICES. Only five of its functions are used; the others are
/// kept as padding so that the used ones sit at their place in the table.
#[repr(C)]
struct BootServices {
    header: TableHeader,
    _0_to_3: [usize; 4],
    get_memory_map: extern "efiapi" fn(*mut usize, *mut u8, *mut usize, *mut usize, *mut u32) -> Status,
    allocate_pool: extern "efiapi" fn(u32, usize, *mut *mut u8) -> Status,
    _6_to_15: [usize; 10],
    handle_protocol: extern "efiapi" fn(Handle, *const Guid, *mut *mut c_void) -> Status,
    _17_to_25: [usize; 9],
    exit_boot_services: extern "efiapi" fn(Handle, usize) -> Status,
    _27_to_36: [usize; 10],
    locate_protocol: extern "efiapi" fn(*const Guid, *mut c_void, *mut *mut c_void) -> Status,
}

#[repr(C)]
struct Guid(u32, u16, u16, [u8; 8]);

const GRAPHICS_OUTPUT: Guid = Guid(0x9042a9de, 0x23dc, 0x4a38, [0x96, 0xfb, 0x7a, 0xde, 0xd0, 0x80, 0x51, 0x6a]);

#[repr(C)]
struct GraphicsOutput {
    query_mode: extern "efiapi" fn(*mut GraphicsOutput, u32, *mut usize, *mut *const ModeInfo) -> Status,
    set_mode: extern "efiapi" fn(*mut GraphicsOutput, u32) -> Status,
    blt: usize,
    mode: *const Mode,
}

#[repr(C)]
struct Mode {
    max_mode: u32,
    mode: u32,
    info: *const ModeInfo,
    size_of_info: usize,
    frame_buffer_base: u64,
    frame_buffer_size: usize,
}

#[repr(C)]
struct ModeInfo {
    version: u32,
    width: u32,
    height: u32,
    pixel_format: u32,
    pixel_information: [u32; 4],
    pixels_per_scan_line: u32,
}

const LOADED_IMAGE: Guid = Guid(0x5b1b31a1, 0x9562, 0x11d2, [0x8e, 0x3f, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b]);
const FILE_SYSTEM: Guid = Guid(0x964e5b22, 0x6459, 0x11d2, [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b]);

/// EFI_LOADED_IMAGE_PROTOCOL, as far as it is read: what the firmware
/// started, from which device and which file of it.
#[repr(C)]
struct LoadedImage {
    revision: u32,
    parent: Handle,
    system_table: *mut c_void,
    device: Handle,
    /// A list of pieces, each with its kind, its size, and what it says.
    file_path: *const u8,
}

#[repr(C)]
struct FileSystem {
    revision: u64,
    open_volume: extern "efiapi" fn(*mut FileSystem, *mut *mut File) -> Status,
}

/// EFI_FILE_PROTOCOL: a file or a directory the firmware has open.
#[repr(C)]
struct File {
    revision: u64,
    open: extern "efiapi" fn(*mut File, *mut *mut File, *const u16, u64, u64) -> Status,
    close: extern "efiapi" fn(*mut File) -> Status,
    delete: usize,
    read: extern "efiapi" fn(*mut File, *mut usize, *mut u8) -> Status,
    write: usize,
    get_position: extern "efiapi" fn(*mut File, *mut u64) -> Status,
    set_position: extern "efiapi" fn(*mut File, u64) -> Status,
}

/// The kind of memory that stays with whoever the firmware started.
const LOADER_DATA: u32 = 2;
/// A system file larger than this is not kept.
const MOST_SYSTEM: u64 = 32 << 20;

#[repr(C)]
struct MemoryDescriptor {
    kind: u32,
    physical_start: u64,
    virtual_start: u64,
    number_of_pages: u64,
    attribute: u64,
}

/// The screen the firmware set up: a block of memory with one 32 bit value per pixel.
#[derive(Clone, Copy)]
pub struct Framebuffer {
    pub base: *mut u32,
    pub width: usize,
    pub height: usize,
    /// Pixels per row in memory; may be more than `width`.
    pub stride: usize,
    /// Byte order of a pixel: blue first (the usual one), or red first.
    pub blue_first: bool,
}

/// Most regions of free memory the kernel takes from the firmware.
pub const MAX_FREE: usize = 48;

pub struct BootInfo {
    pub screens: Screens,
    /// Free memory above the first megabyte: first page and number of pages.
    pub free: [(usize, usize); MAX_FREE],
    pub free_count: usize,
    pub firmware: FixedStr<32>,
    /// The file the firmware started, which is the kernel: kept to put it
    /// on a disk (`install`). If it could not be read, why.
    pub system: Result<&'static [u8], &'static str>,
    pub usable_kb: u64,
    pub firmware_kb: u64,
    pub regions: u32,
}

/// Most sizes of screen kept from what the firmware offers.
pub const MAX_SIZES: usize = 40;

/// What the firmware says of the screen.
pub struct Screens {
    pub framebuffer: Option<Framebuffer>,
    /// The sizes the firmware offers, in pixels, each one once.
    pub sizes: [(usize, usize); MAX_SIZES],
    pub count: usize,
    /// The size of the panel itself, when it tells.
    pub panel: Option<(usize, usize)>,
}

const EDID_ACTIVE: Guid = Guid(0xbd8c1056, 0x9f36, 0x44ec, [0x92, 0xa8, 0xa6, 0x33, 0x7f, 0x81, 0x79, 0x86]);

/// EFI_EDID_ACTIVE_PROTOCOL: what the screen in use says of itself.
#[repr(C)]
struct Edid {
    size: u32,
    data: *const u8,
}

const MAP_SIZE: usize = 64 * 1024;
static mut MAP: [u8; MAP_SIZE] = [0; MAP_SIZE];

/// Prepares the machine and leaves the boot services.
///
/// # Safety
/// Must be called once, with the arguments the firmware gave to the entry point.
pub unsafe fn boot(image: Handle, table: *mut SystemTable, log: &mut dyn FnMut(&str)) -> BootInfo {
    let table = &*table;
    let services = &*table.boot_services;

    let mut firmware = FixedStr::new();
    let mut p = table.firmware_vendor;
    while !p.is_null() && *p != 0 {
        let c = *p;
        firmware.push(if c < 0x80 { c as u8 } else { b'?' });
        p = p.add(1);
    }

    let screens = graphics(services, log);
    let system = own_file(image, services);

    // The memory map must be the latest one when leaving the boot services,
    // and asking for it can itself change it: try twice.
    let map = ptr::addr_of_mut!(MAP) as *mut u8;
    let (mut size, mut key, mut entry_size, mut version) = (0usize, 0usize, 0usize, 0u32);
    for attempt in 0..2 {
        size = MAP_SIZE;
        let status = (services.get_memory_map)(&mut size, map, &mut key, &mut entry_size, &mut version);
        if status & ERROR != 0 {
            log("uefi: the memory map does not fit");
            size = 0;
            break;
        }
        if (services.exit_boot_services)(image, key) & ERROR == 0 {
            break;
        }
        if attempt == 1 {
            log("uefi: could not leave the boot services");
        }
    }

    let mut info =
        BootInfo { screens, free: [(0, 0); MAX_FREE], free_count: 0, firmware, system, usable_kb: 0, firmware_kb: 0, regions: 0 };
    let mut offset = 0;
    while entry_size != 0 && offset + entry_size <= size {
        let entry = ptr::read_unaligned(map.add(offset) as *const MemoryDescriptor);
        let kb = entry.number_of_pages * 4;
        match entry.kind {
            // code and data of the loader and of the boot services, and free memory
            1..=4 | 7 => info.usable_kb += kb,
            // runtime services and ACPI tables
            5 | 6 | 9 | 10 => info.firmware_kb += kb,
            // reserved ranges and device registers are not memory to count
            _ => {}
        }
        // Only memory that was free all along goes to the kernel for now. The
        // memory of the boot services is free too, but the kernel still runs
        // on a stack that the firmware put there.
        let first_page = (entry.physical_start / 4096) as usize;
        if entry.kind == 7 && first_page >= 256 && info.free_count < MAX_FREE {
            info.free[info.free_count] = (first_page, entry.number_of_pages as usize);
            info.free_count += 1;
        }
        info.regions += 1;
        offset += entry_size;
    }
    info
}

/// Reads the file the firmware started the kernel from, into memory that
/// stays the kernel's. The kernel has no driver for what it may have been
/// started from, a USB stick for one: only the firmware can read it, and
/// only now.
unsafe fn own_file(image: Handle, services: &BootServices) -> Result<&'static [u8], &'static str> {
    let mut found: *mut c_void = ptr::null_mut();
    if (services.handle_protocol)(image, &LOADED_IMAGE, &mut found) & ERROR != 0 || found.is_null() {
        return Err("the firmware does not say what it started");
    }
    let loaded = &*(found as *const LoadedImage);

    // The name of the file: the pieces of the path that are names, joined.
    let mut name = [0u16; 128];
    let mut length = 0;
    let mut piece = loaded.file_path;
    while !piece.is_null() {
        let (kind, sub, size) = (*piece, *piece.add(1), u16::from_le_bytes([*piece.add(2), *piece.add(3)]) as usize);
        // the piece that ends the list, or one that says no size
        if kind == 0x7f || size < 4 {
            break;
        }
        if kind == 4 && sub == 4 {
            if length > 0 && name[length - 1] != b'\\' as u16 && length < name.len() - 1 {
                name[length] = b'\\' as u16;
                length += 1;
            }
            for at in (4..size - 1).step_by(2) {
                let character = u16::from_le_bytes([*piece.add(at), *piece.add(at + 1)]);
                if character == 0 {
                    break;
                }
                if length == name.len() - 1 {
                    return Err("the name of its file is too long");
                }
                name[length] = character;
                length += 1;
            }
        }
        piece = piece.add(size);
    }
    if length == 0 {
        // not said: then it is where a firmware looks by itself
        for (to, from) in name.iter_mut().zip(b"\\EFI\\BOOT\\BOOTX64.EFI") {
            *to = *from as u16;
        }
    }

    let mut found: *mut c_void = ptr::null_mut();
    if loaded.device.is_null() || (services.handle_protocol)(loaded.device, &FILE_SYSTEM, &mut found) & ERROR != 0 || found.is_null() {
        return Err("the firmware cannot read files where it started from");
    }
    let volume = found as *mut FileSystem;
    let mut root: *mut File = ptr::null_mut();
    if ((*volume).open_volume)(volume, &mut root) & ERROR != 0 || root.is_null() {
        return Err("the firmware cannot read files where it started from");
    }
    let mut file: *mut File = ptr::null_mut();
    // 1: to read
    let opened = ((*root).open)(root, &mut file, name.as_ptr(), 1, 0) & ERROR == 0 && !file.is_null();
    let mut result = Err("the firmware does not find its file again");
    if opened {
        result = read_whole(file, services);
        ((*file).close)(file);
    }
    ((*root).close)(root);
    result
}

/// Reads a whole open file into memory asked from the firmware.
unsafe fn read_whole(file: *mut File, services: &BootServices) -> Result<&'static [u8], &'static str> {
    // its size: where its end is
    let mut size = 0u64;
    if ((*file).set_position)(file, u64::MAX) & ERROR != 0
        || ((*file).get_position)(file, &mut size) & ERROR != 0
        || ((*file).set_position)(file, 0) & ERROR != 0
    {
        return Err("the firmware does not say how large its file is");
    }
    if size < 2 || size > MOST_SYSTEM {
        return Err("its file has a size that cannot be right");
    }
    let mut memory: *mut u8 = ptr::null_mut();
    if (services.allocate_pool)(LOADER_DATA, size as usize, &mut memory) & ERROR != 0 || memory.is_null() {
        return Err("the firmware has no memory for a copy of its file");
    }
    let mut read = size as usize;
    if ((*file).read)(file, &mut read, memory) & ERROR != 0 || read != size as usize {
        return Err("its file could not be read whole");
    }
    let bytes = core::slice::from_raw_parts(memory, read);
    // what a firmware starts begins with these two letters
    if &bytes[..2] != b"MZ" {
        return Err("its file is not a system a firmware starts");
    }
    Ok(bytes)
}

/// The size of the panel, as the screen says it: the first of the timings
/// it describes is the one it was made for.
unsafe fn panel(services: &BootServices) -> Option<(usize, usize)> {
    let mut interface: *mut c_void = ptr::null_mut();
    if (services.locate_protocol)(&EDID_ACTIVE, ptr::null_mut(), &mut interface) & ERROR != 0 || interface.is_null() {
        return None;
    }
    let edid = &*(interface as *const Edid);
    if edid.size < 128 || edid.data.is_null() {
        return None;
    }
    let data = core::slice::from_raw_parts(edid.data, 128);
    let timing = &data[54..72];
    // what every such description starts with, and a timing rather than a text
    if data[..8] != [0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0] || (timing[0], timing[1]) == (0, 0) {
        return None;
    }
    let width = timing[2] as usize | (timing[4] as usize & 0xf0) << 4;
    let height = timing[5] as usize | (timing[7] as usize & 0xf0) << 4;
    (width > 0 && height > 0).then_some((width, height))
}

/// Picks the graphics mode and describes the framebuffer.
unsafe fn graphics(services: &BootServices, log: &mut dyn FnMut(&str)) -> Screens {
    let mut screens = Screens { framebuffer: None, sizes: [(0, 0); MAX_SIZES], count: 0, panel: panel(services) };
    let mut interface: *mut c_void = ptr::null_mut();
    if (services.locate_protocol)(&GRAPHICS_OUTPUT, ptr::null_mut(), &mut interface) & ERROR != 0 || interface.is_null() {
        log("uefi: no graphics output");
        return screens;
    }
    let gop = interface as *mut GraphicsOutput;
    // The sizes with a pixel the kernel can write, and the mode of each.
    let mut numbers = [0u32; MAX_SIZES];
    for number in 0..(*(*gop).mode).max_mode {
        let mut size = 0usize;
        let mut info: *const ModeInfo = ptr::null();
        if ((*gop).query_mode)(gop, number, &mut size, &mut info) & ERROR != 0 || info.is_null() {
            continue;
        }
        let pixels = ((*info).width as usize, (*info).height as usize);
        if (*info).pixel_format <= 1 && screens.count < MAX_SIZES && !screens.sizes[..screens.count].contains(&pixels) {
            screens.sizes[screens.count] = pixels;
            numbers[screens.count] = number;
            screens.count += 1;
        }
    }
    if let Some(best) = xila_core::grid::choose(&screens.sizes[..screens.count], screens.panel) {
        if numbers[best] != (*(*gop).mode).mode {
            ((*gop).set_mode)(gop, numbers[best]);
        }
    }
    // whatever was asked, the mode the firmware is in now is the one to describe
    let mode = &*(*gop).mode;
    let info = &*mode.info;
    if info.pixel_format > 1 {
        log("uefi: the pixel format of the screen is not supported");
        return screens;
    }
    screens.framebuffer = Some(Framebuffer {
        base: mode.frame_buffer_base as *mut u32,
        width: info.width as usize,
        height: info.height as usize,
        stride: info.pixels_per_scan_line as usize,
        blue_first: info.pixel_format == 1,
    });
    screens
}
