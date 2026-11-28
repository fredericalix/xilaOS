//! Fixed capacity byte strings.

use core::fmt;

/// A byte string of at most `N` bytes. Writes beyond the capacity are dropped.
#[derive(Clone, Copy)]
pub struct FixedStr<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> FixedStr<N> {
    pub const fn new() -> Self {
        FixedStr { buf: [0; N], len: 0 }
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut s = Self::new();
        s.push_bytes(bytes);
        s
    }

    /// Appends one byte. Returns false if the string is full.
    pub fn push(&mut self, byte: u8) -> bool {
        if self.len == N {
            return false;
        }
        self.buf[self.len] = byte;
        self.len += 1;
        true
    }

    /// The same text with more at the end.
    pub fn also(mut self, bytes: &[u8]) -> Self {
        self.push_bytes(bytes);
        self
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if !self.push(b) {
                break;
            }
        }
    }

    /// Inserts a byte at `at`. Returns false if the string is full.
    pub fn insert(&mut self, at: usize, byte: u8) -> bool {
        if self.len == N || at > self.len {
            return false;
        }
        self.buf.copy_within(at..self.len, at + 1);
        self.buf[at] = byte;
        self.len += 1;
        true
    }

    /// Removes the byte at `at`, if there is one.
    pub fn remove(&mut self, at: usize) {
        if at < self.len {
            self.buf.copy_within(at + 1..self.len, at);
            self.len -= 1;
        }
    }

    /// Turns ASCII small letters into capitals.
    pub fn upper(&mut self) {
        self.buf[..self.len].make_ascii_uppercase();
    }

    /// Makes the letters small.
    pub fn lower(&mut self) {
        let len = self.len;
        self.buf[..len].make_ascii_lowercase();
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }

    pub fn bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<const N: usize> PartialEq for FixedStr<N> {
    fn eq(&self, other: &Self) -> bool {
        self.bytes() == other.bytes()
    }
}

/// Shown as text, with characters outside ASCII as `?`.
/// Shows the text; characters outside ASCII become `?`.
impl<const N: usize> fmt::Display for FixedStr<N> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for &byte in self.bytes() {
            fmt::Write::write_char(f, if byte.is_ascii() { byte as char } else { '?' })?;
        }
        Ok(())
    }
}

impl<const N: usize> fmt::Debug for FixedStr<N> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("\"")?;
        for &b in self.bytes() {
            fmt::Write::write_char(f, if b.is_ascii() { b as char } else { '?' })?;
        }
        f.write_str("\"")
    }
}

impl<const N: usize> fmt::Write for FixedStr<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.push_bytes(s.as_bytes());
        Ok(())
    }
}

/// Removes spaces at both ends.
pub fn trim(mut bytes: &[u8]) -> &[u8] {
    while let [b' ', rest @ ..] = bytes {
        bytes = rest;
    }
    while let [rest @ .., b' '] = bytes {
        bytes = rest;
    }
    bytes
}

/// Splits at the first space: the first word, and the trimmed remainder.
/// Cuts a text into lines of `width` characters at most, at its line
/// breaks and at spaces, and calls `visit` with each.
pub fn wrap(text: &[u8], width: usize, visit: &mut dyn FnMut(&[u8])) {
    for mut line in text.split(|byte| *byte == b'\n') {
        loop {
            if line.len() <= width {
                visit(line);
                break;
            }
            // at the last space that fits, or where the width ends if there is none
            let cut = line[..width + 1].iter().rposition(|byte| *byte == b' ').filter(|cut| *cut > 0).unwrap_or(width);
            visit(line[..cut].trim_ascii_end());
            line = line[cut..].trim_ascii_start();
        }
    }
}

pub fn split_word(bytes: &[u8]) -> (&[u8], &[u8]) {
    let bytes = trim(bytes);
    match bytes.iter().position(|&b| b == b' ') {
        Some(i) => (&bytes[..i], trim(&bytes[i..])),
        None => (bytes, &[]),
    }
}

/// Parses a decimal number made of digits only.
pub fn parse_number(bytes: &[u8]) -> Option<usize> {
    if bytes.is_empty() || bytes.len() > 9 {
        return None;
    }
    let mut n = 0usize;
    for &b in bytes {
        if !b.is_ascii_digit() {
            return None;
        }
        n = n * 10 + (b - b'0') as usize;
    }
    Some(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::fmt::Write;

    #[test]
    fn fixed_str_drops_what_does_not_fit() {
        let mut s = FixedStr::<4>::from_bytes(b"abcdef");
        assert_eq!(s.bytes(), b"abcd");
        assert!(!s.push(b'x'));
        s.remove(1);
        assert_eq!(s.bytes(), b"acd");
        assert!(s.insert(0, b'z'));
        assert_eq!(s.bytes(), b"zacd");
    }

    #[test]
    fn fixed_str_formats_numbers() {
        let mut s = FixedStr::<16>::new();
        write!(s, "{} MB", 42).unwrap();
        assert_eq!(s.bytes(), b"42 MB");
    }

    #[test]
    fn words_and_numbers() {
        assert_eq!(split_word(b"  mood   calm "), (&b"mood"[..], &b"calm"[..]));
        assert_eq!(split_word(b"help"), (&b"help"[..], &b""[..]));
        assert_eq!(parse_number(b"12"), Some(12));
        assert_eq!(parse_number(b"1a"), None);
        assert_eq!(parse_number(b""), None);
    }
}
