//! A small JSON value, parser, and serializer, with no external crate.
//!
//! `picklejar` stores a `JSON` column as validated text and navigates it with
//! the `->` / `->>` operators. This module parses JSON into a [`Json`] tree
//! (for validation and navigation) and serializes a sub-value back to compact
//! text. It is deliberately minimal: enough for storage, access, and the
//! differential needs of the engine, not a spec-perfect implementation.

use std::fmt::Write as _;

/// A parsed JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    /// `null`.
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// A number (stored as `f64`; integers print without a fraction).
    Num(f64),
    /// A string.
    Str(String),
    /// An array.
    Arr(Vec<Self>),
    /// An object, preserving member order.
    Obj(Vec<(String, Self)>),
}

impl Json {
    /// The object member named `key`, if this is an object that has it.
    #[must_use]
    pub fn get_key(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Obj(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The array element at `index` (negative counts from the end), if this is
    /// an array with that element.
    #[must_use]
    pub fn get_index(&self, index: i64) -> Option<&Self> {
        let Self::Arr(items) = self else {
            return None;
        };
        let i = if index < 0 {
            items
                .len()
                .checked_sub(usize::try_from(index.unsigned_abs()).ok()?)?
        } else {
            usize::try_from(index).ok()?
        };
        items.get(i)
    }

    /// The text form of a scalar for `->>`: a string yields its contents
    /// unquoted; everything else yields its compact JSON serialization.
    #[must_use]
    pub fn as_text(&self) -> String {
        match self {
            Self::Str(s) => s.clone(),
            other => to_string(other),
        }
    }
}

/// Parse `input` as a single JSON value, returning `None` if it is not valid
/// JSON (or has trailing content).
#[must_use]
pub fn parse(input: &str) -> Option<Json> {
    let mut p = Parser {
        bytes: input.as_bytes(),
        pos: 0,
    };
    p.skip_ws();
    let value = p.value()?;
    p.skip_ws();
    if p.pos == p.bytes.len() {
        Some(value)
    } else {
        None
    }
}

/// Whether `input` is valid JSON.
#[must_use]
pub fn is_valid(input: &str) -> bool {
    parse(input).is_some()
}

/// Serialize a [`Json`] to compact text (no insignificant whitespace).
#[must_use]
pub fn to_string(value: &Json) -> String {
    let mut out = String::new();
    write_json(&mut out, value);
    out
}

#[allow(clippy::cast_possible_truncation)]
fn write_json(out: &mut String, value: &Json) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Num(n) => {
            if n.fract() == 0.0 && n.is_finite() && n.abs() < 1e15 {
                let _ = write!(out, "{}", *n as i64);
            } else {
                let _ = write!(out, "{n}");
            }
        }
        Json::Str(s) => write_quoted(out, s),
        Json::Arr(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(out, item);
            }
            out.push(']');
        }
        Json::Obj(members) => {
            out.push('{');
            for (i, (k, v)) in members.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_quoted(out, k);
                out.push(':');
                write_json(out, v);
            }
            out.push('}');
        }
    }
}

fn write_quoted(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A byte cursor over the input.
struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn literal(&mut self, word: &[u8]) -> bool {
        if self.bytes[self.pos..].starts_with(word) {
            self.pos += word.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Option<Json> {
        self.skip_ws();
        match self.peek()? {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => Some(Json::Str(self.string()?)),
            b't' => self.literal(b"true").then_some(Json::Bool(true)),
            b'f' => self.literal(b"false").then_some(Json::Bool(false)),
            b'n' => self.literal(b"null").then_some(Json::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => None,
        }
    }

    fn object(&mut self) -> Option<Json> {
        self.pos += 1; // '{'
        let mut members = Vec::new();
        self.skip_ws();
        if self.eat(b'}') {
            return Some(Json::Obj(members));
        }
        loop {
            self.skip_ws();
            if self.peek()? != b'"' {
                return None;
            }
            let key = self.string()?;
            self.skip_ws();
            if !self.eat(b':') {
                return None;
            }
            let val = self.value()?;
            members.push((key, val));
            self.skip_ws();
            if self.eat(b',') {
                continue;
            }
            return self.eat(b'}').then_some(Json::Obj(members));
        }
    }

    fn array(&mut self) -> Option<Json> {
        self.pos += 1; // '['
        let mut items = Vec::new();
        self.skip_ws();
        if self.eat(b']') {
            return Some(Json::Arr(items));
        }
        loop {
            let val = self.value()?;
            items.push(val);
            self.skip_ws();
            if self.eat(b',') {
                continue;
            }
            return self.eat(b']').then_some(Json::Arr(items));
        }
    }

    fn string(&mut self) -> Option<String> {
        self.pos += 1; // opening quote
        let mut out = String::new();
        loop {
            let c = self.peek()?;
            self.pos += 1;
            match c {
                b'"' => return Some(out),
                b'\\' => {
                    let esc = self.peek()?;
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000c}'),
                        b'u' => out.push(self.unicode_escape()?),
                        _ => return None,
                    }
                }
                // A raw byte: collect the whole UTF-8 sequence.
                _ => {
                    let start = self.pos - 1;
                    while self.peek().is_some_and(|b| b & 0xC0 == 0x80) {
                        self.pos += 1;
                    }
                    out.push_str(std::str::from_utf8(&self.bytes[start..self.pos]).ok()?);
                }
            }
        }
    }

    fn unicode_escape(&mut self) -> Option<char> {
        let hex = self.bytes.get(self.pos..self.pos + 4)?;
        self.pos += 4;
        let code = u32::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
        char::from_u32(code)
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while matches!(
            self.peek(),
            Some(b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
        ) {
            self.pos += 1;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos]).ok()?;
        text.parse::<f64>().ok().map(Json::Num)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_reserializes() {
        let j = parse(r#"{"a": 1, "b": [true, null, "x"], "c": {"d": 2.5}}"#).expect("parse");
        assert_eq!(
            to_string(&j),
            r#"{"a":1,"b":[true,null,"x"],"c":{"d":2.5}}"#
        );
    }

    #[test]
    fn navigates_keys_and_indexes() {
        let j = parse(r#"{"name": "ada", "tags": ["a", "b", "c"]}"#).expect("parse");
        assert_eq!(j.get_key("name").map(Json::as_text).as_deref(), Some("ada"));
        let tags = j.get_key("tags").expect("tags");
        assert_eq!(tags.get_index(0).map(Json::as_text).as_deref(), Some("a"));
        assert_eq!(tags.get_index(-1).map(Json::as_text).as_deref(), Some("c"));
        assert_eq!(tags.get_index(9), None);
        assert_eq!(j.get_key("missing"), None);
    }

    #[test]
    fn rejects_invalid() {
        assert!(!is_valid("{"));
        assert!(!is_valid(r#"{"a": }"#));
        assert!(!is_valid("[1, 2,]"));
        assert!(!is_valid("nul"));
        assert!(!is_valid(r#"{"a": 1} junk"#));
        assert!(is_valid("  [1, 2, 3]  "));
        assert!(is_valid("\"escaped \\\" quote\""));
    }

    #[test]
    fn as_text_unquotes_strings_only() {
        assert_eq!(Json::Str("hi".into()).as_text(), "hi");
        assert_eq!(Json::Num(5.0).as_text(), "5");
        assert_eq!(Json::Bool(true).as_text(), "true");
        assert_eq!(parse("[1,2]").unwrap().as_text(), "[1,2]");
    }
}
