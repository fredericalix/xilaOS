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

/// The key right of P on a French keyboard is a dead key: its accent
/// (circumflex, or diaeresis with Shift) waits for the letter that comes
/// next. The font has no diaeresis alone: a double quote stands for it.
const DEAD: usize = 0x1a;

/// A letter under the accent of a dead key, for the letters the font has
/// (code page 437).
fn accented(accent: u8, letter: u8) -> Option<u8> {
    let (letters, with): (&[u8], &[u8]) = match accent {
        b'^' => (b"aeiou", b"\x83\x88\x8c\x93\x96"),
        _ => (b"aeiouyAOU", b"\x84\x89\x8b\x94\x81\x98\x8e\x99\x9a"),
    };
    letters.iter().position(|l| *l == letter).map(|at| with[at])
}

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
    /// The accent of a dead key that waits for its letter.
    dead: Option<u8>,
    /// A second event that the last scancode made: `rest` gives it.
    second: Option<KeyEvent>,
}

impl Decoder {
    pub const fn new() -> Decoder {
        Decoder { shift_left: false, shift_right: false, ctrl: false, alt: false, altgr: false, caps: false, extended: false, dead: None, second: None }
    }

    /// The second event of the scancode `feed` just took, if it made two:
    /// an accent that found no letter to go on, then the key itself.
    pub fn rest(&mut self) -> Option<KeyEvent> {
        self.second.take()
    }

    /// Takes one scancode byte. Returns an event when a key was pressed;
    /// `rest` may then have one more.
    pub fn feed(&mut self, layout: Layout, code: u8) -> Option<KeyEvent> {
        let dead_key = layout == Layout::Fr && code as usize == DEAD && !self.extended && !self.altgr;
        let event = self.scan(layout, code)?;
        let plain = !event.alt && !event.ctrl;
        if dead_key && plain {
            // a dead key says nothing by itself: it waits for the next key.
            // Pressed twice, the first accent comes out alone.
            let accent = if event.key == Key::Char(b'"') { b'"' } else { b'^' };
            return self.dead.replace(accent).map(|before| KeyEvent { key: Key::Char(before), ..event });
        }
        let Some(accent) = self.dead.take() else {
            return Some(event);
        };
        let alone = KeyEvent { key: Key::Char(accent), alt: false, ctrl: false, digit: None };
        match event.key {
            Key::Char(letter) if plain => match accented(accent, letter) {
                Some(with) => Some(KeyEvent { key: Key::Char(with), ..event }),
                // a space gives the accent alone
                None if letter == b' ' => Some(alone),
                None => {
                    self.second = Some(event);
                    Some(alone)
                }
            },
            // the accent is taken back
            Key::Backspace | Key::Esc => None,
            _ => {
                self.second = Some(event);
                Some(alone)
            }
        }
    }

    fn scan(&mut self, layout: Layout, code: u8) -> Option<KeyEvent> {
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
    fn dead_keys_put_their_accent_on_the_next_letter() {
        // the key right of P, then E (0x12), I (0x17), then the key marked A on it (0x10)
        assert_eq!(chars(Layout::Fr, &[0x1a, 0x9a, 0x12, 0x92]), b"\x88");
        assert_eq!(chars(Layout::Fr, &[0x1a, 0x17, 0x1a, 0x10]), b"\x8c\x83");
        // with Shift it is a diaeresis; the letter may be a capital
        assert_eq!(chars(Layout::Fr, &[0x2a, 0x1a, 0xaa, 0x12]), b"\x89");
        assert_eq!(chars(Layout::Fr, &[0x2a, 0x1a, 0x16, 0xaa]), b"\x9a");
        // a space gives the accent alone, and so does the key pressed twice
        assert_eq!(chars(Layout::Fr, &[0x1a, 0x39]), b"^");
        assert_eq!(chars(Layout::Fr, &[0x1a, 0x1a, 0x39]), b"^^");
        // a letter that takes no such accent comes after it
        let mut d = Decoder::new();
        assert_eq!(d.feed(Layout::Fr, 0x1a), None);
        assert_eq!(d.feed(Layout::Fr, 0x14).map(|e| e.key), Some(Key::Char(b'^')));
        assert_eq!(d.rest().map(|e| e.key), Some(Key::Char(b't')));
        assert_eq!(d.rest(), None);
        // a capital has no circumflex in the font
        assert_eq!(d.feed(Layout::Fr, 0x1a), None);
        assert_eq!(d.feed(Layout::Fr, 0x2a), None);
        assert_eq!(d.feed(Layout::Fr, 0x12).map(|e| e.key), Some(Key::Char(b'^')));
        assert_eq!(d.rest().map(|e| e.key), Some(Key::Char(b'E')));
        assert_eq!(d.feed(Layout::Fr, 0xaa), None);
        // Enter comes after the accent; Backspace takes the accent back
        assert_eq!(d.feed(Layout::Fr, 0x1a), None);
        assert_eq!(d.feed(Layout::Fr, 0x1c).map(|e| e.key), Some(Key::Char(b'^')));
        assert_eq!(d.rest().map(|e| e.key), Some(Key::Enter));
        assert_eq!(d.feed(Layout::Fr, 0x1a), None);
        assert_eq!(d.feed(Layout::Fr, 0x0e), None);
        assert_eq!(d.feed(Layout::Fr, 0x12).map(|e| e.key), Some(Key::Char(b'e')));
        // AltGr and 9 still give the circumflex at once, and the US layout has no dead key
        assert_eq!(chars(Layout::Fr, &[0xe0, 0x38, 0x0a]), b"^");
        assert_eq!(chars(Layout::Us, &[0x1a, 0x12]), b"[e");
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
