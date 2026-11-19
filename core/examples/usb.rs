//! Makes the image of a USB stick that starts XilaOS, on the development
//! machine: the same disk the `install` command makes, in a file.
//!
//! usage: usb SYSTEM IMAGE [MEGABYTES]
//!
//! SYSTEM is the kernel as the firmware starts it (BOOTX64.EFI). IMAGE is
//! the file to make; write it whole on a stick to start a machine from it.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use xila_core::gpt::SECTOR;
use xila_core::install::{whole, Sectors};

struct Image(File);

impl Sectors for Image {
    fn read(&mut self, at: u64, into: &mut [u8]) -> bool {
        self.0.seek(SeekFrom::Start(at * SECTOR as u64)).is_ok() && self.0.read_exact(into).is_ok()
    }

    fn write(&mut self, at: u64, from: &[u8]) -> bool {
        self.0.seek(SeekFrom::Start(at * SECTOR as u64)).is_ok() && self.0.write_all(from).is_ok()
    }

    fn flush(&mut self) -> bool {
        self.0.sync_all().is_ok()
    }
}

fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    if !(3..=4).contains(&arguments.len()) {
        eprintln!("usage: usb SYSTEM IMAGE [MEGABYTES]");
        std::process::exit(2);
    }
    let megabytes: u64 = arguments.get(3).map_or(128, |text| text.parse().expect("MEGABYTES is a number"));
    let system = std::fs::read(&arguments[1]).expect("the system file can be read");
    // a new file each time: nothing of an older image stays in it
    let file = File::options().read(true).write(true).create(true).truncate(true).open(&arguments[2]).expect("the image can be made");
    file.set_len(megabytes << 20).expect("the image can be given its size");
    let seed = SystemTime::now().duration_since(UNIX_EPOCH).map_or(1, |since| since.as_nanos() as u64);
    match whole(&mut Image(file), megabytes << 11, &system, seed) {
        Ok(_) => println!("{}", arguments[2]),
        Err(failed) => {
            eprintln!("the image could not be made: {failed:?}");
            std::process::exit(1);
        }
    }
}
