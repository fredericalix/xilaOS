//! Keyboard: PS/2 scancodes (set 1) to key events.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    /// A character in the character set of the font.
    Char(u8),
    Enter,
    Backspace,
    Tab,
    Esc,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Delete,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyEvent {
    pub key: Key,
    pub alt: bool,
    pub ctrl: bool,
    /// The digit printed on a key of the top row, whatever the layout makes
    /// of it. Shortcuts like Alt+1 use this: on a French keyboard the digits
    /// need Shift.
    pub digit: Option<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layout {
    Us,
    Fr,
}

impl Layout {
    pub fn name(self) -> &'static [u8] {
        match self {
            Layout::Us => b"us",
            Layout::Fr => b"fr",
        }
    }
}

/// Characters for scancodes 0x00-0x57: plain, with Shift, with AltGr. 0 = none.
struct Table {
    plain: [u8; 0x58],
    shift: [u8; 0x58],
    altgr: [u8; 0x58],
}

/// Fills a row of keys starting at scancode `at`.
const fn row(mut table: [u8; 0x58], at: usize, chars: &[u8]) -> [u8; 0x58] {
    let mut i = 0;
    while i < chars.len() {
        table[at + i] = chars[i];
        i += 1;
    }
    table
}

const fn keys(digits: &[u8], top: &[u8], home: &[u8], bottom: &[u8], extra: u8) -> [u8; 0x58] {
    let mut t = [0u8; 0x58];
    t = row(t, 0x02, digits);
    t = row(t, 0x10, top);
    t = row(t, 0x1e, home);
    t = row(t, 0x2b, bottom);
    t[0x39] = b' ';
    t[0x56] = extra;
    t
}

const US: Table = Table {
    plain: keys(b"1234567890-=", b"qwertyuiop[]", b"asdfghjkl;'`", b"\\zxcvbnm,./", b'\\'),
    shift: keys(b"!@#$%^&*()_+", b"QWERTYUIOP{}", b"ASDFGHJKL:\"~", b"|ZXCVBNM<>?", b'|'),
    altgr: [0; 0x58],
};

// French AZERTY (PC). Accented letters use the codes of the font (code page 437).
const FR: Table = Table {
    plain: keys(b"&\x82\"'(-\x8a_\x87\x85)=", b"azertyuiop^$", b"qsdfghjklm\x97\xfd", b"*wxcvbn,;:!", b'<'),
    shift: keys(b"1234567890\xf8+", b"AZERTYUIOP\"\x9c", b"QSDFGHJKLM%\0", b"\xe6WXCVBN?./\x15", b'>'),
    altgr: keys(b"\0~#{[|`\\^@]}", b"", b"", b"", 0),
};

/// Keeps the state of the modifier keys between scancodes.
pub struct Decoder {
    shift_left: bool,
    shift_right: bool,
    ctrl: bool,
    alt: bool,
    altgr: bool,
    caps: bool,
    extended: bool,
}

impl Decoder {
    pub const fn new() -> Decoder {
        Decoder { shift_left: false, shift_right: false, ctrl: false, alt: false, altgr: false, caps: false, extended: false }
    }

    /// Takes one scancode byte. Returns an event when a key was pressed.
    pub fn feed(&mut self, layout: Layout, code: u8) -> Option<KeyEvent> {
        if code == 0xe0 {
            self.extended = true;
            return None;
        }
        let extended = core::mem::replace(&mut self.extended, false);
        let pressed = code & 0x80 == 0;
        let code = (code & 0x7f) as usize;

        match (extended, code) {
            (false, 0x2a) => self.shift_left = pressed,
            (false, 0x36) => self.shift_right = pressed,
            (true, 0x2a) | (true, 0x36) => {} // sent around some extended keys
            (_, 0x1d) => self.ctrl = pressed,
            (false, 0x38) => self.alt = pressed,
            (true, 0x38) => self.altgr = pressed,
            (false, 0x3a) => {
                if pressed {
                    self.caps = !self.caps;
                }
            }
            _ => return if pressed { self.pressed(layout, extended, code) } else { None },
        }
        None
    }

    fn pressed(&self, layout: Layout, extended: bool, code: usize) -> Option<KeyEvent> {
        let mut digit = None;
        let key = if extended {
            match code {
                0x48 => Key::Up,
                0x50 => Key::Down,
                0x4b => Key::Left,
                0x4d => Key::Right,
                0x49 => Key::PageUp,
                0x51 => Key::PageDown,
                0x47 => Key::Home,
                0x4f => Key::End,
                0x53 => Key::Delete,
                0x1c => Key::Enter,
                0x35 => Key::Char(b'/'),
                _ => return None,
            }
        } else {
            match code {
                0x01 => Key::Esc,
                0x0e => Key::Backspace,
                0x0f => Key::Tab,
                0x1c => Key::Enter,
                _ if code >= 0x58 => return None,
                _ => {
                    if (0x02..=0x0b).contains(&code) {
                        digit = Some(((code - 1) % 10) as u8);
                    }
                    let table = match layout {
                        Layout::Us => &US,
                        Layout::Fr => &FR,
                    };
                    let shift = self.shift_left || self.shift_right;
                    let mut ch = if self.altgr {
                        table.altgr[code]
                    } else if shift {
                        table.shift[code]
                    } else {
                        table.plain[code]
                    };
                    if self.caps && ch.is_ascii_alphabetic() {
                        ch ^= 0x20;
                    }
                    if ch == 0 {
                        return None;
                    }
                    Key::Char(ch)
                }
            }
        };
        Some(KeyEvent { key, alt: self.alt, ctrl: self.ctrl, digit })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(layout: Layout, codes: &[u8]) -> Vec<u8> {
        let mut d = Decoder::new();
        codes
            .iter()
            .filter_map(|&c| match d.feed(layout, c) {
                Some(KeyEvent { key: Key::Char(ch), .. }) => Some(ch),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn same_keys_two_layouts() {
        // the keys Q W A Z of a US keyboard, pressed and released
        let codes = [0x10, 0x90, 0x11, 0x91, 0x1e, 0x9e, 0x2c, 0xac];
        assert_eq!(chars(Layout::Us, &codes), b"qwaz");
        assert_eq!(chars(Layout::Fr, &codes), b"azqw");
    }

    #[test]
    fn shift_and_altgr() {
        // Shift down, key 2, Shift up, key 2
        assert_eq!(chars(Layout::Us, &[0x2a, 0x03, 0x83, 0xaa, 0x03]), b"@2");
        assert_eq!(chars(Layout::Fr, &[0x2a, 0x03, 0x83, 0xaa, 0x03]), b"2\x82");
        // AltGr (E0 38) + key 0 gives @ on a French keyboard
        assert_eq!(chars(Layout::Fr, &[0xe0, 0x38, 0x0b]), b"@");
    }

    #[test]
    fn alt_digit_keeps_the_printed_digit() {
        let mut d = Decoder::new();
        assert_eq!(d.feed(Layout::Fr, 0x38), None);
        let ev = d.feed(Layout::Fr, 0x02).unwrap();
        assert!(ev.alt);
        assert_eq!(ev.digit, Some(1));
        let ev = d.feed(Layout::Fr, 0x0b).unwrap();
        assert_eq!(ev.digit, Some(0));
    }

    #[test]
    fn extended_keys() {
        let mut d = Decoder::new();
        assert_eq!(d.feed(Layout::Us, 0xe0), None);
        assert_eq!(d.feed(Layout::Us, 0x48).unwrap().key, Key::Up);
        assert_eq!(d.feed(Layout::Us, 0x48), None); // without the prefix: keypad 8, ignored
        assert_eq!(d.feed(Layout::Us, 0x01).unwrap().key, Key::Esc);
    }
}
