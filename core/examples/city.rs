//! Draws the city on the development machine, to look at it without starting
//! the system: `cargo run -p xila-core --example city -- day > city.ppm`.

use std::io::Write;

use xila_core::city::{City, HEIGHT, WIDTH};
use xila_core::shell::{System, TaskInfo};
use xila_core::text::FixedStr;
use xila_core::theme::THEMES;

fn main() {
    let night = std::env::args().nth(1).as_deref() == Some("night");
    let mut sys = System::new();
    sys.machine.serial = true;
    sys.energy = 600;
    sys.log_rows = 11;
    sys.term_rows = 40;
    sys.serial_rate = 120;
    let tasks: [(&[u8], u16); 5] = [(b"interface", 60), (b"idle", 300), (b"serial shell", 40), (b"spin", 450), (b"spin", 150)];
    for (id, (name, share)) in tasks.iter().enumerate() {
        sys.tasks[id] = TaskInfo { name: FixedStr::from_bytes(name), state: b"ready", ticks: 0, system: id < 3, used: true };
        sys.cpu_share[id] = *share;
    }
    let mut city = City::new();
    for _ in 0..60 {
        city.step(&sys);
    }
    let mut pixels = vec![0u32; WIDTH * HEIGHT];
    city.draw(&sys, THEMES[night as usize], &mut pixels);

    let mut out = std::io::stdout().lock();
    writeln!(out, "P6 {WIDTH} {HEIGHT} 255").unwrap();
    for p in pixels {
        out.write_all(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]).unwrap();
    }
}
