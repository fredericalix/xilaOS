//! Hardware independent logic of XilaOS.
//!
//! Everything here is plain computation on fixed size buffers: no ports, no
//! pointers into the machine, and no heap of its own (only the storage
//! engine, `xila-db`, allocates: `System::db`). The kernel feeds it scancodes, serial
//! bytes and timer ticks, and copies the resulting character grid to the
//! screen. That keeps this crate testable on the development machine.
//!
//! Text is bytes in the character set of the V2_OS font (ASCII below 0x80),
//! not UTF-8, so it is handled as `&[u8]` throughout.

#![cfg_attr(not(test), no_std)]

pub mod ask;
pub mod catalog;
pub mod change;
pub mod chars;
pub mod city;
pub mod clock;
pub mod code;
pub mod companion;
pub mod dns;
pub mod events;
pub mod expr;
pub mod fat;
pub mod font;
pub mod gpt;
pub mod grid;
pub mod install;
pub mod intel;
pub mod jobs;
pub mod json;
pub mod keys;
pub mod lang;
pub mod line;
pub mod machine;
pub mod mem;
pub mod net;
pub mod qr;
pub mod remote;
pub mod secrets;
pub mod settings;
pub mod shell;
pub mod sql;
pub mod tcp;
pub mod term;
pub mod text;
pub mod tftp;
pub mod theme;
pub mod ui;
pub mod waits;
pub mod watch;
pub mod web;

/// Version shown in the header and by `about`.
pub const VERSION: &[u8] = b"0.1";
