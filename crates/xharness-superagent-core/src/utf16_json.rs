//! Lossless ECMAScript JSON for the skill protocol. Never funnels code units
//! through serde_json::Value or fabricates a Unicode replacement character.
//! Legacy JsValue/configuration users are deliberately not switched by this API.
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::let_underscore_must_use,
    unused_must_use
)]
#![cfg_attr(
    not(test),
    forbid(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::let_underscore_must_use,
        unused_must_use
    )
)]
use crate::js_json::JsonError;

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Text(pub Vec<u16>);
impl Text {
    pub fn scalar(value: &str) -> Self {
        Self(value.encode_utf16().collect())
    }
    pub fn is(&self, value: &str) -> bool {
        self.0.iter().copied().eq(value.encode_utf16())
    }
    pub fn as_scalar(&self) -> Result<String, std::string::FromUtf16Error> {
        String::from_utf16(&self.0)
    }
    pub fn push(&mut self, value: &str) {
        self.0.extend(value.encode_utf16());
    }
    pub fn append(&mut self, value: &Self) {
        self.0.extend_from_slice(&value.0);
    }
    pub fn escaped_text(&self) -> Self {
        let mut out = Self::default();
        for &unit in &self.0 {
            match unit {
                38 => out.push("&amp;"),
                60 => out.push("&lt;"),
                62 => out.push("&gt;"),
                _ => out.0.push(unit),
            }
        }
        out
    }
    pub fn quoted(&self) -> String {
        let mut out = String::from("\"");
        for decoded in char::decode_utf16(self.0.iter().copied()) {
            match decoded {
                Err(error) => push_hex(&mut out, error.unpaired_surrogate()),
                Ok(c) => match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\u{0008}' => out.push_str("\\b"),
                    '\t' => out.push_str("\\t"),
                    '\n' => out.push_str("\\n"),
                    '\u{000c}' => out.push_str("\\f"),
                    '\r' => out.push_str("\\r"),
                    c if c <= '\u{001f}' => push_hex(&mut out, c as u16),
                    c => out.push(c),
                },
            }
        }
        out.push('"');
        out
    }
}
fn push_hex(out: &mut String, unit: u16) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push_str("\\u");
    for shift in [12, 8, 4, 0] {
        out.push(char::from(HEX[usize::from((unit >> shift) & 15)]));
    }
}
/// The complete JSON value domain; even object keys may contain lone surrogates.
#[derive(Clone, Debug)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(Text),
    Array(Vec<Self>),
    Object(Vec<(Text, Self)>),
}
// JSON structural equality ignores object insertion order, as the SDK's
// isDeepEqualJson does. Serialization still observes ECMAScript key order.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Number(a), Self::Number(b)) => a == b,
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Array(a), Self::Array(b)) => a == b,
            (Self::Object(a), Self::Object(b)) => {
                a.len() == b.len()
                    && a.iter().all(|(key, value)| {
                        b.iter()
                            .find(|(k, _)| k == key)
                            .is_some_and(|(_, other)| value == other)
                    })
            }
            _ => false,
        }
    }
}
impl Value {
    pub fn object<'a>(values: impl IntoIterator<Item = (&'a str, Self)>) -> Self {
        let mut out = Self::Object(vec![]);
        for (k, v) in values {
            out.set(Text::scalar(k), v);
        }
        out
    }
    pub fn text(value: &str) -> Self {
        Self::String(Text::scalar(value))
    }
    pub fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Object(entries) => entries.iter().find(|(k, _)| k.is(key)).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn set(&mut self, key: Text, value: Self) {
        if let Self::Object(entries) = self {
            if let Some((_, old)) = entries.iter_mut().find(|(k, _)| *k == key) {
                *old = value;
            } else {
                entries.push((key, value));
            }
        }
    }
    pub fn string(&self) -> Option<&Text> {
        if let Self::String(s) = self {
            Some(s)
        } else {
            None
        }
    }
    pub fn array(&self) -> Option<&[Self]> {
        if let Self::Array(a) = self {
            Some(a)
        } else {
            None
        }
    }
    pub fn boolean(&self) -> Option<bool> {
        if let Self::Bool(b) = self {
            Some(*b)
        } else {
            None
        }
    }
    pub fn integer(&self) -> Option<u64> {
        if let Self::Number(n) = self {
            if n.is_finite() && *n >= 0.0 && *n <= 9_007_199_254_740_991.0 && n.fract() == 0.0 {
                return Some(*n as u64);
            }
        }
        None
    }
    pub fn is_text(&self, text: &str) -> bool {
        self.string().is_some_and(|s| s.is(text))
    }
    pub fn parse(input: &str) -> Result<Self, JsonError> {
        let mut parser = Parser { input, offset: 0 };
        let value = parser.value(0)?;
        parser.space();
        if parser.offset != input.len() {
            return Err(parser.error("Trailing JSON content"));
        }
        Ok(value)
    }
    pub fn stringify(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }
    fn write(&self, out: &mut String) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Self::Number(n) => {
                if n.is_finite() {
                    out.push_str(ryu_js::Buffer::new().format(*n));
                } else {
                    out.push_str("null");
                }
            }
            Self::String(s) => out.push_str(&s.quoted()),
            Self::Array(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Self::Object(entries) => {
                let mut sorted: Vec<_> = entries.iter().enumerate().collect();
                sorted.sort_by_key(|(position, (key, _))| {
                    array_index(key).map_or((1, *position as u64), |i| (0, u64::from(i)))
                });
                out.push('{');
                for (i, (_, (key, value))) in sorted.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&key.quoted());
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }
}
fn array_index(key: &Text) -> Option<u32> {
    if key.0.is_empty() || key.0.len() > 10 {
        return None;
    }
    if key.0.len() > 1 && key.0[0] == 48 {
        return None;
    }
    let mut value = 0u32;
    for &unit in &key.0 {
        if !(48..=57).contains(&unit) {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(u32::from(unit - 48))?;
    }
    (value != u32::MAX).then_some(value)
}
pub const fn js_space(unit: u16) -> bool {
    matches!(unit,0x0009..=0x000d|0x0020|0x00a0|0x1680|0x2000..=0x200a|0x2028|0x2029|0x202f|0x205f|0x3000|0xfeff)
}
struct Parser<'a> {
    input: &'a str,
    offset: usize,
}
impl Parser<'_> {
    fn error(&self, message: &str) -> JsonError {
        JsonError {
            offset: self.offset,
            message: message.into(),
        }
    }
    fn peek(&self) -> Option<u8> {
        self.input.as_bytes().get(self.offset).copied()
    }
    fn space(&mut self) {
        while self
            .peek()
            .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
        {
            self.offset += 1;
        }
    }
    fn consume(&mut self, byte: u8) -> bool {
        self.space();
        if self.peek() == Some(byte) {
            self.offset += 1;
            true
        } else {
            false
        }
    }
    fn string(&mut self) -> Result<Text, JsonError> {
        self.space();
        if self.peek() != Some(b'"') {
            return Err(self.error("Expected JSON string"));
        }
        self.offset += 1;
        let mut out = Text::default();
        loop {
            match self.peek() {
                None => return Err(self.error("Unterminated JSON string")),
                Some(b'"') => {
                    self.offset += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.offset += 1;
                    let escaped = match self.peek() {
                        Some(b'"') => 34,
                        Some(b'\\') => 92,
                        Some(b'/') => 47,
                        Some(b'b') => 8,
                        Some(b'f') => 12,
                        Some(b'n') => 10,
                        Some(b'r') => 13,
                        Some(b't') => 9,
                        Some(b'u') => {
                            self.offset += 1;
                            let mut unit = 0u16;
                            for _ in 0..4 {
                                let digit = match self.peek() {
                                    Some(b'0'..=b'9') => self.input.as_bytes()[self.offset] - b'0',
                                    Some(b'a'..=b'f') => {
                                        self.input.as_bytes()[self.offset] - b'a' + 10
                                    }
                                    Some(b'A'..=b'F') => {
                                        self.input.as_bytes()[self.offset] - b'A' + 10
                                    }
                                    _ => return Err(self.error("Invalid Unicode escape")),
                                };
                                unit = (unit << 4) | u16::from(digit);
                                self.offset += 1;
                            }
                            out.0.push(unit);
                            continue;
                        }
                        _ => return Err(self.error("Invalid JSON escape")),
                    };
                    out.0.push(escaped);
                    self.offset += 1;
                }
                Some(0..=31) => return Err(self.error("Unescaped JSON control character")),
                _ => {
                    let c = self.input[self.offset..]
                        .chars()
                        .next()
                        .ok_or_else(|| self.error("Unterminated JSON string"))?;
                    let mut buffer = [0; 2];
                    out.0.extend_from_slice(c.encode_utf16(&mut buffer));
                    self.offset += c.len_utf8();
                }
            }
        }
    }
    fn value(&mut self, depth: usize) -> Result<Value, JsonError> {
        if depth > 128 {
            return Err(self.error("JSON nesting limit exceeded"));
        }
        self.space();
        match self.peek() {
            Some(b'"') => self.string().map(Value::String),
            Some(b'{') => {
                self.offset += 1;
                let mut out = Value::Object(vec![]);
                if self.consume(b'}') {
                    return Ok(out);
                }
                loop {
                    let key = self.string()?;
                    if !self.consume(b':') {
                        return Err(self.error("Expected colon"));
                    }
                    let value = self.value(depth + 1)?;
                    out.set(key, value);
                    if self.consume(b'}') {
                        return Ok(out);
                    }
                    if !self.consume(b',') {
                        return Err(self.error("Expected comma"));
                    }
                }
            }
            Some(b'[') => {
                self.offset += 1;
                let mut out = vec![];
                if self.consume(b']') {
                    return Ok(Value::Array(out));
                }
                loop {
                    out.push(self.value(depth + 1)?);
                    if self.consume(b']') {
                        return Ok(Value::Array(out));
                    }
                    if !self.consume(b',') {
                        return Err(self.error("Expected comma"));
                    }
                }
            }
            Some(b't' | b'f' | b'n') => {
                for (token, value) in [
                    ("true", Value::Bool(true)),
                    ("false", Value::Bool(false)),
                    ("null", Value::Null),
                ] {
                    if self.input[self.offset..].starts_with(token) {
                        self.offset += token.len();
                        return Ok(value);
                    }
                }
                Err(self.error("Invalid JSON literal"))
            }
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(self.error("Expected JSON value")),
        }
    }
    fn number(&mut self) -> Result<Value, JsonError> {
        let start = self.offset;
        if self.peek() == Some(b'-') {
            self.offset += 1;
        }
        match self.peek() {
            Some(b'0') => self.offset += 1,
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(self.error("Invalid JSON number")),
        }
        if self.peek() == Some(b'.') {
            self.offset += 1;
            let start = self.offset;
            self.digits();
            if start == self.offset {
                return Err(self.error("Missing fraction digits"));
            }
        }
        if self.peek().is_some_and(|b| matches!(b, b'e' | b'E')) {
            self.offset += 1;
            if self.peek().is_some_and(|b| matches!(b, b'+' | b'-')) {
                self.offset += 1;
            }
            let start = self.offset;
            self.digits();
            if start == self.offset {
                return Err(self.error("Missing exponent digits"));
            }
        }
        self.input[start..self.offset]
            .parse::<f64>()
            .map(Value::Number)
            .map_err(|_| self.error("Invalid JSON number"))
    }
    fn digits(&mut self) {
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.offset += 1;
        }
    }
}
