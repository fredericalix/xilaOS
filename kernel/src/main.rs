//! The XilaOS kernel.
//!
//! It starts from the firmware, takes the machine, and runs tasks: one for
//! the interface on the screen, one for the shell on the serial port, and any
//! that commands start. The state they share sits behind one lock.

#![no_std]
#![no_main]

extern crate alloc;

mod ahci;
mod arch;
mod chip;
mod disk;
mod e1000;
mod fb;
mod memory;
mod pci;
mod serial;
mod task;
mod uefi;
mod virtio;

use alloc::boxed::Box;
use core::cell::UnsafeCell;
use core::fmt::Write;
use core::sync::atomic::{AtomicUsize, Ordering};

use xila_core::city::{self, City};
use xila_core::events;
use xila_core::keys::Decoder;
use xila_core::grid::Grid;
use xila_core::ask;
use xila_core::dns;
use xila_core::remote;
use xila_core::tftp;
use xila_core::shell::{self, Measure, Out, Pictures, Request, Style, System, Table, FRAME_TICKS, PROMPT};
use xila_core::text::FixedStr;
use xila_core::theme::{role, THEMES};
use xila_core::ui::{Rhythm, Ui, BODY_TOP, CITY_ROWS};

use serial::Serial;

/// What the tasks share: the interface, the state the commands work on, and
/// what draws on the screen.
struct Shared {
    ui: Ui,
    sys: System,
    /// There once the interface task has set the screen up.
    stage: Option<Stage>,
    /// The SATA disks, and the kernel as the firmware read it from where
    /// it started: what `install` puts on one of them.
    disks: disk::Disks,
    system: &'static [u8],
    /// The network card, if there is one XilaOS drives.
    nic: Option<e1000::Nic>,
    /// What the last command from the network said: kept, to be sent
    /// again if the network lost it.
    answer: xila_core::remote::Answer,
    /// The page being given to a browser.
    page: xila_core::web::Page,
    /// The call to the companion's model that is going on, and the room
    /// for what is sent and what comes back, made at the first question.
    call: Option<Thinking>,
    room: Option<(Box<[u8]>, Box<[u8]>)>,
}

/// A question to the companion's model on its way: the asking, the TLS
/// it goes through if it does, and how long what is sent is.
struct Thinking {
    ask: ask::Ask,
    tls: Option<Box<xila_tls::Client>>,
    length: usize,
}

/// What draws on the screen.
struct Stage {
    screen: fb::Screen,
    city: City,
    /// The picture of the city, drawn here before the screen enlarges it.
    picture: alloc::vec::Vec<u32>,
    /// The picture of a code (`qr`), when one is shown.
    code: alloc::vec::Vec<u32>,
}

/// A virtual machine as it is before anything runs in it: what each room
/// of the jobs is made from. One of them is in the file of the kernel,
/// not nine: a machine is 270 KB, and is not made of zeros.
static NEW_ROOM: xila_core::machine::Machine = xila_core::machine::Machine::new();

/// The rooms of the jobs: a virtual machine each, eight for the jobs
/// people start and one for the programs that run by themselves, in the
/// memory of the kernel. Asked for once, by the interface task as it
/// sets up; they are never given back.
fn rooms() -> &'static mut [xila_core::machine::Machine] {
    use xila_core::machine::Machine;
    let count = xila_core::jobs::PLACES + 1;
    let Ok(layout) = core::alloc::Layout::array::<Machine>(count) else {
        fatal(format_args!("the rooms of the jobs have no size"));
    };
    // Safety: the memory is as large as `count` machines and aligned for
    // them; each is written whole before the slice is made, from a machine
    // that owns nothing outside itself, so a copy of its bytes is one too.
    unsafe {
        let memory = alloc::alloc::alloc(layout) as *mut Machine;
        if memory.is_null() {
            fatal(format_args!("no memory for the rooms of the jobs"));
        }
        for at in 0..count {
            core::ptr::copy_nonoverlapping(&NEW_ROOM, memory.add(at), 1);
        }
        core::slice::from_raw_parts_mut(memory, count)
    }
}

/// How long jobs run before the system is given back, in microseconds:
/// those nobody waits for, then the one a prompt waits for, which has
/// most of a tick since that prompt does nothing else meanwhile.
const SLICE: u64 = 3_000;
const SLICE_WAITED: u64 = 8_000;

static SHARED: task::Mutex<Shared> =
    task::Mutex::new(Shared { ui: Ui::new(), sys: System::new(), stage: None, disks: disk::Disks::new(), system: &[], nic: None, answer: xila_core::remote::Answer::new(), page: xila_core::web::Page::new(), call: None, room: None });

/// A value written once while the kernel starts, before any other task
/// exists, and only read afterwards.
struct Once<T>(UnsafeCell<T>);
unsafe impl<T> Sync for Once<T> {}

static SERIAL: Once<Serial> = Once(UnsafeCell::new(Serial::absent()));

fn serial() -> Serial {
    unsafe { *SERIAL.0.get() }
}

/// Writes command output to the serial port, in a form a program can read:
/// plain lines, and tables as tab separated rows between `#table` and `#end`.
struct SerialOut<'a> {
    serial: &'a Serial,
}

impl Out for SerialOut<'_> {
    fn line(&mut self, _style: Style, text: &[u8]) {
        self.serial.write(text);
        self.serial.write(b"\n");
    }

    fn table(&mut self, t: &Table) {
        self.serial.write(b"#table ");
        self.serial.write(t.name.bytes());
        for col in 0..t.cols {
            self.serial.write(b"\t");
            self.serial.write(t.header[col].bytes());
        }
        self.serial.write(b"\n");
        for row in 0..t.rows {
            for col in 0..t.cols {
                if col > 0 {
                    self.serial.write(b"\t");
                }
                self.serial.write(t.cells[row][col].bytes());
            }
            self.serial.write(b"\n");
        }
        self.serial.write(b"#end\n");
    }
}

#[no_mangle]
pub extern "efiapi" fn efi_main(image: uefi::Handle, table: *mut uefi::SystemTable) -> usize {
    let serial = Serial::init();
    unsafe { *SERIAL.0.get() = serial };
    serial.write(b"\nXilaOS ");
    serial.write(xila_core::VERSION);
    serial.write(b" starting\n");
    let boot = unsafe {
        uefi::boot(image, table, &mut |text| {
            serial.write(text.as_bytes());
            serial.write(b"\n");
        })
    };
    memory::init(&boot.free[..boot.free_count]);
    unsafe {
        arch::init();
        task::init();
    }
    // What was learnt from the firmware stays here, on the stack of this
    // flow, which never ends: it becomes the idle task.
    if task::spawn(b"interface", interface, &boot as *const uefi::BootInfo as usize, true).is_none() {
        fatal(format_args!("no memory for the interface task"));
    }
    task::idle()
}

/// The task of the screen: sets it up, then reads the keyboard and redraws.
extern "C" fn interface(boot: usize) -> ! {
    let boot = unsafe { &*(boot as *const uefi::BootInfo) };
    let serial = serial();
    let Some(framebuffer) = boot.screens.framebuffer else {
        fatal(format_args!("no usable screen"));
    };
    // The timer, which the disk drivers count on, and the clock that
    // measures against it.
    arch::interrupts_on();
    arch::clock_init();
    let mut screen = fb::Screen::new(framebuffer);
    let time_whole_screen = |screen: &mut fb::Screen| {
        screen.show_all();
        let start = arch::micros();
        screen.show();
        (arch::micros() - start) as u32
    };
    // A whole screen, black, with the memory as the firmware left it...
    let firmware_us = time_whole_screen(&mut screen);
    // ...then the kernel's own map of the memory, wide enough for the screen
    // and for the registers of the devices, and with the screen marked as a
    // screen: the same picture again tells what that gains on this machine.
    let first = framebuffer.base as u64;
    let last = first + (framebuffer.stride * framebuffer.height * 4) as u64 - 1;
    let mapped = unsafe { arch::map_memory((first, last)) };
    let whole_us = time_whole_screen(&mut screen);
    // What the tables in use say, whoever built them: how many pages around
    // the screen gather their writes. It should be the screen and no more.
    let combining = arch::combines(first) && arch::combines(last);
    let around = (first.saturating_sub(2 << 20) >> 12)..=((last + (2 << 20)) >> 12);
    let pages = around.filter(|page| arch::combines(page << 12)).count();
    let mut theme;
    {
        let mut shared = SHARED.lock();
        let Shared { ui, sys, disks, system, nic, .. } = &mut *shared;
        ui.grid.rows = screen.rows;

        sys.rooms = Some(rooms());
        sys.machine.cpu = arch::cpu_name();
        sys.machine.firmware = boot.firmware;
        sys.machine.screen_width = screen.width() as u32;
        sys.machine.screen_height = screen.height() as u32;
        sys.machine.scale = screen.scale as u32;
        sys.display.combining = combining;
        sys.display.whole_us = whole_us;
        sys.display.firmware_us = firmware_us;
        sys.machine.serial = serial.present();
        sys.machine.mem_usable_kb = boot.usable_kb;
        sys.machine.mem_firmware_kb = boot.firmware_kb;
        sys.machine.mem_regions = boot.regions;
        // without a disk the stored tables may take a quarter of the memory
        sys.db.set_limit(boot.usable_kb as usize * 1024 / 4);
        sys.card = chip::find(&framebuffer);
        let _ = match sys.card {
            None => write!(sys.machine.graphics, "none found"),
            Some(card) => match xila_core::intel::known(card.id) {
                Some((name, _)) => write!(sys.machine.graphics, "{name}"),
                None => write!(sys.machine.graphics, "{:04x}:{:04x}, not known to XilaOS", card.id & 0xffff, card.id >> 16),
            },
        };
        let found = disk::open(&mut sys.db, disks);
        sys.machine.disk = found.disk;
        sys.spare = found.spare;
        // what the machine is made of, for `devices`
        let mut slots = sys.devices.iter_mut();
        pci::scan(&mut |function, id, class| {
            let driven = class == ahci::CLASS
                || id == virtio::ID
                || (class >> 16 == 0x03 && xila_core::intel::known(id).is_some())
                || (class == e1000::CLASS && e1000::known(id).is_some());
            if class == e1000::CLASS && nic.is_none() {
                *nic = e1000::Nic::start(function, id);
            }
            if let Some(slot) = slots.next() {
                *slot = Some(shell::Device { place: function.place(), id, class, driven });
            }
        });
        if let Some(nic) = nic {
            sys.net.start(nic.mac, sys.ticks);
            sys.net_card = Some(card(nic));
        }
        // the time, by the clock that ran while the machine was off
        sys.clock = arch::clock_read().and_then(xila_core::clock::Moment::read).map(|moment| (moment.seconds(), sys.ticks));
        // what the user chose before this start: keyboard, colors, brightness...
        let kept = xila_core::settings::load(sys);
        theme = sys.theme;
        disk::look(disks, sys);
        *system = boot.system.unwrap_or(&[]);
        sys.system_bytes = system.len();

        // The boot log goes to its screen and to the serial port.
        let mut log = |text: core::fmt::Arguments| {
            let mut line = FixedStr::<80>::new();
            let _ = line.write_fmt(text);
            ui.log(line.bytes());
            serial.write(line.bytes());
            serial.write(b"\n");
        };
        log(format_args!("XilaOS {}", Ascii(xila_core::VERSION)));
        log(format_args!("Firmware ............ {}", Ascii(sys.machine.firmware.bytes())));
        log(format_args!("Processor ........... {}", Ascii(sys.machine.cpu.bytes())));
        log(format_args!("Memory .............. {} MB usable, {} regions", boot.usable_kb / 1024, boot.regions));
        let memory = memory::stats();
        let heap = if memory::self_test() { "heap tested" } else { "HEAP TEST FAILED" };
        log(format_args!("Kernel memory ....... {} MB managed, {}", memory.managed_kb / 1024, heap));
        log(format_args!(
            "Screen .............. {}x{}, {} rows of 80 characters",
            screen.width(),
            screen.height(),
            screen.rows
        ));
        // Every size the firmware offered, on as many lines as it takes.
        let mut sizes = FixedStr::<80>::from_bytes(b"Screen sizes ........");
        for (width, height) in &boot.screens.sizes[..boot.screens.count] {
            if sizes.len() > 68 {
                log(format_args!("{}", Ascii(sizes.bytes())));
                sizes = FixedStr::from_bytes(b"                     ");
            }
            let _ = write!(sizes, " {width}x{height}");
        }
        log(format_args!("{}", Ascii(sizes.bytes())));
        match boot.screens.panel {
            Some((width, height)) => log(format_args!("Panel ............... {width}x{height}")),
            None => log(format_args!("Panel ............... does not tell its size")),
        }
        log(format_args!("Graphics chip ....... {}", Ascii(sys.machine.graphics.bytes())));
        match mapped {
            Some((gigabytes, true)) => log(format_args!("Page tables ......... {gigabytes} GB mapped, pages of 1 GB")),
            Some((gigabytes, false)) => log(format_args!("Page tables ......... {gigabytes} GB mapped, pages of 2 MB")),
            None => log(format_args!("Page tables ......... those of the firmware")),
        }
        match combining {
            true => log(format_args!("Screen memory ....... write combining, {pages} pages of 4 KB")),
            false => log(format_args!("Screen memory ....... written piece by piece")),
        }
        log(format_args!(
            "Whole screen ........ shown in {}.{} ms; {}.{} ms as the firmware left the memory",
            (whole_us + 50) / 1000,
            (whole_us + 50) % 1000 / 100,
            (firmware_us + 50) / 1000,
            (firmware_us + 50) % 1000 / 100
        ));
        log(format_args!("Disk ................ {}", Ascii(found.log.bytes())));
        log(format_args!("Database ............ {}", Ascii(found.database.bytes())));
        match boot.system {
            Ok(file) => log(format_args!("System file ......... {} KB, kept in memory for install", file.len().div_ceil(1024))),
            Err(why) => log(format_args!("System file ......... not kept: {why}")),
        }
        match (&nic, sys.net.mac) {
            (Some(nic), Some(mac)) => log(format_args!("Network ............. {}, {}", nic.name, Ascii(xila_core::net::mac_text(mac).bytes()))),
            _ => log(format_args!("Network ............. no card XilaOS drives")),
        }
        match sys.now() {
            Some(now) => log(format_args!("Clock ............... {}", Ascii(xila_core::clock::Moment::at(now).text().bytes()))),
            None => log(format_args!("Clock ............... tells no time")),
        }
        // what keys are drawn from: without it nothing is asked through TLS
        match arch::random(&mut [0; 8]) {
            true => log(format_args!("Chance .............. the processor's own source")),
            false => log(format_args!("Chance .............. none good enough for keys: no https")),
        }
        if kept > 0 {
            log(format_args!("Settings ............ {kept} kept from before, in the table kept_settings"));
        }
        log(format_args!("Keyboard ............ layout {}", Ascii(sys.layout.name())));
        log(format_args!("Serial port ......... {}", if serial.present() { "development shell" } else { "none" }));
        log(format_args!("Tasks ............... switched 100 times per second"));
        screen.clear(THEMES[theme].color(role::PAGE, 0));
        let mut stage = Stage { screen, city: City::new(), picture: alloc::vec![0u32; city::WIDTH * city::HEIGHT], code: alloc::vec::Vec::new() };
        // A chip that can show two pictures in turn does, from the start,
        // unless the user chose otherwise; and gives the screen the
        // brightness the user set.
        if let Some(card) = sys.card.filter(|card| card.registers != 0) {
            match if sys.display.direct { Ok(Pictures::Direct) } else { flip(sys, &mut stage, Pictures::Two) } {
                Ok(Pictures::Direct) => log(format_args!("Pictures ............ written directly, as was asked")),
                Ok(_) => log(format_args!("Pictures ............ change between two sweeps of the screen")),
                Err(why) => log(format_args!("Pictures ............ written directly: {}", why.words())),
            }
            if let (Some(wish), Some((_, generation))) = (sys.display.light, xila_core::intel::known(card.id)) {
                if let Some(now) = chip::light(&card, generation, Some(wish)) {
                    log(format_args!("Brightness .......... {now} of 100, as was set"));
                }
            }
        }
        log(format_args!("System ready."));
        ui.welcome();
        shared.stage = Some(stage);
    }

    if serial.present() && task::spawn(b"serial shell", serial_shell, 0, true).is_none() {
        fatal(format_args!("no room for the serial shell task"));
    }
    if task::spawn(b"jobs", jobs, 0, true).is_none() {
        fatal(format_args!("no room for the task of the jobs"));
    }

    let mut decoder = Decoder::new();
    let mut seen = arch::ticks();
    // The screen and the picture of what moves by itself that are shown.
    let (mut shown, mut picture) = (usize::MAX, 0);
    // The rhythm of what moves by itself, when the screen gives it.
    let mut rhythm = Rhythm::new();
    // The minute, by the timer, in which the clock was last read.
    let mut minute = seen / 6000;
    // Whether a code is on the screen, whether it is painted, and which
    // one it is: a code can take the place of another.
    let (mut coded, mut painted, mut codes) = (false, false, 0);
    // Counted during the current second, for `display`.
    let (mut second, mut pictures, mut longest) = (0, 0, 0);
    loop {
        {
            let mut shared = SHARED.lock();
            let Shared { ui, sys, stage, disks, system, nic, answer, page, call, room } = &mut *shared;
            let Some(stage) = stage else {
                fatal(format_args!("the screen is gone"));
            };
            observe(sys);

            while let Some(code) = arch::keyboard_read() {
                // a dead key that found no letter gives two: its accent, then the key
                let mut next = decoder.feed(sys.layout, code);
                while let Some(event) = next {
                    observe(sys);
                    counted(sys, nic.as_mut());
                    ui.key(sys, event);
                    serve(sys, ui, Some(stage), disks, system, nic.as_mut());
                    ui.served(sys);
                    next = decoder.rest();
                }
            }

            let now = arch::ticks();
            // The screen gives the rhythm when a chip shows the pictures and
            // counts its sweeps; the timer does otherwise.
            let swept = rhythm.advance(stage.screen.sweeps(), now);
            sys.picture = swept.map(|_| rhythm.pictures());
            (sys.display.every, sys.display.sweeps) = if swept.is_some() { rhythm.pace() } else { (0, 0) };
            if now != seen {
                ui.tick(sys, now - seen);
                seen = now;
            }
            // The program the prompt of the screen waits for: some of it, then
            // the rest of this pass. What it asks of the kernel is served as
            // what a command typed here asks.
            let began = arch::micros();
            ui.work(sys, &mut || arch::micros().wrapping_sub(began) >= SLICE_WAITED);
            if xila_core::jobs::asks(sys) {
                serve(sys, ui, Some(stage), disks, system, nic.as_mut());
                ui.served(sys);
            }
            // The network: the frames that came, and the time that passes.
            if let Some(nic) = nic {
                if let Some(call) = network(sys, nic) {
                    // a command from another machine: run as one typed here would be
                    if let Some(line) = call.line {
                        answer.clear();
                        observe(sys);
                        counted(sys, Some(nic));
                        sys.session = events::NETWORK;
                        // `peek` tells what the screen shows now
                        if ui.dirty {
                            ui.render(sys);
                        }
                        let ok = shell::exec(sys, line.bytes(), answer);
                        answer.events(sys);
                        serve(sys, ui, Some(stage), disks, system, Some(nic));
                        // a request the kernel could not meet makes the command a failure
                        let ok = shell::served(sys, answer) && ok;
                        ui.dirty = true;
                        answer.end(sys.blocks[events::NETWORK].open(), ok);
                    }
                    let mut message = [0u8; remote::MESSAGE];
                    for index in 0..remote::parts(answer) {
                        let length = remote::part(answer, call.number, index, &mut message);
                        sys.net.message(call.from, call.port, remote::PORT, &message[..length], sys.ticks, nic);
                    }
                }
                // the pages for browsers, and the companion's model: a little
                // of what there is to do, then what the conversations have to say
                xila_core::web::step(sys, page);
                companion(sys, nic, call, room);
                sys.net.speak(sys.ticks, nic);
                if now / 100 != second {
                    nic.count();
                    sys.net_card = Some(card(nic));
                }
            }
            if now / 6000 != minute {
                // the timer is not a clock: once a minute, the real one is read again
                minute = now / 6000;
                if let Some(moment) = arch::clock_read().and_then(xila_core::clock::Moment::read) {
                    sys.clock = Some((moment.seconds(), sys.ticks));
                }
            }
            if sys.reboot {
                arch::reboot();
            }
            let started = arch::micros();
            // What moves by itself, the city and the cursor, advances one
            // picture at a time, at a fixed pace. What the user changes does
            // not wait for a picture: it is shown at the next tick.
            let due = sys.ticks / FRAME_TICKS;
            let behind = swept.unwrap_or(due - picture);
            picture = due;
            sys.display.skipped += behind.saturating_sub(1);
            let mut redraw_city = behind > 0;
            if sys.theme != theme {
                theme = sys.theme;
                stage.screen.clear(THEMES[theme].color(role::PAGE, 0));
                ui.grid.mark_all();
                ui.dirty = true;
                redraw_city = true;
                painted = false;
            }
            if sys.codes != codes {
                codes = sys.codes;
                painted = false;
            }
            if sys.code.is_some() != coded {
                // a code comes or goes: everything under it must be redrawn
                coded = sys.code.is_some();
                painted = false;
                ui.grid.mark_all();
                ui.dirty = true;
                redraw_city = true;
            }
            if sys.screen != shown {
                // another screen: everything under the picture must come back
                shown = sys.screen;
                ui.grid.mark_all();
                ui.dirty = true;
                redraw_city = true;
            }
            if ui.dirty {
                ui.render(sys);
            }
            let over = pictured(sys, ui.grid.rows);
            stage.screen.compose(&mut ui.grid, THEMES[theme], over);
            if let Some(code) = &sys.code {
                if !painted {
                    painted = true;
                    stage.code.clear();
                    stage.code.resize(city::WIDTH * xila_core::ui::code_points(ui.grid.rows), 0);
                    xila_core::qr::paint(code, &mut stage.code, city::WIDTH, THEMES[theme].color(role::TERM_BASE, 0));
                    stage.screen.blit(&stage.code, city::WIDTH, BODY_TOP);
                }
            } else if sys.screen == 0 && redraw_city {
                let from = arch::micros();
                // a late picture is skipped, but not the time it stands for
                for _ in 0..behind.min(50) {
                    stage.city.step(sys);
                }
                stage.city.draw(sys, THEMES[theme], &mut stage.picture);
                stage.screen.blit(&stage.picture, city::WIDTH, BODY_TOP);
                sys.display.city_us = (arch::micros() - from) as u32;
            }
            if stage.screen.show() > 0 {
                pictures += 1;
                longest = longest.max((arch::micros() - started) as u32);
            }
            if stage.screen.stuck() {
                // a chip that no longer changes pictures: do without it
                stage.screen.direct();
                sys.display.shown = Pictures::Direct;
                ui.log(b"Display: the chip stopped changing pictures; the screen is written directly.");
            }
            if now / 100 != second {
                second = now / 100;
                sys.display.pictures = pictures;
                sys.display.longest_us = longest;
                (pictures, longest) = (0, 0);
            }
        }
        task::sleep(1);
    }
}

/// Copies into the system state what commands may want to know about the kernel.
fn observe(sys: &mut System) {
    // what nobody else can guess: when exactly this runs, again and again
    sys.chance = sys.chance.rotate_left(7) ^ arch::chance();
    // the conversations of the network draw their first numbers from it too
    sys.net.tcp.stir(sys.chance);
    // and the questions about names their numbers
    sys.net.names.stir(sys.chance);
    task::snapshot(&mut sys.tasks);
    let memory = memory::stats();
    sys.machine.mem_managed_kb = memory.managed_kb;
    sys.machine.mem_free_kb = memory.free_kb;
    sys.machine.heap_used = memory.heap_used;
}

/// Brings what `net` shows of the card up to date: a command reads what
/// the card counted until now, not until the last second.
fn counted(sys: &mut System, nic: Option<&mut e1000::Nic>) {
    if let Some(nic) = nic {
        nic.count();
        sys.net_card = Some(card(nic));
    }
}

/// The rows of the screen a picture is shown over: a code, or the city.
fn pictured(sys: &System, rows: usize) -> (usize, usize) {
    match (&sys.code, sys.screen) {
        (Some(_), _) => (BODY_TOP, rows - 1),
        (None, 0) => (BODY_TOP, BODY_TOP + CITY_ROWS),
        _ => (0, 0),
    }
}

/// Does what a command asked of the kernel.
fn serve(sys: &mut System, ui: &mut Ui, mut stage: Option<&mut Stage>, disks: &mut disk::Disks, system: &mut &'static [u8], nic: Option<&mut e1000::Nic>) {
    match sys.request.take() {
        Some(Request::NetRegisters) => sys.net_registers = nic.map(|nic| nic.registers()),
        Some(Request::NetRestart) => {
            if let Some(nic) = nic {
                let back = nic.restart();
                sys.net.start(nic.mac, sys.ticks);
                sys.net_card = Some(card(nic));
                sys.net_restarted = Some(back);
            }
        }
        Some(Request::Ping(to)) => {
            if let Some(nic) = nic {
                // sent, then a second for the answer to come
                let start = arch::micros();
                sys.net.ping(to, sys.ticks, nic);
                let mut took = None;
                while took.is_none() && arch::micros() - start < 1_000_000 {
                    // a command from the network that comes meanwhile is not
                    // run: who sent it asks again
                    let _ = network(sys, nic);
                    took = sys.net.ping.and_then(|ping| ping.took).map(|_| (arch::micros() - start) as u32);
                    core::hint::spin_loop();
                }
                sys.pinged = Some((to, took));
            }
        }
        Some(Request::Light(wish)) => {
            let known = sys.card.filter(|card| card.registers != 0).and_then(|card| Some((card, xila_core::intel::known(card.id)?.1)));
            sys.lit = Some(known.and_then(|(card, generation)| chip::light(&card, generation, wish)));
            if let (Some(_), Some(Some(now))) = (wish, sys.lit) {
                // what the user set is kept for the next start
                sys.display.light = Some(now);
            }
        }
        Some(Request::Flip(wish)) => {
            if let Some(stage) = stage {
                sys.flipped = Some(flip(sys, stage, wish));
                // what the user chose is kept for the next start; a trial is not a choice
                match (wish, sys.flipped) {
                    (Pictures::Direct, _) => sys.display.direct = true,
                    (Pictures::Two, Some(Ok(_))) => sys.display.direct = false,
                    _ => {}
                }
            }
        }
        Some(Request::Chip { registers }) => {
            let known = sys.card.and_then(|card| Some((card, xila_core::intel::known(card.id)?.1)));
            if let Some((card, generation)) = known.filter(|(card, _)| card.registers != 0) {
                sys.chip = Some((chip::look(&card, generation), registers));
            }
        }
        Some(Request::Ask { to, port, secret, post }) => {
            let got = match nic {
                Some(nic) => get(sys, nic, to, port, secret, post),
                None => Err(ask::Failed::NoAnswer),
            };
            sys.got = Some((port, got));
        }
        Some(Request::Name) => {
            sys.named = Some(match nic {
                Some(nic) => named(sys, nic, arch::micros()),
                None => Err(dns::Failed::NoServer),
            });
        }
        Some(Request::Fetch(server)) => {
            let fetched = match nic {
                Some(nic) => fetch(sys, nic, server, system),
                None => Err(tftp::Failed::NoWay),
            };
            sys.fetched = Some((server, fetched));
        }
        Some(Request::Install { disk, whole }) => {
            // The machine does nothing else meanwhile, a second or two on a
            // real disk: a bar on the last row of the screen says how far it is.
            let (theme, screen, over) = (THEMES[sys.theme], sys.screen, pictured(sys, ui.grid.rows));
            let (mut filled, mut steps, mut sectors) = (usize::MAX, 0u32, 0);
            let mut progress = |done: u64, total: u64| {
                sectors = total / 2;
                let now = ui.progress(screen, done, total);
                if now != filled {
                    filled = now;
                    if let Some(stage) = stage.as_deref_mut() {
                        stage.screen.compose(&mut ui.grid, theme, over);
                        stage.screen.show();
                        steps += 1;
                    }
                }
            };
            let installed = disk::install(sys, disks, disk, whole, system, &mut progress);
            if installed.is_ok() {
                let mut line = FixedStr::<80>::new();
                let _ = write!(line, "Install: disk {disk}, {sectors} sectors written and read back, shown in {steps} steps.");
                ui.log(line.bytes());
            }
            sys.installed = Some(installed);
            disk::look(disks, sys);
            ui.dirty = true;
        }
        Some(Request::Measure) => {
            if let Some(stage) = stage {
                sys.measured = Some(measure(sys, &mut ui.grid, stage));
            }
        }
        Some(Request::Spin) => {
            task::spawn(b"spin", spin, 0, false);
        }
        Some(Request::Stop(id)) => {
            task::stop(id);
        }
        None => {}
    }
}

/// Does a little of the companion's asking: calls its model when a
/// question waits, carries the bytes of a call that is going on, and
/// gives what came back to `companion::heard`, which either ends the
/// question or wants the model called again. One pass at each tick: the
/// machine goes on living while the model thinks.
fn companion(sys: &mut System, nic: &mut e1000::Nic, call: &mut Option<Thinking>, room: &mut Option<(Box<[u8]>, Box<[u8]>)>) {
    use xila_core::companion::{self, Stage, Trouble};
    if sys.companion.stage != Stage::Calling {
        // turned off, or given up, while its model was being called: hang up
        if let Some(mut gone) = call.take() {
            gone.ask.drop_it(&mut sys.net.tcp);
        }
        return;
    }
    let (request, answer) = room.get_or_insert_with(|| (alloc::vec![0u8; companion::REQUEST].into_boxed_slice(), alloc::vec![0u8; companion::ANSWER].into_boxed_slice()));
    let now = sys.ticks;
    let Some(going) = call else {
        let who = companion::who(&sys.db);
        let Some(place) = ask::place(who.place.bytes()) else {
            return companion::give_up(sys, Trouble::NoPlace);
        };
        // the address of the model's server, by its name if it has one:
        // asked for again at each tick until it is known
        let to = match place.ip {
            Some(to) => to,
            None => match sys.net.name(place.machine, now, nic) {
                dns::Lookup::Found(to) => to,
                dns::Lookup::Asking => return,
                dns::Lookup::Failed(why) => return companion::heard(sys, Err(ask::Failed::Name(why))),
            },
        };
        let Some(length) = companion::request(sys, request) else {
            return companion::give_up(sys, Trouble::TooLong);
        };
        let tls = match place.secret {
            false => None,
            true => {
                let mut chance = [0u8; 64];
                if !arch::random(&mut chance) {
                    return companion::heard(sys, Err(ask::Failed::NoChance));
                }
                let Some(date) = sys.now() else {
                    return companion::heard(sys, Err(ask::Failed::NoClock));
                };
                // the clock counts from the year 2000, certificates from 1970
                Some(Box::new(xila_tls::Client::new(place.machine, &chance, date + 946_684_800, xila_tls::public_roots())))
            }
        };
        let lines = companion::lines(sys, place.secret);
        let question = ask::Question { how: b"POST", lines: lines.bytes(), bytes: &request[..length] };
        match ask::Ask::start(&mut sys.net.tcp, to, &place, &question, now) {
            Ok(mut ask) => {
                // a model may think for a while before it says anything
                ask.patience = companion::PATIENCE;
                *call = Some(Thinking { ask, tls, length });
            }
            Err(why) => companion::heard(sys, Err(why)),
        }
        return;
    };
    let step = going.ask.step(&mut sys.net.tcp, going.tls.as_deref_mut(), &request[..going.length], answer, now);
    match step {
        ask::Step::Going => {}
        ask::Step::Done(status) => {
            let kept = going.ask.kept;
            *call = None;
            companion::heard(sys, Ok((status, &answer[..kept])));
        }
        ask::Step::Failed(why) => {
            *call = None;
            companion::heard(sys, Err(why));
        }
    }
}

/// Finds the address of the machine named `System::asked_machine`: in
/// what is known, or by asking the name server and waiting for its
/// answer. The machine does nothing else meanwhile, three seconds at
/// most. `start` is when the command began to be served, by `micros`.
fn named(sys: &mut System, nic: &mut e1000::Nic, start: u64) -> Result<xila_core::net::Ip, dns::Failed> {
    // the time of the network, which stands still while a command is served
    let now = |sys: &System| sys.ticks + (arch::micros() - start) / 10_000;
    let name = sys.asked_machine;
    let mut frame = [0u8; 2048];
    loop {
        while let Some(length) = nic.receive(&mut frame) {
            let at = now(sys);
            // a command from the network that comes meanwhile is not run: who sent it asks again
            let _ = sys.net.receive(&frame[..length], at, nic);
        }
        let at = now(sys);
        sys.net.tick(at, nic);
        match sys.net.name(name.bytes(), at, nic) {
            dns::Lookup::Found(ip) => return Ok(ip),
            dns::Lookup::Asking => core::hint::spin_loop(),
            dns::Lookup::Failed(why) => return Err(why),
        }
    }
}

/// Asks a machine for a page, or with `post` sends it the text of
/// `System::asked_text`, and waits for its answer: how it went, the
/// start of what it said and how much it said in all. The machine does
/// nothing else meanwhile: until the answer is whole, or until five
/// seconds have passed without a byte, after the three at most that
/// finding its address may take when it is asked by its name (`to` is
/// then None). With `secret` it is asked through TLS: the client is made
/// here, on the heap (it is 37 KB), and lives as long as the question.
fn get(sys: &mut System, nic: &mut e1000::Nic, to: Option<xila_core::net::Ip>, port: u16, secret: bool, post: bool) -> Result<shell::Got, ask::Failed> {
    let start = arch::micros();
    // the time of the network, which stands still while a command is served
    let now = |sys: &System| sys.ticks + (arch::micros() - start) / 10_000;
    // what TLS needs is looked at before anything is asked of anyone
    let mut tls = match secret {
        false => None,
        true => {
            let mut chance = [0u8; 64];
            if !arch::random(&mut chance) {
                return Err(ask::Failed::NoChance);
            }
            // the clock counts from the year 2000, certificates from 1970
            let date = sys.now().ok_or(ask::Failed::NoClock)? + 946_684_800;
            Some(Box::new(xila_tls::Client::new(sys.asked_machine.bytes(), &chance, date, xila_tls::public_roots())))
        }
    };
    let to = match to {
        Some(to) => to,
        None => named(sys, nic, start).map_err(ask::Failed::Name)?,
    };
    let (machine, path, text) = (sys.asked_machine, sys.asked_path, sys.asked_text);
    let place = ask::Place { machine: machine.bytes(), ip: Some(to), port, path: path.bytes(), secret };
    let question = if post { ask::Question::post(text.bytes()) } else { ask::Question::GET };
    let at = now(sys);
    let mut asking = ask::Ask::start(&mut sys.net.tcp, to, &place, &question, at)?;
    // the start of what they say is kept; the rest is counted
    let mut start = [0u8; 640];
    let mut frame = [0u8; 2048];
    loop {
        while let Some(length) = nic.receive(&mut frame) {
            let at = now(sys);
            // a command from the network that comes meanwhile is not run: who sent it asks again
            let _ = sys.net.receive(&frame[..length], at, nic);
        }
        let at = now(sys);
        sys.net.tick(at, nic);
        let step = asking.step(&mut sys.net.tcp, tls.as_deref_mut(), question.bytes, &mut start, at);
        sys.net.speak(at, nic);
        match step {
            ask::Step::Going => core::hint::spin_loop(),
            ask::Step::Done(status) => return Ok(shell::Got { status, start: FixedStr::from_bytes(&start[..asking.kept]), total: asking.total }),
            ask::Step::Failed(why) => return Err(why),
        }
    }
}

/// Fetches the system from a machine that serves it, into memory kept
/// for that, and makes it the system `install` puts on a disk. Returns
/// its size and its check number. The machine does nothing else
/// meanwhile, a few seconds at most.
fn fetch(sys: &mut System, nic: &mut e1000::Nic, server: xila_core::net::Ip, system: &mut &'static [u8]) -> Result<(usize, u32), tftp::Failed> {
    /// Room for a system: several times what it weighs today.
    const ROOM: usize = 8 << 20;
    static KEPT: AtomicUsize = AtomicUsize::new(0);
    if KEPT.load(Ordering::Relaxed) == 0 {
        KEPT.store(memory::pages(ROOM / 4096).ok_or(tftp::Failed::NoMemory)?, Ordering::Relaxed);
    }
    let kept = KEPT.load(Ordering::Relaxed);
    if system.as_ptr() as usize == kept {
        // the system fetched before is about to be written over
        *system = &[];
        sys.system_bytes = 0;
    }
    // Safety: the pages are this function's alone, and nothing else reads
    // them while it writes: `system` was just taken off them.
    let into = unsafe { core::slice::from_raw_parts_mut(kept as *mut u8, ROOM) };
    let start = arch::micros();
    // the time of the network, which stands still while a command is served
    let now = |sys: &System| sys.ticks + (arch::micros() - start) / 10_000;
    let mut fetch = tftp::Fetch::start(server, now(sys), &mut sys.net, nic);
    let mut frame = [0u8; 2048];
    let length = loop {
        let mut step = tftp::Step::Going;
        while let Some(length) = nic.receive(&mut frame) {
            let at = now(sys);
            if let Some(message) = sys.net.receive(&frame[..length], at, nic) {
                step = fetch.message(&message, into, at, &mut sys.net, nic);
            }
        }
        let at = now(sys);
        sys.net.tick(at, nic);
        if step == tftp::Step::Going {
            step = fetch.tick(at, &mut sys.net, nic);
        }
        match step {
            tftp::Step::Done(length) => break length,
            tftp::Step::Failed(why) => return Err(why),
            // a minute is far more than a system takes on a local network
            tftp::Step::Going if arch::micros() - start > 60_000_000 => return Err(tftp::Failed::Silent),
            tftp::Step::Going => core::hint::spin_loop(),
        }
    };
    let fetched: &'static [u8] = &into[..length];
    if !tftp::is_system(fetched) {
        return Err(tftp::Failed::NotASystem);
    }
    *system = fetched;
    sys.system_bytes = length;
    Ok((length, xila_core::gpt::crc32(0, fetched)))
}

/// What `net` shows of the card.
fn card(nic: &mut e1000::Nic) -> shell::NetCard {
    shell::NetCard { name: nic.name, link: nic.link(), damaged: nic.damaged, missed: nic.missed, sent: nic.sent, refused: nic.refused }
}

/// A command that came from another machine, for the prompt for the
/// network: who to answer, the number of the request, and the line to
/// run, or None to send the last answer again.
struct Call {
    from: xila_core::net::Ip,
    port: u16,
    number: u32,
    line: Option<FixedStr<{ remote::LINE }>>,
}

/// Takes the frames that came from the cable and lets the network answer
/// them, then lets it do what the passing time asks for. Returns the
/// command for the prompt that came among them, if one did: one at a time.
fn network(sys: &mut System, nic: &mut e1000::Nic) -> Option<Call> {
    let mut frame = [0u8; 2048];
    let mut call = None;
    // no more at once than the card holds
    for _ in 0..32 {
        let Some(length) = nic.receive(&mut frame) else {
            break;
        };
        // a message for a port of this machine: only the prompt has one
        let Some(message) = sys.net.receive(&frame[..length], sys.ticks, nic) else {
            continue;
        };
        if message.port != remote::PORT || call.is_some() {
            continue;
        }
        let local = sys.net.local(message.from);
        let (number, line) = match sys.remote.asked(message.from, local, message.bytes) {
            remote::Asked::Nothing => continue,
            remote::Asked::Again(number) => (number, None),
            remote::Asked::Run(number, line) => {
                let mut kept = FixedStr::new();
                kept.push_bytes(line);
                (number, Some(kept))
            }
        };
        call = Some(Call { from: message.from, port: message.from_port, number, line });
    }
    sys.net.tick(sys.ticks, nic);
    call
}

/// Changes how pictures get to the screen. The screen is written directly
/// when what was asked cannot be done.
fn flip(sys: &mut System, stage: &mut Stage, wish: Pictures) -> Result<Pictures, xila_core::intel::NoFlip> {
    use xila_core::intel::{self, NoFlip};
    stage.screen.direct();
    sys.display.shown = Pictures::Direct;
    let flipper = match wish {
        Pictures::Direct => return Ok(wish),
        Pictures::Trial => {
            let (screen, words) = stage.screen.memory();
            chip::Flipper::trial(screen, words)
        }
        Pictures::Two => {
            let card = sys.card.filter(|card| card.registers != 0).ok_or(NoFlip::NoPicture)?;
            let (_, generation) = intel::known(card.id).ok_or(NoFlip::NoPicture)?;
            chip::Flipper::chip(&card, generation)?
        }
    };
    // The proof, before anything else counts on it: the picture as it is
    // now, shown through the second of the two.
    stage.screen.flip(flipper);
    stage.screen.show();
    if !stage.screen.settle() {
        stage.screen.direct();
        return Err(NoFlip::Deaf);
    }
    sys.display.shown = wish;
    Ok(wish)
}

/// Times the drawing of a whole screen: every character into the prepared
/// picture, then the whole picture to the screen.
fn measure(sys: &System, grid: &mut Grid, stage: &mut Stage) -> Measure {
    const ROUNDS: u32 = 5;
    let (mut drawn, mut shown) = (u64::MAX, u64::MAX);
    for _ in 0..ROUNDS {
        // with two pictures, the last one must be on the screen before the next
        stage.screen.settle();
        grid.mark_all();
        let start = arch::micros();
        stage.screen.compose(grid, THEMES[sys.theme], (0, 0));
        drawn = drawn.min(arch::micros() - start);
        // the picture goes back over the characters before anyone sees them
        if sys.code.is_some() {
            stage.screen.blit(&stage.code, city::WIDTH, BODY_TOP);
        } else if sys.screen == 0 {
            stage.screen.blit(&stage.picture, city::WIDTH, BODY_TOP);
        }
        stage.screen.show_all();
        let start = arch::micros();
        stage.screen.show();
        shown = shown.min(arch::micros() - start);
    }
    Measure { rounds: ROUNDS, drawn_us: drawn as u32, shown_us: shown as u32 }
}

/// A task that never rests. It is there to show that the others keep running.
extern "C" fn spin(_: usize) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

/// The task that carries the jobs nobody waits for: those that were
/// submitted or sent to the background, and the programs that run by
/// themselves. It lets one of them run for a slice, serves what it may
/// have asked of the kernel, and gives the system back: nothing a job
/// does holds the system longer than a slice and one instruction.
extern "C" fn jobs(_: usize) -> ! {
    loop {
        let worked = {
            let mut shared = SHARED.lock();
            let Shared { ui, sys, stage, disks, system, nic, .. } = &mut *shared;
            if xila_core::jobs::idle(sys) || sys.request.is_some() {
                false
            } else {
                observe(sys);
                let began = arch::micros();
                if let Some(at) = ui.carry(sys, &mut || arch::micros().wrapping_sub(began) >= SLICE) {
                    serve(sys, ui, stage.as_mut(), disks, system, nic.as_mut());
                    ui.carried(sys, at);
                }
                true
            }
        };
        if worked {
            arch::yield_now();
        } else {
            task::sleep(1);
        }
    }
}

/// The task of the shell on the serial port.
extern "C" fn serial_shell(_: usize) -> ! {
    let serial = serial();
    let mut line = FixedStr::<240>::new();
    let mut after_return = false;
    // What came while a program was waited for, to be read once it has
    // ended: who types ahead, or pastes several lines, loses nothing.
    let mut ahead = FixedStr::<240>::new();
    let mut taken = 0;
    serial.write(PROMPT);
    loop {
        while let Some(byte) = {
            let kept = ahead.bytes().get(taken).copied();
            match kept {
                Some(_) => taken += 1,
                None if taken > 0 => {
                    ahead.clear();
                    taken = 0;
                }
                None => {}
            }
            kept.or_else(|| serial.read())
        } {
            match byte {
                b'\r' | b'\n' => {
                    // a line feed right after a carriage return ends the same line
                    if byte == b'\n' && after_return {
                        after_return = false;
                        continue;
                    }
                    after_return = byte == b'\r';
                    serial.write(b"\n");
                    let ok = {
                        let mut shared = SHARED.lock();
                        let Shared { ui, sys, stage, disks, system, nic, .. } = &mut *shared;
                        observe(sys);
                        counted(sys, nic.as_mut());
                        sys.serial_bytes += line.len() as u64 + 40;
                        sys.session = events::SERIAL;
                        // `peek` tells what the screen shows after the commands
                        // before it, whether or not the interface task has
                        // had the time to redraw
                        if ui.dirty {
                            ui.render(sys);
                        }
                        let ok = shell::exec(sys, line.bytes(), &mut SerialOut { serial: &serial });
                        // as with PostgreSQL, what this prompt listens to arrives with
                        // the next command; an empty line is enough to ask
                        while let Some(event) = sys.events.take(events::SERIAL) {
                            serial.write(b"#event ");
                            serial.write(event.channel.bytes());
                            serial.write(b"\t");
                            serial.write(event.text.bytes());
                            serial.write(b"\n");
                        }
                        let lost = sys.events.take_lost(events::SERIAL);
                        if lost > 0 {
                            let mut note = FixedStr::<32>::new();
                            let _ = write!(note, "#lost {lost}\n");
                            serial.write(note.bytes());
                        }
                        serve(sys, ui, stage.as_mut(), disks, system, nic.as_mut());
                        // a request the kernel could not meet makes the command a failure
                        let ok = shell::served(sys, &mut SerialOut { serial: &serial }) && ok;
                        ui.dirty = true;
                        // a block is open: more lines are expected before anything runs
                        if sys.blocks[events::SERIAL].open() {
                            serial.write(b"#more\n");
                        }
                        ok
                    };
                    // A program: this prompt waits for it, and the rest of the
                    // machine does not. Ctrl+C stops it.
                    let mut ok = ok;
                    loop {
                        {
                            let mut shared = SHARED.lock();
                            let Shared { ui, sys, stage, disks, system, nic, .. } = &mut *shared;
                            let watch = xila_core::jobs::Watch::Serial;
                            let Some(at) = xila_core::jobs::watched(sys, watch) else {
                                break;
                            };
                            while let Some(byte) = serial.read() {
                                if byte == 0x03 {
                                    if let Some(message) = xila_core::jobs::stop_watched(sys, watch) {
                                        serial.write(message.bytes());
                                        serial.write(b"\n");
                                    }
                                } else {
                                    // (a line ahead at most: what is beyond is lost, as it would be in the card)
                                    ahead.push(byte);
                                }
                            }
                            observe(sys);
                            let began = arch::micros();
                            xila_core::jobs::turn(sys, at, &mut SerialOut { serial: &serial }, &mut || arch::micros().wrapping_sub(began) >= SLICE_WAITED);
                            if xila_core::jobs::asks(sys) {
                                serve(sys, ui, stage.as_mut(), disks, system, nic.as_mut());
                                let answered = shell::served(sys, &mut SerialOut { serial: &serial });
                                xila_core::jobs::answered(sys, at, answered);
                            }
                            ui.dirty = true;
                            if let Some(went) = xila_core::jobs::outcome(sys, watch) {
                                ok &= went;
                                // what the program made this prompt hear comes with its own status line
                                while let Some(event) = sys.events.take(events::SERIAL) {
                                    serial.write(b"#event ");
                                    serial.write(event.channel.bytes());
                                    serial.write(b"\t");
                                    serial.write(event.text.bytes());
                                    serial.write(b"\n");
                                }
                                break;
                            }
                        }
                        arch::yield_now();
                    }
                    // A question to the companion: this prompt waits for
                    // the answer, and the rest of the machine does not.
                    loop {
                        {
                            let mut shared = SHARED.lock();
                            let sys = &mut shared.sys;
                            if !xila_core::companion::waits(sys, events::SERIAL) {
                                break;
                            }
                            if let Some(told) = xila_core::companion::tell(sys, events::SERIAL, xila_core::grid::COLS - 1, &mut SerialOut { serial: &serial }) {
                                ok &= told;
                                break;
                            }
                        }
                        task::sleep(1);
                    }
                    serial.write(if ok { b"#ok\n" } else { b"#error\n" });
                    serial.write(PROMPT);
                    line.clear();
                }
                0x08 | 0x7f => {
                    after_return = false;
                    if !line.is_empty() {
                        line.remove(line.len() - 1);
                        serial.write(b"\x08 \x08");
                    }
                }
                0x20..=0x7e => {
                    after_return = false;
                    if line.push(byte) {
                        serial.write_byte(byte);
                    }
                }
                _ => after_return = false,
            }
        }
        task::sleep(1);
    }
}

/// Shows bytes of the system character set in a formatted text.
struct Ascii<'a>(&'a [u8]);

impl core::fmt::Display for Ascii<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        for &b in self.0 {
            f.write_char(if b.is_ascii() { b as char } else { '?' })?;
        }
        Ok(())
    }
}

/// Reports an error the kernel cannot continue after, on the serial port, and stops.
pub fn fatal(what: core::fmt::Arguments) -> ! {
    arch::interrupts_off();
    let mut serial = Serial::init();
    let _ = write!(serial, "\n#fatal {what}\n");
    arch::stop()
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fatal(format_args!("{info}"))
}
