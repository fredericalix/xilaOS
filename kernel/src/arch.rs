//! The processor and the old PC devices: ports, descriptor tables, interrupts,
//! timer, keyboard controller.
//!
//! The timer interrupt counts a tick and switches to another task (see
//! `task.rs`); a task gives up the processor through the same path with a
//! software interrupt. Both save every register and the floating point state
//! on the stack of the task. The other hardware interrupts only acknowledge:
//! the devices are polled by tasks.

use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicU64, Ordering};

use xila_core::text::FixedStr;

/// Timer ticks since start, 100 per second. Incremented by the timer interrupt.
pub static TICKS: AtomicU64 = AtomicU64::new(0);

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// # Safety
/// Writing to a port can do anything to the machine.
pub unsafe fn outb(port: u16, value: u8) {
    asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
}

/// # Safety
/// Reading a port can have side effects on the device.
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags));
    value
}

/// # Safety
/// As `outb`, for 16 bits.
pub unsafe fn outw(port: u16, value: u16) {
    asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags));
}

/// # Safety
/// As `inb`, for 16 bits.
pub unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    asm!("in ax, dx", in("dx") port, out("ax") value, options(nomem, nostack, preserves_flags));
    value
}

/// # Safety
/// As `outb`, for 32 bits.
pub unsafe fn outl(port: u16, value: u32) {
    asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags));
}

/// # Safety
/// As `inb`, for 32 bits.
pub unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    asm!("in eax, dx", in("dx") port, out("eax") value, options(nomem, nostack, preserves_flags));
    value
}

/// A page of zeros for a table of the memory map.
unsafe fn map_table() -> Option<*mut u64> {
    let table = crate::memory::pages(1)? as *mut u64;
    core::ptr::write_bytes(table, 0, 512);
    Some(table)
}

/// Replaces the firmware's map of the memory by one of the kernel: every
/// address is itself, as before, but now for certain. What the firmware
/// maps outside of the memory it reported is not defined, and the registers
/// of devices are there.
///
/// The memory of the screen, `screen.0..=screen.1`, is marked write
/// combining when the processor can: see `combine`.
///
/// Returns how many gigabytes are mapped and whether with pages of 1 GB
/// (otherwise 2 MB). Returns `None`, and changes nothing, when the map
/// would not reach the end of the screen or cannot be built.
///
/// # Safety
/// Must be called once, after `memory::init`, before other tasks run.
pub unsafe fn map_memory(screen: (u64, u64)) -> Option<(u64, bool)> {
    use core::arch::x86_64::__cpuid;
    let cr4: u64;
    asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags));
    if cr4 & 1 << 12 != 0 {
        // five levels of tables: not a form this code builds
        return None;
    }
    let extended = __cpuid(0x8000_0000).eax;
    let width = if extended >= 0x8000_0008 { __cpuid(0x8000_0008).eax & 0xff } else { 36 };
    let big = extended >= 0x8000_0001 && __cpuid(0x8000_0001).edx & 1 << 26 != 0;
    // as much as the processor can address, within what one table of the
    // second level holds; small pages cost a table per gigabyte
    let gigabytes = (1u64 << (width.clamp(32, 48) - 30)).min(if big { 512 } else { 64 });
    if screen.1 >= gigabytes << 30 {
        return None;
    }
    let top = map_table()?;
    let second = map_table()?;
    for gigabyte in 0..gigabytes {
        // present, writable, and for the large ones: a page, not a table
        let entry = if big { gigabyte << 30 | 0x83 } else { split(gigabyte << 30, 30)? as u64 | 0x03 };
        second.add(gigabyte as usize).write(entry);
    }
    top.write(second as u64 | 0x03);
    // Without the table of memory kinds there is no write combining, and
    // without room for the smaller tables the screen stays as it was: slower,
    // but right.
    if __cpuid(1).edx & 1 << 16 != 0 {
        // kind 1 of the table: write combining, in place of a kind nothing uses
        wrmsr(KINDS, rdmsr(KINDS) & !0xff00 | (COMBINING as u64) << 8);
        let _ = combine(second, screen.0 & !0xfff, (screen.1 | 0xfff) + 1);
    }
    asm!("mov cr3, {}", in(reg) top as u64, options(nostack, preserves_flags));
    if cr4 & 1 << 7 != 0 {
        // the firmware's map may have entries that outlive a change of
        // tables; turning that feature off and on forgets them
        asm!("mov cr4, {}", in(reg) cr4 & !(1 << 7), options(nostack, preserves_flags));
        asm!("mov cr4, {}", in(reg) cr4, options(nostack, preserves_flags));
    }
    MAPPED.store(gigabytes << 30, Ordering::Relaxed);
    Some((gigabytes, big))
}

/// The register that holds the eight kinds of memory a page can choose from.
const KINDS: u32 = 0x277;
/// The kind "write combining": the processor gathers the writes and sends
/// them 64 bytes at a time, without reading first. Made for screens.
const COMBINING: u8 = 1;
/// In an entry of a table: the page is of kind 1.
const KIND_1: u64 = 1 << 3;
/// The address in an entry of a table.
const ADDRESS: u64 = 0x000f_ffff_ffff_f000;

unsafe fn rdmsr(register: u32) -> u64 {
    let (low, high): (u32, u32);
    asm!("rdmsr", in("ecx") register, out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags));
    (high as u64) << 32 | low as u64
}

unsafe fn wrmsr(register: u32, value: u64) {
    asm!("wrmsr", in("ecx") register, in("eax") value as u32, in("edx") (value >> 32) as u32, options(nomem, nostack, preserves_flags));
}

/// A table that maps what one page of `1 << bits` bytes at `start` maps,
/// in 512 pages of the size below.
unsafe fn split(start: u64, bits: u32) -> Option<*mut u64> {
    let table = map_table()?;
    for i in 0..512 {
        // only the smallest pages have no "this is a page" mark
        table.add(i as usize).write(start | i << (bits - 9) | if bits == 21 { 0x03 } else { 0x83 });
    }
    Some(table)
}

/// Marks `from..to` (whole pages of 4 KB) write combining in a map that is
/// not in use yet. Only those pages: around a screen there can be the
/// registers of a device, which must stay as they are. So the large pages
/// at the two ends are cut into smaller ones.
unsafe fn combine(second: *mut u64, from: u64, to: u64) -> Option<()> {
    let mut at = from;
    while at < to {
        let entry = second.add((at >> 30) as usize);
        if *entry & 0x80 != 0 {
            *entry = split(at >> 30 << 30, 30)? as u64 | 0x03;
        }
        let third = (*entry & ADDRESS) as *mut u64;
        let (start, end) = (at >> 21 << 21, (at >> 21 << 21) + (1 << 21));
        let entry = third.add((at >> 21 & 511) as usize);
        if *entry & 0x80 != 0 && from <= start && end <= to {
            // a whole page of 2 MB
            *entry |= KIND_1;
        } else {
            // part of one: by pages of 4 KB, cut now or by an earlier call
            if *entry & 0x80 != 0 {
                *entry = split(start, 21)? as u64 | 0x03;
            }
            let fourth = (*entry & ADDRESS) as *mut u64;
            for i in 0..512u64 {
                if (from..to).contains(&(start + (i << 12))) {
                    *fourth.add(i as usize) |= KIND_1;
                }
            }
        }
        at = end;
    }
    Some(())
}

/// Whether the processor combines the writes to this address. It is read
/// from the tables in use, whoever built them.
/// Marks more memory as write combining, as the screen is: `from..to`,
/// which is cut to whole pages of 4 KB. False, and then some of it may be
/// marked and some not, if the kernel does not have its own map of the
/// memory or has no room for the tables.
pub fn combine_more(from: u64, to: u64) -> bool {
    use core::arch::x86_64::__cpuid;
    if mapped() == 0 || to > mapped() || __cpuid(1).edx & 1 << 16 == 0 {
        return false;
    }
    unsafe {
        let (top, cr4): (u64, u64);
        asm!("mov {}, cr3", out(reg) top, options(nomem, nostack, preserves_flags));
        asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags));
        // the map of the kernel has one table of the second level
        let second = (*((top & ADDRESS) as *const u64) & ADDRESS) as *mut u64;
        let done = combine(second, from & !0xfff, (to + 0xfff) & !0xfff).is_some();
        // the processor forgets what it remembered of the old tables
        asm!("mov cr3, {}", in(reg) top, options(nostack, preserves_flags));
        if cr4 & 1 << 7 != 0 {
            asm!("mov cr4, {}", in(reg) cr4 & !(1 << 7), options(nostack, preserves_flags));
            asm!("mov cr4, {}", in(reg) cr4, options(nostack, preserves_flags));
        }
        done
    }
}

pub fn combines(address: u64) -> bool {
    unsafe {
        let top: u64;
        asm!("mov {}, cr3", out(reg) top, options(nomem, nostack, preserves_flags));
        let mut table = (top & ADDRESS) as *const u64;
        for level in (0..4).rev() {
            let entry = *table.add((address >> (12 + 9 * level) & 511) as usize);
            if entry & 1 == 0 {
                return false;
            }
            if level == 0 || entry & 0x80 != 0 {
                // which of the eight kinds: three bits of the entry, one of
                // them elsewhere in a large page
                let high = if level == 0 { entry >> 7 & 1 } else { entry >> 12 & 1 };
                let kind = high << 2 | (entry >> 4 & 1) << 1 | (entry >> 3 & 1);
                return (rdmsr(KINDS) >> (8 * kind)) as u8 & 7 == COMBINING;
            }
            table = (entry & ADDRESS) as *const u64;
        }
        false
    }
}

/// What the clock of the machine holds: the one that runs on a battery
/// while the machine is off. None if it does not hold still long enough
/// to be read.
pub fn clock_read() -> Option<xila_core::clock::Raw> {
    use xila_core::clock::Raw;
    // The clock is read through two ports: the number of a register, then
    // its value.
    let register = |number: u8| unsafe {
        outb(0x70, number);
        inb(0x71)
    };
    let all = || {
        // not while the clock moves to the next second
        for _ in 0..100_000 {
            if register(0x0a) & 0x80 == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        Raw { second: register(0), minute: register(2), hour: register(4), day: register(7), month: register(8), year: register(9), form: register(0x0b) }
    };
    // twice the same: no register changed between two others
    let mut last = all();
    for _ in 0..5 {
        let again = all();
        if again == last {
            return Some(last);
        }
        last = again;
    }
    None
}

/// How much memory the kernel's own map covers, from address 0; 0 while
/// the map of the firmware is in use.
static MAPPED: AtomicU64 = AtomicU64::new(0);

/// Bytes from address 0 that the kernel can reach for sure: memory, and
/// the registers of devices. 0 if the kernel could not make its own map.
pub fn mapped() -> u64 {
    MAPPED.load(Ordering::Relaxed)
}

/// How far the processor's own counter advances in a millisecond; 0 until
/// it has been measured.
static COUNTER_PER_MS: AtomicU64 = AtomicU64::new(0);

pub fn counter() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// A number that is hard to guess from outside the machine: the
/// processor's counter, mixed with a number of its own source of chance
/// when it has one.
pub fn chance() -> u64 {
    use core::arch::x86_64::{__cpuid, _rdrand64_step};
    let mut drawn = 0;
    // bit 30 of what the processor says of itself: it has that source
    if __cpuid(1).ecx & 1 << 30 != 0 {
        unsafe { _rdrand64_step(&mut drawn) };
    }
    counter() ^ drawn
}

/// Fills `bytes` from the processor's own source of chance (RDRAND), the
/// only one here that is good enough to draw keys from. False if the
/// processor has none, or if it gave nothing: then nothing was drawn
/// that may be used.
pub fn random(bytes: &mut [u8]) -> bool {
    use core::arch::x86_64::{__cpuid, _rdrand64_step};
    // bit 30 of what the processor says of itself: it has that source
    if __cpuid(1).ecx & 1 << 30 == 0 {
        return false;
    }
    for part in bytes.chunks_mut(8) {
        let mut drawn = 0;
        let mut tries = 0;
        // the source may have nothing ready: it says so, and is asked again
        while unsafe { _rdrand64_step(&mut drawn) } != 1 {
            tries += 1;
            if tries == 32 {
                return false;
            }
        }
        part.copy_from_slice(&drawn.to_le_bytes()[..part.len()]);
    }
    true
}

/// Measures the processor's counter against the timer, for `micros`. Takes
/// 50 to 60 ms. Interrupts must be on.
pub fn clock_init() {
    let edge = ticks();
    while ticks() == edge {
        wait();
    }
    let (from, start) = (ticks(), counter());
    while ticks() < from + 5 {
        wait();
    }
    COUNTER_PER_MS.store((counter() - start) / 50, Ordering::Relaxed);
}

/// Microseconds since some moment before the kernel started. Much finer
/// than the timer: it is there to measure how long things take.
pub fn micros() -> u64 {
    match COUNTER_PER_MS.load(Ordering::Relaxed) {
        0 => ticks() * 10_000,
        per_ms => (counter() as u128 * 1000 / per_ms as u128) as u64,
    }
}

/// Sleeps until the next interrupt.
pub fn wait() {
    unsafe { asm!("hlt", options(nomem, nostack)) };
}

pub fn interrupts_on() {
    unsafe { asm!("sti", options(nomem, nostack)) };
}

pub fn interrupts_off() {
    unsafe { asm!("cli", options(nomem, nostack)) };
}

/// Stops the processor for good.
pub fn stop() -> ! {
    loop {
        interrupts_off();
        wait();
    }
}

/// The name the processor gives itself.
pub fn cpu_name() -> FixedStr<48> {
    use core::arch::x86_64::__cpuid;
    let mut name = FixedStr::<48>::new();
    #[allow(unused_unsafe)]
    unsafe {
        if __cpuid(0x8000_0000).eax >= 0x8000_0004 {
            for leaf in 0x8000_0002u32..=0x8000_0004 {
                let r = __cpuid(leaf);
                for word in [r.eax, r.ebx, r.ecx, r.edx] {
                    for byte in word.to_le_bytes() {
                        if byte != 0 {
                            name.push(byte);
                        }
                    }
                }
            }
        }
    }
    let trimmed = FixedStr::<48>::from_bytes(xila_core::text::trim(name.bytes()));
    if trimmed.is_empty() {
        FixedStr::from_bytes(b"unknown")
    } else {
        trimmed
    }
}

#[repr(C, packed)]
struct TablePointer {
    limit: u16,
    base: u64,
}

/// Segments: nothing, 64 bit code, data.
static GDT: [u64; 3] = [0, 0x00af_9a00_0000_ffff, 0x00cf_9200_0000_ffff];
const CODE: u16 = 0x08;

#[repr(C)]
#[derive(Clone, Copy)]
struct Gate {
    offset_low: u16,
    selector: u16,
    stack: u8,
    kind: u8,
    offset_middle: u16,
    offset_high: u32,
    reserved: u32,
}

impl Gate {
    const EMPTY: Gate = Gate { offset_low: 0, selector: 0, stack: 0, kind: 0, offset_middle: 0, offset_high: 0, reserved: 0 };

    /// An interrupt gate: interrupts stay off while the handler runs.
    fn to(handler: u64) -> Gate {
        Gate {
            offset_low: handler as u16,
            selector: CODE,
            stack: 0,
            kind: 0x8e,
            offset_middle: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            reserved: 0,
        }
    }
}

static mut IDT: [Gate; 256] = [Gate::EMPTY; 256];

extern "C" {
    static exc_table: [u64; 32];
    fn irq_timer();
    fn irq_yield();
    fn irq_first();
    fn irq_second();
    fn irq_none();
}

/// Interrupt numbers of the hardware interrupts 0-7 and 8-15.
const IRQ_BASE: usize = 32;
/// The interrupt a task raises to give the processor to another task.
const YIELD: usize = 0x30;

/// Lets another task run. Returns when this task is chosen again.
pub fn yield_now() {
    unsafe { asm!("int 0x30") };
}

/// Turns interrupts off and tells whether they were on.
pub fn interrupts_save() -> bool {
    let flags: u64;
    unsafe { asm!("pushfq", "pop {}", "cli", out(reg) flags) };
    flags & 0x200 != 0
}

/// Undoes `interrupts_save`.
pub fn interrupts_restore(were_on: bool) {
    if were_on {
        interrupts_on();
    }
}

/// Takes over the processor tables and the interrupt hardware. Interrupts are
/// left off; call `interrupts_on` when the rest is ready.
///
/// # Safety
/// Must be called once, after the boot services were left.
pub unsafe fn init() {
    interrupts_off();

    // Our own segments: the ones of the firmware live in memory that is free now.
    let gdt = TablePointer { limit: (core::mem::size_of_val(&GDT) - 1) as u16, base: GDT.as_ptr() as u64 };
    asm!(
        "lgdt [{gdt}]",
        "push 0x08",
        "lea rax, [rip + 2f]",
        "push rax",
        "retfq",
        "2:",
        "mov eax, 0x10",
        "mov ds, eax",
        "mov es, eax",
        "mov ss, eax",
        "mov fs, eax",
        "mov gs, eax",
        gdt = in(reg) &gdt,
        out("rax") _,
    );

    let idt = &mut *core::ptr::addr_of_mut!(IDT);
    for (i, gate) in idt.iter_mut().enumerate() {
        let handler = match i {
            0..=31 => exc_table[i],
            32 => irq_timer as *const () as u64,
            33..=39 => irq_first as *const () as u64,
            40..=47 => irq_second as *const () as u64,
            YIELD => irq_yield as *const () as u64,
            _ => irq_none as *const () as u64,
        };
        *gate = Gate::to(handler);
    }
    let idt = TablePointer { limit: (core::mem::size_of::<[Gate; 256]>() - 1) as u16, base: core::ptr::addr_of!(IDT) as u64 };
    asm!("lidt [{0}]", in(reg) &idt, options(readonly, nostack));

    // The two interrupt controllers: move their interrupts to 32-47, away from
    // the numbers the processor uses for its own exceptions, and let only the
    // timer (0) and the keyboard (1) through.
    outb(0x20, 0x11);
    outb(0xa0, 0x11);
    outb(0x21, IRQ_BASE as u8);
    outb(0xa1, IRQ_BASE as u8 + 8);
    outb(0x21, 0x04);
    outb(0xa1, 0x02);
    outb(0x21, 0x01);
    outb(0xa1, 0x01);
    outb(0x21, 0xfc);
    outb(0xa1, 0xff);

    // The interval timer: 1193182 Hz divided by 11932 gives 100 ticks per second.
    outb(0x43, 0x36);
    outb(0x40, (11932 & 0xff) as u8);
    outb(0x40, (11932 >> 8) as u8);

    keyboard_init();
}

/// Waits until the keyboard controller accepts a byte.
unsafe fn keyboard_ready() {
    for _ in 0..100_000 {
        if inb(0x64) & 0x02 == 0 {
            break;
        }
    }
}

/// Makes the keyboard controller send scancodes of set 1 and raise interrupt 1.
unsafe fn keyboard_init() {
    while inb(0x64) & 0x01 != 0 {
        inb(0x60);
    }
    keyboard_ready();
    outb(0x64, 0x20); // read the configuration
    let mut config = 0x45;
    for _ in 0..100_000 {
        if inb(0x64) & 0x01 != 0 {
            config = inb(0x60);
            break;
        }
    }
    config |= 0x41; // keyboard interrupt on, translation to set 1 on
    config &= !0x10; // keyboard not disabled
    keyboard_ready();
    outb(0x64, 0x60); // write the configuration
    keyboard_ready();
    outb(0x60, config);
    keyboard_ready();
    outb(0x64, 0xae); // enable the keyboard port
}

/// The next scancode, if the keyboard sent one.
pub fn keyboard_read() -> Option<u8> {
    unsafe {
        loop {
            let status = inb(0x64);
            if status & 0x01 == 0 {
                return None;
            }
            let byte = inb(0x60);
            if status & 0x20 == 0 {
                return Some(byte);
            }
            // a byte from the pointing device: not used
        }
    }
}

/// Restarts the machine through the keyboard controller.
pub fn reboot() -> ! {
    unsafe {
        interrupts_off();
        keyboard_ready();
        outb(0x64, 0xfe);
    }
    stop()
}

/// Called by the exception stubs. `vector` is the number of the exception.
extern "C" fn exception(vector: u64, code: u64, address: u64) -> ! {
    crate::fatal(format_args!("processor exception {vector}, code {code:#x}, at {address:#x}"))
}

// Entry points of the interrupts. An exception pushes its number (and a zero
// if the processor pushed no error code) and ends in `exception`. The timer
// and the yield interrupt save the task, ask `schedule` for the stack of the
// next one and restore from there. The other hardware interrupts acknowledge
// and return.
global_asm!(
    r#"
    .text
    .global exc_table
    .global irq_timer
    .global irq_yield
    .global irq_first
    .global irq_second
    .global irq_none
exc_0:
    push 0
    push 0
    jmp exc_common
exc_1:
    push 0
    push 1
    jmp exc_common
exc_2:
    push 0
    push 2
    jmp exc_common
exc_3:
    push 0
    push 3
    jmp exc_common
exc_4:
    push 0
    push 4
    jmp exc_common
exc_5:
    push 0
    push 5
    jmp exc_common
exc_6:
    push 0
    push 6
    jmp exc_common
exc_7:
    push 0
    push 7
    jmp exc_common
exc_8:
    push 8
    jmp exc_common
exc_9:
    push 0
    push 9
    jmp exc_common
exc_10:
    push 10
    jmp exc_common
exc_11:
    push 11
    jmp exc_common
exc_12:
    push 12
    jmp exc_common
exc_13:
    push 13
    jmp exc_common
exc_14:
    push 14
    jmp exc_common
exc_15:
    push 0
    push 15
    jmp exc_common
exc_16:
    push 0
    push 16
    jmp exc_common
exc_17:
    push 17
    jmp exc_common
exc_18:
    push 0
    push 18
    jmp exc_common
exc_19:
    push 0
    push 19
    jmp exc_common
exc_20:
    push 0
    push 20
    jmp exc_common
exc_21:
    push 21
    jmp exc_common
exc_22:
    push 0
    push 22
    jmp exc_common
exc_23:
    push 0
    push 23
    jmp exc_common
exc_24:
    push 0
    push 24
    jmp exc_common
exc_25:
    push 0
    push 25
    jmp exc_common
exc_26:
    push 0
    push 26
    jmp exc_common
exc_27:
    push 0
    push 27
    jmp exc_common
exc_28:
    push 0
    push 28
    jmp exc_common
exc_29:
    push 29
    jmp exc_common
exc_30:
    push 30
    jmp exc_common
exc_31:
    push 0
    push 31
    jmp exc_common
exc_common:
    mov rcx, [rsp]
    mov rdx, [rsp + 8]
    mov r8, [rsp + 16]
    and rsp, -16
    sub rsp, 32
    call {exception}
exc_stop:
    hlt
    jmp exc_stop

irq_timer:
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    sub rsp, 512
    fxsave64 [rsp]
    lock inc qword ptr [rip + {ticks}]
    mov al, 0x20
    out 0x20, al
    mov rcx, rsp
    mov edx, 1
    jmp switch_task

irq_yield:
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    sub rsp, 512
    fxsave64 [rsp]
    mov rcx, rsp
    xor edx, edx
switch_task:
    sub rsp, 32
    call {schedule}
    mov rsp, rax
    fxrstor64 [rsp]
    add rsp, 512
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax
    iretq

irq_first:
    push rax
    mov al, 0x20
    out 0x20, al
    pop rax
    iretq

irq_second:
    push rax
    mov al, 0x20
    out 0xa0, al
    out 0x20, al
    pop rax
    iretq

irq_none:
    iretq

    .p2align 3
exc_table:
    .quad exc_0
    .quad exc_1
    .quad exc_2
    .quad exc_3
    .quad exc_4
    .quad exc_5
    .quad exc_6
    .quad exc_7
    .quad exc_8
    .quad exc_9
    .quad exc_10
    .quad exc_11
    .quad exc_12
    .quad exc_13
    .quad exc_14
    .quad exc_15
    .quad exc_16
    .quad exc_17
    .quad exc_18
    .quad exc_19
    .quad exc_20
    .quad exc_21
    .quad exc_22
    .quad exc_23
    .quad exc_24
    .quad exc_25
    .quad exc_26
    .quad exc_27
    .quad exc_28
    .quad exc_29
    .quad exc_30
    .quad exc_31
"#,
    exception = sym exception,
    ticks = sym TICKS,
    schedule = sym crate::task::schedule,
);
