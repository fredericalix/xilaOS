//! QR codes: text as a square of dots that a phone reads off the screen.
//!
//! A machine without a network, a serial port or a driver for USB has one
//! way to give its text to another machine: its screen. A person can copy
//! a few words from it; a camera can copy three thousand characters, and
//! without a mistake, since the code carries what it takes to correct
//! errors and to know that none is left.
//!
//! This makes codes of the model 2 of the standard (ISO/IEC 18004), from
//! 21 dots a side (version 1) to 177 (version 40), with the text as plain
//! bytes and the lowest of the four levels of error correction: a screen
//! is a clean thing to photograph, and room for text is what matters.
//!
//! The steps, in `encode`:
//! 1. the bytes of the text, behind a header that says how many there are,
//!    filled up to what the version holds;
//! 2. cut in blocks, each followed by its correction bytes (Reed-Solomon),
//!    and the blocks woven together;
//! 3. the fixed patterns a reader finds the code by, drawn first, and the
//!    bits laid in a zigzag in the dots that are left;
//! 4. one of eight masks over the bits, the one that leaves the fewest
//!    shapes a reader could take for the fixed patterns.
//!
//! The tables and the rules are those of the standard, as the reference
//! implementation of Project Nayuki states them; what this draws is read
//! back by another program in `tools/check.py`.

/// Dots on a side of the largest code, and bytes to keep a bit per dot.
const SIDE: usize = 177;
const BITS: usize = (SIDE * SIDE).div_ceil(8);
/// Bytes a code holds at most, correction included.
const MOST_BYTES: usize = 3706;
/// Most characters of text in one code.
pub const MOST_TEXT: usize = 2953;

/// For each version, 1 to 40: correction bytes per block, and blocks.
const CORRECTION: [u8; 40] = [
    7, 10, 15, 20, 26, 18, 20, 24, 30, 18, 20, 24, 26, 30, 22, 24, 28, 30, 28, 28, 28, 28, 30, 30, 26, 28, 30, 30, 30, 30, 30, 30, 30, 30, 30,
    30, 30, 30, 30, 30,
];
const BLOCKS: [u8; 40] = [
    1, 1, 1, 1, 1, 2, 2, 2, 2, 4, 4, 4, 4, 4, 6, 6, 6, 6, 7, 8, 8, 9, 9, 10, 12, 12, 12, 13, 14, 15, 16, 17, 18, 19, 19, 20, 21, 22, 24, 25,
];

/// A square of dots, each dark or light.
#[derive(Clone, Copy)]
struct Square {
    size: usize,
    bits: [u8; BITS],
}

impl Square {
    const fn new(size: usize) -> Square {
        Square { size, bits: [0; BITS] }
    }

    fn get(&self, x: usize, y: usize) -> bool {
        let at = y * self.size + x;
        self.bits[at / 8] & 1 << (at % 8) != 0
    }

    fn set(&mut self, x: usize, y: usize, dark: bool) {
        let at = y * self.size + x;
        if dark {
            self.bits[at / 8] |= 1 << (at % 8);
        } else {
            self.bits[at / 8] &= !(1 << (at % 8));
        }
    }
}

/// A code, ready to be drawn.
#[derive(Clone, Copy)]
pub struct Code(Square);

impl Code {
    /// Dots on a side. A reader also wants four light dots all around.
    pub fn size(&self) -> usize {
        self.0.size
    }

    pub fn dark(&self, x: usize, y: usize) -> bool {
        self.0.get(x, y)
    }
}

/// Dots of a version that carry bits, once the fixed patterns are drawn.
fn free_dots(version: usize) -> usize {
    let mut dots = (16 * version + 128) * version + 64;
    if version >= 2 {
        let marks = version / 7 + 2;
        dots -= (25 * marks - 10) * marks - 55;
        if version >= 7 {
            dots -= 36;
        }
    }
    dots
}

/// Bytes of a version left for the header and the text.
fn data_bytes(version: usize) -> usize {
    free_dots(version) / 8 - CORRECTION[version - 1] as usize * BLOCKS[version - 1] as usize
}

/// Characters of text a version holds.
pub fn room(version: usize) -> usize {
    // the header: four bits for the kind of text, then its length on one byte or two
    data_bytes(version) - if version < 10 { 2 } else { 3 }
}

/// Multiplies two numbers of the field the correction works in.
fn times(x: u8, y: u8) -> u8 {
    let mut z = 0u8;
    for bit in (0..8).rev() {
        z = (z << 1) ^ ((z >> 7) * 0x1d);
        z ^= ((y >> bit) & 1) * x;
    }
    z
}

/// The polynomial that `count` correction bytes are the remainder by: its
/// coefficients from the highest power down, without the first, which is 1.
fn divisor(count: usize) -> [u8; 30] {
    let mut result = [0u8; 30];
    result[count - 1] = 1;
    let mut root = 1u8;
    for _ in 0..count {
        for at in 0..count {
            result[at] = times(result[at], root);
            if at + 1 < count {
                result[at] ^= result[at + 1];
            }
        }
        root = times(root, 2);
    }
    result
}

/// The correction bytes of a block.
fn correction(block: &[u8], divisor: &[u8]) -> [u8; 30] {
    let count = divisor.len();
    let mut result = [0u8; 30];
    for byte in block {
        let factor = byte ^ result[0];
        result.copy_within(1..count, 0);
        result[count - 1] = 0;
        for (kept, by) in result.iter_mut().zip(divisor) {
            *kept ^= times(*by, factor);
        }
    }
    result
}

/// The header, the text and the filling: the bytes of step 1.
fn data(version: usize, text: &[u8], into: &mut [u8]) {
    let into = &mut into[..data_bytes(version)];
    into.fill(0);
    let mut at = 0;
    let mut put = |value: usize, bits: usize| {
        for bit in (0..bits).rev() {
            into[at / 8] |= ((value >> bit & 1) as u8) << (7 - at % 8);
            at += 1;
        }
    };
    // bytes; how many; the bytes. What ends the text, four bits of nothing,
    // is already there, as is the rest of the last byte
    put(4, 4);
    put(text.len(), if version < 10 { 8 } else { 16 });
    for byte in text {
        put(*byte as usize, 8);
    }
    let used = (at + 4).div_ceil(8).min(into.len());
    for (filler, byte) in [0xec, 0x11].iter().cycle().zip(&mut into[used..]) {
        *byte = *filler;
    }
}

/// Cuts the data in blocks, works out the correction of each, and weaves
/// them: the first byte of every block, then the second of every block...
fn weave(version: usize, data: &[u8], into: &mut [u8]) -> usize {
    let (blocks, extra) = (BLOCKS[version - 1] as usize, CORRECTION[version - 1] as usize);
    let all = free_dots(version) / 8;
    // the last blocks are one byte longer than the first ones
    let (short, short_bytes) = (blocks - all % blocks, all / blocks - extra);
    let start = |block: usize| block * short_bytes + block.saturating_sub(short);
    let divisor = divisor(extra);
    let mut at = 0;
    for byte in 0..=short_bytes {
        for block in 0..blocks {
            if byte < short_bytes || block >= short {
                into[at] = data[start(block) + byte];
                at += 1;
            }
        }
    }
    for block in 0..blocks {
        let bytes = correction(&data[start(block)..start(block + 1)], &divisor[..extra]);
        for (byte, value) in bytes[..extra].iter().enumerate() {
            into[at + byte * blocks + block] = *value;
        }
    }
    all
}

/// A code being drawn: its dots, and which of them are fixed patterns.
struct Drawing {
    dots: Square,
    fixed: Square,
}

impl Drawing {
    fn fix(&mut self, x: usize, y: usize, dark: bool) {
        self.dots.set(x, y, dark);
        self.fixed.set(x, y, true);
    }

    /// Everything that does not depend on the text.
    fn patterns(&mut self, version: usize) {
        let size = self.dots.size;
        // the two dotted lines readers count the dots by
        for i in 0..size {
            self.fix(6, i, i % 2 == 0);
            self.fix(i, 6, i % 2 == 0);
        }
        // the three large corners, with a light border
        for (cx, cy) in [(3, 3), (size as i32 - 4, 3), (3, size as i32 - 4)] {
            for dy in -4..=4i32 {
                for dx in -4..=4i32 {
                    let (x, y) = (cx + dx, cy + dy);
                    if (0..size as i32).contains(&x) && (0..size as i32).contains(&y) {
                        let ring = dx.abs().max(dy.abs());
                        self.fix(x as usize, y as usize, ring != 2 && ring != 4);
                    }
                }
            }
        }
        // the small marks spread over the larger codes
        let mut places = [6usize; 7];
        let marks = if version == 1 { 0 } else { version / 7 + 2 };
        if marks > 0 {
            let step = (version * 8 + marks * 3 + 5) / (marks * 4 - 4) * 2;
            for i in 0..marks - 1 {
                places[marks - 1 - i] = size - 7 - i * step;
            }
        }
        for i in 0..marks {
            for j in 0..marks {
                // not on the three corners
                if (i == 0 && j == 0) || (i == 0 && j == marks - 1) || (i == marks - 1 && j == 0) {
                    continue;
                }
                for dy in 0..5usize {
                    for dx in 0..5usize {
                        let ring = dx.abs_diff(2).max(dy.abs_diff(2));
                        self.fix(places[i] + dx - 2, places[j] + dy - 2, ring != 1);
                    }
                }
            }
        }
        // where the mask will be written, kept out of the way of the bits
        self.mask_name(0);
        // the version, twice, in the larger codes
        if version >= 7 {
            let mut rest = version as u32;
            for _ in 0..12 {
                rest = (rest << 1) ^ ((rest >> 11) * 0x1f25);
            }
            let bits = (version as u32) << 12 | rest;
            for i in 0..18 {
                let dark = bits >> i & 1 != 0;
                let (a, b) = (size - 11 + i % 3, i / 3);
                self.fix(a, b, dark);
                self.fix(b, a, dark);
            }
        }
    }

    /// Writes which mask is used and the level of correction, twice, with
    /// correction bits of their own.
    fn mask_name(&mut self, mask: u32) {
        let size = self.dots.size;
        // 1: the lowest level of correction
        let name = 1 << 3 | mask;
        let mut rest = name;
        for _ in 0..10 {
            rest = (rest << 1) ^ ((rest >> 9) * 0x537);
        }
        let bits = (name << 10 | rest) ^ 0x5412;
        let bit = |i: usize| bits >> i & 1 != 0;
        for i in 0..6 {
            self.fix(8, i, bit(i));
        }
        self.fix(8, 7, bit(6));
        self.fix(8, 8, bit(7));
        self.fix(7, 8, bit(8));
        for i in 9..15 {
            self.fix(14 - i, 8, bit(i));
        }
        for i in 0..8 {
            self.fix(size - 1 - i, 8, bit(i));
        }
        for i in 8..15 {
            self.fix(8, size - 15 + i, bit(i));
        }
        // one dot that is always dark
        self.fix(8, size - 8, true);
    }

    /// Lays the bits in the free dots: two columns at a time from the
    /// right, going up, then down, then up.
    fn lay(&mut self, bytes: &[u8]) {
        let size = self.dots.size;
        let mut at = 0;
        let mut right = size as i32 - 1;
        while right >= 1 {
            if right == 6 {
                // the dotted line takes a whole column
                right = 5;
            }
            for step in 0..size {
                for side in 0..2 {
                    let x = (right - side) as usize;
                    let up = (right + 1) & 2 == 0;
                    let y = if up { size - 1 - step } else { step };
                    if !self.fixed.get(x, y) && at < bytes.len() * 8 {
                        self.dots.set(x, y, bytes[at / 8] >> (7 - at % 8) & 1 != 0);
                        at += 1;
                    }
                }
            }
            right -= 2;
        }
    }

    /// Turns over the bits a mask covers. Doing it twice undoes it.
    fn mask(&mut self, mask: u32) {
        let size = self.dots.size;
        for y in 0..size {
            for x in 0..size {
                let covered = match mask {
                    0 => (x + y) % 2 == 0,
                    1 => y % 2 == 0,
                    2 => x % 3 == 0,
                    3 => (x + y) % 3 == 0,
                    4 => (x / 3 + y / 2) % 2 == 0,
                    5 => x * y % 2 + x * y % 3 == 0,
                    6 => (x * y % 2 + x * y % 3) % 2 == 0,
                    _ => ((x + y) % 2 + x * y % 3) % 2 == 0,
                };
                if covered && !self.fixed.get(x, y) {
                    self.dots.set(x, y, !self.dots.get(x, y));
                }
            }
        }
    }

    /// How hard the code is to read, by the four counts of the standard:
    /// long runs of one color, squares of one color, shapes like the
    /// corners, and more dark than light or the reverse.
    fn trouble(&self) -> u32 {
        let size = self.dots.size;
        let mut result = 0;
        for across in [true, false] {
            for line in 0..size {
                let dot = |i: usize| if across { self.dots.get(i, line) } else { self.dots.get(line, i) };
                let (mut color, mut run) = (false, 0usize);
                let mut runs = Runs { size, last: [0; 7] };
                for i in 0..size {
                    if dot(i) == color {
                        run += 1;
                        if run == 5 {
                            result += 3;
                        } else if run > 5 {
                            result += 1;
                        }
                    } else {
                        runs.add(run);
                        if !color {
                            result += runs.corners() * 40;
                        }
                        color = dot(i);
                        run = 1;
                    }
                }
                // the end of the line, and the light border after it
                if color {
                    runs.add(run);
                    run = 0;
                }
                runs.add(run + size);
                result += runs.corners() * 40;
            }
        }
        let mut dark = 0;
        for y in 0..size {
            for x in 0..size {
                let color = self.dots.get(x, y);
                dark += color as usize;
                if x + 1 < size && y + 1 < size && color == self.dots.get(x + 1, y) && color == self.dots.get(x, y + 1) && color == self.dots.get(x + 1, y + 1) {
                    result += 3;
                }
            }
        }
        // how far from half dark, by steps of 5 %
        let total = size * size;
        let steps = ((dark * 20).abs_diff(total * 10) + total - 1) / total - 1;
        result + steps as u32 * 10
    }
}

/// The last runs of one color along a line, the newest first, to spot
/// the shape of a corner: dark, light, three dark, light, dark.
struct Runs {
    size: usize,
    last: [usize; 7],
}

impl Runs {
    fn add(&mut self, mut run: usize) {
        if self.last[0] == 0 {
            // the first run of a line has the light border before it
            run += self.size;
        }
        self.last.copy_within(0..6, 1);
        self.last[0] = run;
    }

    /// Called after a light run: how many corner shapes end there, with
    /// enough light on one side.
    fn corners(&self) -> u32 {
        let r = &self.last;
        let n = r[1];
        let core = n > 0 && r[2] == n && r[3] == n * 3 && r[4] == n && r[5] == n;
        (core && r[0] >= n * 4 && r[6] >= n) as u32 + (core && r[6] >= n * 4 && r[0] >= n) as u32
    }
}

/// Makes the code of a text, the smallest that holds it, of version
/// `most` at most. None if the text is longer than `room(most)`.
pub fn encode(text: &[u8], most: usize) -> Option<Code> {
    let version = (1..=most.min(40)).find(|version| room(*version) >= text.len())?;
    let (mut plain, mut woven) = ([0u8; MOST_BYTES], [0u8; MOST_BYTES]);
    data(version, text, &mut plain);
    let all = weave(version, &plain, &mut woven);

    let size = 17 + 4 * version;
    let mut drawing = Drawing { dots: Square::new(size), fixed: Square::new(size) };
    drawing.patterns(version);
    drawing.lay(&woven[..all]);
    let mut best = (0, u32::MAX);
    for mask in 0..8 {
        drawing.mask(mask);
        drawing.mask_name(mask);
        let trouble = drawing.trouble();
        if trouble < best.1 {
            best = (mask, trouble);
        }
        drawing.mask(mask);
    }
    drawing.mask(best.0);
    drawing.mask_name(best.0);
    Some(Code(drawing.dots))
}

/// Light dots a reader wants around a code.
const BORDER: usize = 4;

/// Paints a code in the middle of a picture of `width` colors per line:
/// black on white, as large as a whole number of points per dot allows,
/// on `paper`. Returns that number; 0 if the picture is too small, and
/// then it is all paper.
pub fn paint(code: &Code, picture: &mut [u32], width: usize, paper: u32) -> usize {
    picture.fill(paper);
    let height = picture.len() / width;
    let side = code.size() + 2 * BORDER;
    let points = (width.min(height) / side).min(8);
    let (left, top) = ((width - side * points) / 2, (height - side * points) / 2);
    for y in 0..side * points {
        for x in 0..side * points {
            let (dot_x, dot_y) = ((x / points).wrapping_sub(BORDER), (y / points).wrapping_sub(BORDER));
            let dark = dot_x < code.size() && dot_y < code.size() && code.dark(dot_x, dot_y);
            picture[(top + y) * width + left + x] = if dark { 0x000000 } else { 0xffffff };
        }
    }
    points
}

/// The largest version that `paint` gives at least two points per dot in
/// a picture of that many lines (and more across): smaller dots are hard
/// on a camera.
pub fn most_version(height: usize) -> usize {
    ((height / 2).saturating_sub(2 * BORDER + 17) / 4).clamp(1, 40)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_version_holds_what_the_standard_says() {
        // the table of the standard for the lowest level of correction, in bytes
        let known = [(1, 17), (2, 32), (3, 53), (4, 78), (5, 106), (6, 134), (7, 154), (8, 192), (9, 230), (10, 271), (20, 858), (30, 1732), (40, 2953)];
        for (version, bytes) in known {
            assert_eq!(room(version), bytes, "version {version}");
        }
        assert_eq!(free_dots(1) / 8, 26);
        assert_eq!(free_dots(40) / 8, MOST_BYTES);
        assert_eq!(room(40), MOST_TEXT);
        // the smallest version that holds the text is taken, up to the one allowed
        assert_eq!(encode(&[b'a'; 17], 40).unwrap().size(), 21);
        assert_eq!(encode(&[b'a'; 18], 40).unwrap().size(), 25);
        assert_eq!(encode(&[b'a'; 2953], 40).unwrap().size(), 177);
        assert!(encode(&[b'a'; 2954], 40).is_none());
        assert!(encode(&[b'a'; 18], 1).is_none());
        assert_eq!(encode(b"", 40).unwrap().size(), 21);
    }

    #[test]
    fn the_correction_bytes_make_a_word_of_the_code() {
        // a block with its correction is a multiple of the divisor: it is
        // zero at each of the powers of 2 the divisor was built from
        for (count, length) in [(7, 19), (18, 68), (30, 118)] {
            let block: Vec<u8> = (0..length).map(|i| (i * 37 + 11) as u8).collect();
            let extra = correction(&block, &divisor(count)[..count]);
            let word: Vec<u8> = block.iter().chain(&extra[..count]).copied().collect();
            let mut root = 1u8;
            for _ in 0..count {
                let value = word.iter().fold(0u8, |sum, byte| times(sum, root) ^ byte);
                assert_eq!(value, 0, "{count} correction bytes");
                root = times(root, 2);
            }
        }
        assert_eq!(times(0x53, 0xca), 0x8f);
        assert_eq!(times(2, 0x80), 0x1d);
    }

    /// The bits a reader would take from a code, in the order they were laid.
    fn read(code: &Code, version: usize) -> (u32, Vec<u8>) {
        let size = code.size();
        // which mask: the name near the first corner, without its own mask
        let mut name = 0u32;
        for (i, (x, y)) in [(8, 0), (8, 1), (8, 2), (8, 3), (8, 4), (8, 5), (8, 7), (8, 8), (7, 8), (5, 8), (4, 8), (3, 8), (2, 8), (1, 8), (0, 8)]
            .iter()
            .enumerate()
        {
            name |= (code.dark(*x, *y) as u32) << i;
        }
        let name = name ^ 0x5412;
        let mask = name >> 10 & 7;
        // the same code without text tells which dots are fixed
        let mut blank = Drawing { dots: Square::new(size), fixed: Square::new(size) };
        blank.patterns(version);
        let mut plain = Drawing { dots: code.0, fixed: blank.fixed };
        plain.mask(mask);
        let mut bytes = vec![0u8; free_dots(version) / 8];
        let mut at = 0;
        let mut right = size as i32 - 1;
        while right >= 1 {
            if right == 6 {
                right = 5;
            }
            for step in 0..size {
                for side in 0..2 {
                    let x = (right - side) as usize;
                    let y = if (right + 1) & 2 == 0 { size - 1 - step } else { step };
                    if !plain.fixed.get(x, y) && at < bytes.len() * 8 {
                        bytes[at / 8] |= (plain.dots.get(x, y) as u8) << (7 - at % 8);
                        at += 1;
                    }
                }
            }
            right -= 2;
        }
        (name, bytes)
    }

    #[test]
    fn a_code_reads_back() {
        for (version, length) in [(1, 17), (2, 20), (6, 120), (7, 150), (10, 260), (13, 400), (25, 1200), (35, 2303), (40, 2953)] {
            let text: Vec<u8> = (0..length).map(|i| b' ' + (i * 7 % 90) as u8).collect();
            let code = encode(&text, 40).unwrap();
            assert_eq!(code.size(), 17 + 4 * version, "{length} characters");
            let (name, woven) = read(&code, version);
            // the name of the mask says the lowest level of correction, and is a word of its own code
            assert_eq!(name >> 13, 1);
            let mut rest = name >> 10;
            for _ in 0..10 {
                rest = (rest << 1) ^ ((rest >> 9) * 0x537);
            }
            assert_eq!(rest & 0x3ff, name & 0x3ff);
            // unweave: byte i of block b
            let (blocks, extra) = (BLOCKS[version - 1] as usize, CORRECTION[version - 1] as usize);
            let (short, short_bytes) = (blocks - woven.len() % blocks, woven.len() / blocks - extra);
            let mut plain = vec![Vec::new(); blocks];
            let mut at = 0;
            for byte in 0..=short_bytes {
                for (block, bytes) in plain.iter_mut().enumerate() {
                    if byte < short_bytes || block >= short {
                        bytes.push(woven[at]);
                        at += 1;
                    }
                }
            }
            for _ in 0..extra {
                for bytes in plain.iter_mut() {
                    bytes.push(woven[at]);
                    at += 1;
                }
            }
            assert_eq!(at, woven.len());
            // every block is a word of the correcting code
            let mut all = Vec::new();
            for bytes in &plain {
                let mut root = 1u8;
                for _ in 0..extra {
                    assert_eq!(bytes.iter().fold(0u8, |sum, byte| times(sum, root) ^ byte), 0, "version {version}");
                    root = times(root, 2);
                }
                all.extend_from_slice(&bytes[..bytes.len() - extra]);
            }
            // and the text is behind its header
            let wide = version >= 10;
            let said = if wide { (all[0] as usize & 15) << 12 | (all[1] as usize) << 4 | all[2] as usize >> 4 } else { (all[0] as usize & 15) << 4 | all[1] as usize >> 4 };
            assert_eq!((all[0] >> 4, said), (4, length));
            let first = if wide { 2 } else { 1 };
            let found: Vec<u8> = (0..length).map(|i| all[first + i] << 4 | all[first + i + 1] >> 4).collect();
            assert!(found == text, "version {version}");
        }
    }

    #[test]
    fn a_code_is_painted_in_the_middle_of_a_picture() {
        let code = encode(b"XilaOS", 40).unwrap();
        let mut picture = vec![0u32; 640 * 352];
        // 21 dots and the border: 29, twelve times in 352 lines, but eight is plenty
        assert_eq!(paint(&code, &mut picture, 640, 0x123456), 8);
        let at = |x: usize, y: usize| picture[y * 640 + x];
        let (left, top) = ((640 - 29 * 8) / 2, (352 - 29 * 8) / 2);
        assert_eq!((at(0, 0), at(left - 1, top), at(left, top - 1)), (0x123456, 0x123456, 0x123456));
        // the border is white, the first dot of the corner black, on all its points
        assert_eq!((at(left, top), at(left + 31, top + 31)), (0xffffff, 0xffffff));
        assert_eq!((at(left + 32, top + 32), at(left + 39, top + 39), at(left + 40, top + 40)), (0, 0, 0xffffff));
        // the largest code that keeps two points per dot
        assert_eq!((most_version(352), most_version(336), most_version(272), most_version(40)), (37, 35, 27, 1));
        let large = encode(&[b'x'; 2300], most_version(336)).unwrap();
        assert_eq!(paint(&large, &mut picture[..640 * 336], 640, 0), 2);
        let larger = encode(&[b'x'; 2953], 40).unwrap();
        assert_eq!(paint(&larger, &mut picture[..640 * 336], 640, 0), 1);
        assert_eq!(paint(&larger, &mut picture[..640 * 100], 640, 7), 0);
        assert!(picture[..640 * 100].iter().all(|color| *color == 7));
    }

    #[test]
    fn the_fixed_patterns_are_where_readers_look() {
        let code = encode(b"XilaOS", 40).unwrap();
        let size = code.size();
        // a corner: dark ring, light ring, dark heart, and a light border
        let row = |y: usize| (0..8).map(|x| if code.dark(x, y) { '#' } else { '.' }).collect::<String>();
        assert_eq!([row(0), row(1), row(2), row(3), row(7)], ["#######.", "#.....#.", "#.###.#.", "#.###.#.", "........"]);
        assert!(code.dark(size - 1, 0) && code.dark(0, size - 1) && code.dark(size - 4, 3) && code.dark(3, size - 4));
        // the dotted lines, and the dot that is always dark
        assert!((8..size - 8).all(|i| code.dark(i, 6) == (i % 2 == 0) && code.dark(6, i) == (i % 2 == 0)));
        assert!(code.dark(8, size - 8));
        // version 7 writes its number: 000111110010010100, from the low bit up
        let seven = encode(&[b'x'; 150], 40).unwrap();
        assert_eq!(seven.size(), 45);
        let bits = (0..18).fold(0u32, |bits, i| bits | (seven.dark(45 - 11 + i % 3, i / 3) as u32) << i);
        assert_eq!(bits, 0x07c94);
        assert!((0..18).all(|i| seven.dark(i / 3, 45 - 11 + i % 3) == (bits >> i & 1 != 0)));
        // and has six small marks, the first of them centered on dot 22 of the line of dot 6
        assert!(seven.dark(22, 6) && !seven.dark(21, 5) && seven.dark(20, 4) && seven.dark(38, 38) && !seven.dark(37, 37));
    }
}
