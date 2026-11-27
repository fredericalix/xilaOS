//! The characters of the font, and those of the rest of the world.
//!
//! Text in XilaOS is bytes in the character set of the V2_OS font: ASCII
//! below 0x80, and above it the accented letters and signs of code page
//! 850 where V2_OS kept them, among drawings of its own. Everything
//! outside speaks Unicode, written as UTF-8: a model of the Internet, a
//! browser. This is the way between the two, for whoever takes text in
//! or sends it out.
//!
//! The font does not have every letter: it has `É` and `À` but no `È`,
//! no `Ê`, no `œ`. What it lacks is shown by the nearest thing it has
//! (`E`, `oe`), and `?` stands for what it has nothing like.

/// The characters of the font above ASCII that Unicode has too, with
/// their number there. Read from the drawings of `font.rs`, one by one:
/// the bytes that are not here are drawings of V2_OS.
const BOTH: [(u8, u16); 76] = [
    (0x80, 0xc7), // C with a cedilla
    (0x81, 0xfc),
    (0x82, 0xe9), // e acute
    (0x83, 0xe2),
    (0x84, 0xe4),
    (0x85, 0xe0), // a grave
    (0x86, 0xe5),
    (0x87, 0xe7), // c with a cedilla
    (0x88, 0xea),
    (0x89, 0xeb),
    (0x8a, 0xe8), // e grave
    (0x8b, 0xef),
    (0x8c, 0xee),
    (0x8d, 0xec),
    (0x8e, 0xc4),
    (0x8f, 0xc5),
    (0x90, 0xc9), // E acute
    (0x91, 0xe6),
    (0x92, 0xc6),
    (0x93, 0xf4),
    (0x94, 0xf6),
    (0x95, 0xf2),
    (0x96, 0xfb),
    (0x97, 0xf9), // u grave
    (0x98, 0xff),
    (0x99, 0xd6),
    (0x9a, 0xdc),
    (0x9b, 0xf8),
    (0x9c, 0xa3), // pound
    (0x9d, 0xd8),
    (0x9e, 0xd7), // times
    (0x9f, 0x192),
    (0xa0, 0xe1),
    (0xa1, 0xed),
    (0xa2, 0xf3),
    (0xa3, 0xfa),
    (0xa4, 0xf1),
    (0xa5, 0xd1),
    (0xa6, 0xaa),
    (0xa7, 0xba),
    (0xa8, 0xbf),
    (0xa9, 0xae), // registered
    (0xaa, 0xac),
    (0xab, 0xbd), // a half
    (0xac, 0xbc),
    (0xad, 0xa1),
    (0xae, 0xab), // quotes of French, opening
    (0xaf, 0xbb), // and closing
    (0xb5, 0xc1),
    (0xb6, 0xc2),
    (0xb7, 0xc0), // A grave
    (0xb8, 0xa9), // copyright
    (0xbd, 0xa2),
    (0xbe, 0xa5),
    (0xd8, 0xcf),
    (0xdd, 0xa6),
    (0xde, 0xcc),
    (0xe9, 0xda),
    (0xea, 0xdb),
    (0xeb, 0xd9), // U grave
    (0xec, 0xfd),
    (0xed, 0xdd),
    (0xee, 0xaf),
    (0xef, 0xb4),
    (0xf1, 0xb1), // plus or minus
    (0xf3, 0xbe),
    (0xf4, 0xb6),
    (0xf5, 0xa7), // section
    (0xf6, 0xf7), // divided by
    (0xf7, 0xb8),
    (0xf8, 0xb0), // degree
    (0xf9, 0xa8),
    (0xfa, 0xb7), // a dot at mid height
    (0xfb, 0xb9),
    (0xfc, 0xb3),
    (0xfd, 0xb2), // squared
];

/// Every byte, in order: a place to borrow one character from.
const ONE: [u8; 256] = {
    let mut all = [0u8; 256];
    let mut at = 0;
    while at < 256 {
        all[at] = at as u8;
        at += 1;
    }
    all
};

fn one(byte: u8) -> &'static [u8] {
    &ONE[byte as usize..byte as usize + 1]
}

/// What the font shows for a character of Unicode: its own character
/// when it has it, the nearest thing when it does not, `?` when it has
/// nothing like it, and nothing at all for a mark that takes no room.
/// Line ends and tabs are for whoever calls: this knows what is drawn.
pub fn shown(code: u32) -> &'static [u8] {
    if (0x20..=0x7e).contains(&code) {
        return one(code as u8);
    }
    if let Some((byte, _)) = BOTH.iter().find(|(_, unicode)| *unicode as u32 == code) {
        return one(*byte);
    }
    match code {
        // spaces of every width, the one that does not break included
        0xa0 | 0x2000..=0x200a | 0x202f | 0x205f | 0x3000 => b" ",
        // what takes no room: joiners, marks of direction, accents written apart
        0xad | 0x300..=0x36f | 0x200b..=0x200f | 0x2060 | 0xfe00..=0xfe0f | 0xfeff => b"",
        // the quotes and dashes of typographers
        0x2018 | 0x2019 | 0x201a | 0x2032 => b"'",
        0x201c | 0x201d | 0x201e | 0x2033 => b"\"",
        0x2010..=0x2015 | 0x2212 => b"-",
        0x2039 => b"<",
        0x203a => b">",
        0x2026 => b"...",
        0x2022 | 0x2219 => one(0xfa),
        // letters the font lacks: without their accent, or in two letters
        0xc3 => b"A",
        0xc8 | 0xca | 0xcb => b"E",
        0xcd | 0xce => b"I",
        0xd2..=0xd5 => b"O",
        0x178 => b"Y",
        0xe3 => b"a",
        0xf5 => b"o",
        0x152 => b"OE",
        0x153 => b"oe",
        0xdf => b"ss",
        // signs that have a plain way of being written
        0x20ac => b"EUR",
        0x2190 => b"<-",
        0x2192 => b"->",
        0x2260 => b"!=",
        0x2264 => b"<=",
        0x2265 => b">=",
        _ => b"?",
    }
}

/// The number Unicode gives a character of the font. None for a drawing
/// of V2_OS, and for what is not drawn at all.
pub fn unicode(byte: u8) -> Option<u32> {
    match byte {
        0x20..=0x7e => Some(byte as u32),
        _ => BOTH.iter().find(|(font, _)| *font == byte).map(|(_, unicode)| *unicode as u32),
    }
}

/// A character of Unicode as UTF-8 writes it: the bytes, and how many.
pub fn utf8(code: u32) -> ([u8; 4], usize) {
    let low = |shift: u32| 0x80 | ((code >> shift) & 0x3f) as u8;
    match code {
        0..=0x7f => ([code as u8, 0, 0, 0], 1),
        0x80..=0x7ff => ([0xc0 | (code >> 6) as u8, low(0), 0, 0], 2),
        0x800..=0xffff => ([0xe0 | (code >> 12) as u8, low(6), low(0), 0], 3),
        _ => ([0xf0 | ((code >> 18) & 7) as u8, low(12), low(6), low(0)], 4),
    }
}

/// Reads the character of UTF-8 that `bytes` starts with: its number, and
/// how many bytes it took. None when they are not one.
pub fn read(bytes: &[u8]) -> Option<(u32, usize)> {
    let first = *bytes.first()?;
    let (length, start) = match first {
        0..=0x7f => return Some((first as u32, 1)),
        0xc2..=0xdf => (2, first as u32 & 0x1f),
        0xe0..=0xef => (3, first as u32 & 0x0f),
        0xf0..=0xf4 => (4, first as u32 & 0x07),
        _ => return None,
    };
    let mut code = start;
    for byte in bytes.get(1..length)? {
        if byte & 0xc0 != 0x80 {
            return None;
        }
        code = (code << 6) | (*byte as u32 & 0x3f);
    }
    Some((code, length))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A text of Unicode as the font shows it.
    fn seen(text: &str) -> Vec<u8> {
        text.chars().flat_map(|letter| shown(letter as u32).to_vec()).collect()
    }

    #[test]
    fn french_is_shown_with_its_accents() {
        // every accented letter French writes in small letters is in the font
        assert_eq!(seen("\u{e0}\u{e2}\u{e4}\u{e7}\u{e9}\u{e8}\u{ea}\u{eb}\u{ee}\u{ef}\u{f4}\u{f6}\u{f9}\u{fb}\u{fc}\u{ff}"), b"\x85\x83\x84\x87\x82\x8a\x88\x89\x8c\x8b\x93\x94\x97\x96\x81\x98");
        // of the capitals it has some
        assert_eq!(seen("\u{c0}\u{c2}\u{c4}\u{c7}\u{c9}\u{cf}\u{d6}\u{d9}\u{db}\u{dc}"), b"\xb7\xb6\x8e\x80\x90\xd8\x99\xeb\xea\x9a");
        // and the others come without their accent, or in two letters
        assert_eq!(seen("\u{c8}\u{ca}\u{cb}\u{ce}\u{d4} \u{153}uf, \u{152}UVRE"), b"EEEIO oeuf, OEUVRE");
        // what a sentence of French is written with
        assert_eq!(seen("\u{ab}\u{a0}D\u{e9}j\u{e0} l\u{2019}\u{e9}t\u{e9}\u{202f}?\u{a0}\u{bb} \u{2014} 20\u{a0}\u{b0}C, 3\u{a0}\u{20ac}\u{2026}"), b"\xae D\x82j\x85 l'\x82t\x82 ? \xaf - 20 \xf8C, 3 EUR...");
        // what the font has nothing like, and what takes no room
        assert_eq!(seen("\u{4e2d}\u{1f600}e\u{301}\u{200b}!"), b"??e!");
    }

    #[test]
    fn a_character_of_the_font_goes_out_and_comes_back_the_same() {
        let mut both = 0;
        for byte in 0..=255u8 {
            match unicode(byte) {
                Some(code) => {
                    both += 1;
                    assert_eq!(shown(code), [byte], "{byte:#x}");
                    // as UTF-8 writes it, read by this and by Rust's own
                    let (bytes, length) = utf8(code);
                    assert_eq!(read(&bytes[..length]), Some((code, length)));
                    assert_eq!(core::str::from_utf8(&bytes[..length]).unwrap().chars().next().unwrap() as u32, code);
                }
                // the drawings, and what is below the space
                None => assert!(!(0x20..=0x7e).contains(&byte)),
            }
        }
        assert_eq!(both, 95 + BOTH.len());
        // no character of the font is in the table twice, nor any of Unicode
        for (at, (font, code)) in BOTH.iter().enumerate() {
            assert!(BOTH[..at].iter().all(|(other, number)| other != font && number != code), "{font:#x}");
        }
    }

    #[test]
    fn utf8_is_read_as_it_is_written_and_not_otherwise() {
        for letter in ['a', '\u{e9}', '\u{20ac}', '\u{1f600}', '\u{7ff}', '\u{800}', '\u{ffff}', '\u{10ffff}'] {
            let mut bytes = [0u8; 4];
            let length = letter.encode_utf8(&mut bytes).len();
            assert_eq!(utf8(letter as u32), (bytes, length), "{letter}");
            assert_eq!(read(&bytes[..length]), Some((letter as u32, length)));
        }
        // cut short, a second byte that is not one, a byte that starts nothing
        for broken in [&b"\xc3"[..], b"\xc3a", b"\xe2\x82", b"\x82", b"\xff\x80", b""] {
            assert_eq!(read(broken), None, "{broken:?}");
        }
    }
}
