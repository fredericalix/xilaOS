//! JSON: the text services of the Internet speak, written and read.
//!
//! A question to a model is JSON and so is its answer: values that are a
//! text between quotes, a number, `true`, `false`, `null`, a list between
//! `[` and `]`, or named values between `{` and `}`.
//!
//! Nothing here builds a tree, since there is no heap to build it in.
//! Writing goes straight into a buffer (`Writer`). Reading works on the
//! bytes as they are: `get` finds the value under a name, `item` the nth
//! of a list, and both give back the slice of the text that holds it;
//! `text` then writes what a text says without its quotes and its marks.
//!
//! Text in XilaOS is bytes of the font's own set, plain ASCII below 128.
//! Whatever is not plain ASCII becomes `?` in both directions: a model is
//! told to write plain ASCII.

/// Writes JSON into a buffer. What does not fit is dropped and counted:
/// `whole` then says the text is not to be used.
pub struct Writer<'a> {
    bytes: &'a mut [u8],
    len: usize,
    lost: usize,
}

impl<'a> Writer<'a> {
    pub fn new(bytes: &'a mut [u8]) -> Writer<'a> {
        Writer { bytes, len: 0, lost: 0 }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether everything that was written fitted.
    pub fn whole(&self) -> bool {
        self.lost == 0
    }

    /// Goes back to a length it had: what was written since is taken out.
    pub fn back_to(&mut self, len: usize) {
        (self.len, self.lost) = (len.min(self.len), 0);
    }

    fn byte(&mut self, byte: u8) {
        match self.bytes.get_mut(self.len) {
            Some(place) => {
                *place = byte;
                self.len += 1;
            }
            None => self.lost += 1,
        }
    }

    /// JSON as it is: names, brackets, commas.
    pub fn raw(&mut self, json: &[u8]) {
        for &byte in json {
            self.byte(byte);
        }
    }

    /// A number.
    pub fn number(&mut self, mut number: u64) {
        let mut digits = [0u8; 20];
        let mut at = digits.len();
        loop {
            at -= 1;
            digits[at] = b'0' + (number % 10) as u8;
            number /= 10;
            if number == 0 {
                break;
            }
        }
        self.raw(&digits[at..]);
    }

    /// The inside of a text, without its quotes: several calls make one text.
    pub fn inside(&mut self, text: &[u8]) {
        for &byte in text {
            match byte {
                b'"' => self.raw(b"\\\""),
                b'\\' => self.raw(b"\\\\"),
                b'\n' => self.raw(b"\\n"),
                b'\r' => self.raw(b"\\r"),
                b'\t' => self.raw(b"\\t"),
                0x20..=0x7e => self.byte(byte),
                // what is not plain ASCII would not be read as it is meant
                _ => self.byte(b'?'),
            }
        }
    }

    /// A text between its quotes.
    pub fn text(&mut self, text: &[u8]) {
        self.byte(b'"');
        self.inside(text);
        self.byte(b'"');
    }
}

fn skip_space(json: &[u8], mut at: usize) -> usize {
    while json.get(at).is_some_and(|byte| byte.is_ascii_whitespace()) {
        at += 1;
    }
    at
}

/// Where the value that starts at `at` ends. None if it is not one.
fn end(json: &[u8], at: usize, depth: usize) -> Option<usize> {
    // lists in lists in lists: not without end
    if depth > 32 {
        return None;
    }
    match *json.get(at)? {
        b'"' => {
            let mut at = at + 1;
            loop {
                match *json.get(at)? {
                    b'"' => return Some(at + 1),
                    b'\\' => at += 2,
                    _ => at += 1,
                }
            }
        }
        open @ (b'{' | b'[') => {
            let close = if open == b'{' { b'}' } else { b']' };
            let mut at = skip_space(json, at + 1);
            if *json.get(at)? == close {
                return Some(at + 1);
            }
            loop {
                if open == b'{' {
                    // a name, then a colon
                    if *json.get(at)? != b'"' {
                        return None;
                    }
                    at = skip_space(json, end(json, at, depth + 1)?);
                    if *json.get(at)? != b':' {
                        return None;
                    }
                    at = skip_space(json, at + 1);
                }
                at = skip_space(json, end(json, at, depth + 1)?);
                match *json.get(at)? {
                    b',' => at = skip_space(json, at + 1),
                    byte if byte == close => return Some(at + 1),
                    _ => return None,
                }
            }
        }
        // a number, true, false, null: up to what ends it
        byte if byte.is_ascii_alphanumeric() || byte == b'-' => {
            let length = json[at..].iter().position(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'+' | b'.'))).unwrap_or(json.len() - at);
            let word = &json[at..at + length];
            // a word that is none of the three is a piece of one, or nothing
            (byte.is_ascii_digit() || byte == b'-' || matches!(word, b"true" | b"false" | b"null")).then_some(at + length)
        }
        _ => None,
    }
}

/// The value as it is written, without the space around it. None if what
/// is there is not one whole value.
pub fn value(json: &[u8]) -> Option<&[u8]> {
    let at = skip_space(json, 0);
    Some(&json[at..end(json, at, 0)?])
}

/// The value under a name, in named values (`{...}`).
pub fn get<'a>(json: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    let mut at = skip_space(json, 0);
    if *json.get(at)? != b'{' {
        return None;
    }
    at = skip_space(json, at + 1);
    while *json.get(at)? == b'"' {
        let named = end(json, at, 0)?;
        let found = &json[at + 1..named - 1] == name;
        at = skip_space(json, named);
        if *json.get(at)? != b':' {
            return None;
        }
        at = skip_space(json, at + 1);
        let after = end(json, at, 0)?;
        if found {
            return Some(&json[at..after]);
        }
        at = skip_space(json, after);
        if *json.get(at)? != b',' {
            return None;
        }
        at = skip_space(json, at + 1);
    }
    None
}

/// The value under a name, under a name...: `path(json, &[b"error", b"message"])`.
pub fn path<'a>(json: &'a [u8], names: &[&[u8]]) -> Option<&'a [u8]> {
    names.iter().try_fold(json, |json, name| get(json, name))
}

/// The nth value of a list (`[...]`), from 0.
pub fn item(json: &[u8], number: usize) -> Option<&[u8]> {
    let mut at = skip_space(json, 0);
    if *json.get(at)? != b'[' {
        return None;
    }
    at = skip_space(json, at + 1);
    for count in 0.. {
        if *json.get(at)? == b']' {
            return None;
        }
        let after = end(json, at, 0)?;
        if count == number {
            return Some(&json[at..after]);
        }
        at = skip_space(json, after);
        match *json.get(at)? {
            b',' => at = skip_space(json, at + 1),
            _ => return None,
        }
    }
    None
}

/// Whether a value is `null`, or is not there.
pub fn none(value: Option<&[u8]>) -> bool {
    value.is_none_or(|value| value == b"null")
}

/// What a text says: written into `into` without its quotes, with what
/// its marks stand for (`\n` a new line, `\"` a quote...). Returns how
/// many bytes, as many as fit; None if the value is not a text. What is
/// not plain ASCII becomes `?`, one for each character.
pub fn text(value: &[u8], into: &mut [u8]) -> Option<usize> {
    if value.len() < 2 || value[0] != b'"' || value[value.len() - 1] != b'"' {
        return None;
    }
    let inside = &value[1..value.len() - 1];
    let (mut at, mut len) = (0, 0);
    let mut put = |byte: u8, len: &mut usize| {
        if let Some(place) = into.get_mut(*len) {
            *place = byte;
            *len += 1;
        }
    };
    while at < inside.len() {
        let byte = inside[at];
        at += 1;
        match byte {
            b'\\' => {
                let mark = *inside.get(at)?;
                at += 1;
                match mark {
                    b'n' => put(b'\n', &mut len),
                    b't' => put(b'\t', &mut len),
                    b'r' => {}
                    b'b' | b'f' => put(b' ', &mut len),
                    // a character by its number, in four hexadecimal digits
                    b'u' => {
                        let digits = core::str::from_utf8(inside.get(at..at + 4)?).ok()?;
                        let number = u32::from_str_radix(digits, 16).ok()?;
                        at += 4;
                        match number {
                            // the second half of a character written in two halves: said already
                            0xdc00..=0xdfff => {}
                            0x20..=0x7e => put(number as u8, &mut len),
                            0x0a => put(b'\n', &mut len),
                            0x09 => put(b'\t', &mut len),
                            // quotes and dashes that have a plain form
                            0x2018 | 0x2019 => put(b'\'', &mut len),
                            0x201c | 0x201d => put(b'"', &mut len),
                            0x2013 | 0x2014 => put(b'-', &mut len),
                            _ => put(b'?', &mut len),
                        }
                    }
                    other => put(other, &mut len),
                }
            }
            0x20..=0x7e | b'\n' | b'\t' => put(byte, &mut len),
            // a character of several bytes (UTF-8): its first byte says how many
            0xc0..=0xff => {
                let more = if byte >= 0xf0 { 3 } else if byte >= 0xe0 { 2 } else { 1 };
                let whole = inside.get(at - 1..at + more).unwrap_or(&[]);
                at += more;
                put(
                    match whole {
                        // the quotes and dashes of typographers
                        [0xe2, 0x80, 0x98 | 0x99] => b'\'',
                        [0xe2, 0x80, 0x9c | 0x9d] => b'"',
                        [0xe2, 0x80, 0x93 | 0x94] => b'-',
                        _ => b'?',
                    },
                    &mut len,
                );
            }
            _ => put(b'?', &mut len),
        }
    }
    Some(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(write: impl FnOnce(&mut Writer)) -> String {
        let mut bytes = [0u8; 200];
        let mut writer = Writer::new(&mut bytes);
        write(&mut writer);
        assert!(writer.whole());
        let len = writer.len();
        String::from_utf8(bytes[..len].to_vec()).unwrap()
    }

    fn said(value: &[u8]) -> Option<String> {
        let mut into = [0u8; 200];
        let len = text(value, &mut into)?;
        Some(String::from_utf8(into[..len].to_vec()).unwrap())
    }

    #[test]
    fn a_text_is_written_with_marks_for_what_would_end_it() {
        assert_eq!(written(|w| w.text(b"hello")), r#""hello""#);
        assert_eq!(written(|w| w.text(b"say \"hi\"\nC:\\x\tend\r")), r#""say \"hi\"\nC:\\x\tend\r""#);
        // what is not plain ASCII would be read as something else
        assert_eq!(written(|w| w.text(b"caf\xe9 \x07 \x7f")), r#""caf? ? ?""#);
        assert_eq!(written(|w| w.text(b"")), r#""""#);
        // pieces: names and brackets as they are, texts and numbers in between
        assert_eq!(
            written(|w| {
                w.raw(b"{\"n\":");
                w.number(0);
                w.raw(b",\"m\":");
                w.number(18_446_744_073_709_551_615);
                w.raw(b",\"t\":\"");
                w.inside(b"a\"");
                w.inside(b"b");
                w.raw(b"\"}");
            }),
            r#"{"n":0,"m":18446744073709551615,"t":"a\"b"}"#
        );
    }

    #[test]
    fn what_does_not_fit_is_known_and_can_be_taken_back() {
        let mut bytes = [0u8; 8];
        let mut writer = Writer::new(&mut bytes);
        writer.raw(b"[1,");
        let kept = writer.len();
        writer.text(b"too long for it");
        assert!(!writer.whole() && writer.len() == 8);
        writer.back_to(kept);
        writer.raw(b"2]");
        assert!(writer.whole());
        let len = writer.len();
        assert_eq!(&bytes[..len], b"[1,2]");
    }

    #[test]
    fn a_value_is_found_by_its_name_and_by_its_place() {
        let json = br#"  {"id":"gen-1", "choices" : [ {"index":0,"message":{"role":"assistant","content":"Two tasks.","refusal":null}} , {"index":1} ],"usage":{"cost":0.00002,"n":-3e+2},"ok":true,"none":null,"empty":{},"list":[]}  "#;
        assert_eq!(get(json, b"id"), Some(&br#""gen-1""#[..]));
        assert_eq!(get(json, b"ok"), Some(&b"true"[..]));
        assert_eq!(path(json, &[b"usage", b"cost"]), Some(&b"0.00002"[..]));
        assert_eq!(path(json, &[b"usage", b"n"]), Some(&b"-3e+2"[..]));
        assert_eq!((get(json, b"empty"), get(json, b"list")), (Some(&b"{}"[..]), Some(&b"[]"[..])));
        let first = item(get(json, b"choices").unwrap(), 0).unwrap();
        assert_eq!(said(path(first, &[b"message", b"content"]).unwrap()).unwrap(), "Two tasks.");
        assert_eq!(item(get(json, b"choices").unwrap(), 1), Some(&br#"{"index":1}"#[..]));
        assert_eq!(item(get(json, b"choices").unwrap(), 2), None);
        assert_eq!(item(b"[]", 0), None);
        assert_eq!(item(b"[ 1 , [2,3] , \"]\" ]", 2), Some(&b"\"]\""[..]));
        // not there, null, or not what was expected
        assert!(none(get(json, b"missing")) && none(get(json, b"none")) && !none(get(json, b"ok")));
        assert!(none(path(first, &[b"message", b"refusal"])));
        assert_eq!((get(json, b"choice"), get(b"[1]", b"a"), item(json, 0), get(b"", b"a")), (None, None, None, None));
        // a name inside a text, or deeper, is not the name
        assert_eq!(get(br#"{"a":"\"b\":1","b":2}"#, b"b"), Some(&b"2"[..]));
        assert_eq!(get(br#"{"a":{"b":1},"b":2}"#, b"b"), Some(&b"2"[..]));
        assert_eq!(value(b"  [1, 2]  trailing"), Some(&b"[1, 2]"[..]));
    }

    #[test]
    fn what_is_cut_or_broken_is_not_read() {
        let whole = br#"{"a":[1,{"b":"c\"d"}],"e":null}"#;
        assert_eq!(get(whole, b"e"), Some(&b"null"[..]));
        // cut anywhere: what lies after the cut is never found, and nothing reads past the end
        for cut in 0..whole.len() - 1 {
            assert_eq!(get(&whole[..cut], b"e"), None, "cut at {cut}");
            let _ = (get(&whole[..cut], b"a"), value(&whole[..cut]), item(&whole[4..cut.max(4)], 1));
        }
        for broken in [&br#"{"a" 1}"#[..], br#"{"a":1 "b":2}"#, br#"{a:1}"#, br#"{"a":}"#, br#"[1 2]"#, br#"{"a":1,}"#, b"\"open", b"@"] {
            assert_eq!((get(broken, b"b"), item(broken, 1)), (None, None), "{:?}", core::str::from_utf8(broken));
        }
        // lists in lists without end
        let deep = [b'['; 100];
        assert_eq!(value(&deep), None);
    }

    #[test]
    fn a_text_is_read_back_as_what_it_says() {
        assert_eq!(said(br#""hello""#).unwrap(), "hello");
        assert_eq!(said(br#""say \"hi\"\nC:\\x\tend\r\/""#).unwrap(), "say \"hi\"\nC:\\x\tend/");
        assert_eq!(said(br#""""#).unwrap(), "");
        // characters by their number: plain ones as they are, the others as `?` or their plain form
        assert_eq!(said(br#""\u0041\u00e9\u2019\u201cq\u201d \u2014 \ud83d\ude00.""#).unwrap(), "A?'\"q\" - ?.");
        // characters of several bytes, as servers also write them
        assert_eq!(said("\"caf\u{e9} \u{2019}s \u{201c}q\u{201d} \u{2013} \u{1f600} \u{4e2d}.\"".as_bytes()).unwrap(), "caf? 's \"q\" - ? ?.");
        // what is not a text
        for other in [&b"12"[..], b"null", b"{}", b"\"", b"", b"\"a", b"\"\\u00zz\"", b"\"\\u00\"", b"\"\\"] {
            assert_eq!(said(other), None, "{:?}", core::str::from_utf8(other));
        }
        // more than there is room for: the start
        let mut small = [0u8; 4];
        assert_eq!((text(b"\"hello there\"", &mut small), &small), (Some(4), b"hell"));
        // a text that holds JSON holds it as a text: read once, it is JSON again
        let arguments = said(br#""{\"query\":\"select name from tasks where state = 'running'\"}""#).unwrap();
        assert_eq!(said(get(arguments.as_bytes(), b"query").unwrap()).unwrap(), "select name from tasks where state = 'running'");
        // what is written is read back the same
        for plain in ["", "a", "say \"hi\"\n\ttab \\ end", "{\"a\":[1,2]}"] {
            assert_eq!(said(written(|w| w.text(plain.as_bytes())).as_bytes()).unwrap(), plain);
        }
    }
}
