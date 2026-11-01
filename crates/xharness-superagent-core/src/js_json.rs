//! Identity-sensitive JSON without changing serde_json features for legacy crates.
//! Object enumeration and finite-number formatting follow the reference JS encoding.

use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum JsValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsValue>),
    Object(Vec<(String, JsValue)>),
}

impl JsValue {
    pub fn object(entries: impl IntoIterator<Item = (String, JsValue)>) -> Self {
        let mut result = Self::Object(Vec::new());
        for (key, value) in entries {
            result.set(key, value);
        }
        result
    }

    pub fn get(&self, key: &str) -> Option<&Self> {
        self.entries()?
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    pub fn set(&mut self, key: impl Into<String>, value: Self) {
        if let Self::Object(entries) = self {
            let key = key.into();
            if let Some((_, current)) = entries.iter_mut().find(|(name, _)| name == &key) {
                *current = value;
            } else {
                entries.push((key, value));
            }
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        if let Self::String(value) = self {
            Some(value)
        } else {
            None
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        if let Self::Bool(value) = self {
            Some(*value)
        } else {
            None
        }
    }
    pub fn as_number(&self) -> Option<f64> {
        if let Self::Number(value) = self {
            Some(*value)
        } else {
            None
        }
    }
    pub fn array(&self) -> Option<&[Self]> {
        if let Self::Array(value) = self {
            Some(value)
        } else {
            None
        }
    }
    pub fn entries(&self) -> Option<&[(String, Self)]> {
        if let Self::Object(value) = self {
            Some(value)
        } else {
            None
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(value) => *value,
            Self::Number(value) => *value != 0.0 && !value.is_nan(),
            Self::String(value) => !value.is_empty(),
            Self::Array(_) | Self::Object(_) => true,
        }
    }

    pub fn ordered_entries(&self) -> Vec<(&str, &Self)> {
        let mut entries: Vec<_> = self
            .entries()
            .unwrap_or_default()
            .iter()
            .enumerate()
            .collect();
        entries.sort_by_key(|(position, (key, _))| match array_index(key) {
            Some(index) => (0, u64::from(index)),
            None => (1, *position as u64),
        });
        entries
            .into_iter()
            .map(|(_, (key, value))| (key.as_str(), value))
            .collect()
    }

    pub fn stringify(&self) -> Result<String, serde_json::Error> {
        let mut output = String::new();
        self.write(&mut output)?;
        Ok(output)
    }

    fn write(&self, output: &mut String) -> Result<(), serde_json::Error> {
        match self {
            Self::Null => output.push_str("null"),
            Self::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
            Self::Number(value) => {
                if value.is_finite() {
                    output.push_str(ryu_js::Buffer::new().format(*value));
                } else {
                    output.push_str("null");
                }
            }
            Self::String(value) => output.push_str(&serde_json::to_string(value)?),
            Self::Array(values) => {
                output.push('[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        output.push(',');
                    }
                    value.write(output)?;
                }
                output.push(']');
            }
            Self::Object(_) => {
                output.push('{');
                for (index, (key, value)) in self.ordered_entries().iter().enumerate() {
                    if index > 0 {
                        output.push(',');
                    }
                    output.push_str(&serde_json::to_string(key)?);
                    output.push(':');
                    value.write(output)?;
                }
                output.push('}');
            }
        }
        Ok(())
    }

    /// Parse raw request text, preserving object order and JS binary64 number semantics.
    pub fn parse(text: &str) -> Result<Self, JsonError> {
        let mut parser = Parser { text, offset: 0 };
        let result = parser.value(0)?;
        parser.space();
        if parser.offset != text.len() {
            return Err(parser.error("Trailing JSON content"));
        }
        Ok(result)
    }

    pub fn from_serializable(value: &impl Serialize) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self::parse(&serde_json::to_string(value)?)?)
    }
}

impl From<&str> for JsValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}
impl From<String> for JsValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}
impl From<bool> for JsValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

fn array_index(value: &str) -> Option<u32> {
    let index: u32 = value.parse().ok()?;
    (index != u32::MAX && index.to_string() == value).then_some(index)
}

impl Serialize for JsValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => serializer.serialize_none(),
            Self::Bool(value) => serializer.serialize_bool(*value),
            Self::Number(value) => {
                if value.is_finite() {
                    serializer.serialize_f64(*value)
                } else {
                    serializer.serialize_none()
                }
            }
            Self::String(value) => serializer.serialize_str(value),
            Self::Array(values) => {
                let mut sequence = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    sequence.serialize_element(value)?;
                }
                sequence.end()
            }
            Self::Object(_) => {
                let entries = self.ordered_entries();
                let mut map = serializer.serialize_map(Some(entries.len()))?;
                for (key, value) in entries {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonError {
    pub offset: usize,
    pub message: String,
}
impl fmt::Display for JsonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} at byte {}", self.message, self.offset)
    }
}
impl std::error::Error for JsonError {}

struct Parser<'a> {
    text: &'a str,
    offset: usize,
}
impl Parser<'_> {
    fn error(&self, message: &str) -> JsonError {
        JsonError {
            offset: self.offset,
            message: message.to_owned(),
        }
    }
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.offset).copied()
    }
    fn space(&mut self) {
        while self
            .peek()
            .is_some_and(|value| matches!(value, b' ' | b'\t' | b'\n' | b'\r'))
        {
            self.offset += 1;
        }
    }
    fn consume(&mut self, value: u8) -> bool {
        self.space();
        if self.peek() == Some(value) {
            self.offset += 1;
            true
        } else {
            false
        }
    }
    fn string(&mut self) -> Result<String, JsonError> {
        self.space();
        let start = self.offset;
        if self.peek() != Some(b'"') {
            return Err(self.error("Expected JSON string"));
        }
        self.offset += 1;
        while let Some(value) = self.peek() {
            self.offset += 1;
            if value == b'\\' {
                if self.peek().is_none() {
                    break;
                }
                self.offset += 1;
            } else if value == b'"' {
                return serde_json::from_str(&self.text[start..self.offset])
                    .map_err(|error| self.error(&error.to_string()));
            }
        }
        Err(self.error("Unterminated JSON string"))
    }
    fn value(&mut self, depth: usize) -> Result<JsValue, JsonError> {
        if depth > 128 {
            return Err(self.error("JSON nesting limit exceeded"));
        }
        self.space();
        match self.peek() {
            Some(b'"') => self.string().map(JsValue::String),
            Some(b'{') => {
                self.offset += 1;
                let mut result = JsValue::Object(Vec::new());
                if self.consume(b'}') {
                    return Ok(result);
                }
                loop {
                    let key = self.string()?;
                    if !self.consume(b':') {
                        return Err(self.error("Expected colon"));
                    }
                    let value = self.value(depth + 1)?;
                    result.set(key, value);
                    if self.consume(b'}') {
                        return Ok(result);
                    }
                    if !self.consume(b',') {
                        return Err(self.error("Expected comma"));
                    }
                }
            }
            Some(b'[') => {
                self.offset += 1;
                let mut result = Vec::new();
                if self.consume(b']') {
                    return Ok(JsValue::Array(result));
                }
                loop {
                    result.push(self.value(depth + 1)?);
                    if self.consume(b']') {
                        return Ok(JsValue::Array(result));
                    }
                    if !self.consume(b',') {
                        return Err(self.error("Expected comma"));
                    }
                }
            }
            Some(b't' | b'f' | b'n') => {
                for (token, value) in [
                    ("true", JsValue::Bool(true)),
                    ("false", JsValue::Bool(false)),
                    ("null", JsValue::Null),
                ] {
                    if self.text[self.offset..].starts_with(token) {
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
    fn number(&mut self) -> Result<JsValue, JsonError> {
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
            let digits = self.offset;
            self.digits();
            if digits == self.offset {
                return Err(self.error("Missing fraction digits"));
            }
        }
        if self
            .peek()
            .is_some_and(|value| matches!(value, b'e' | b'E'))
        {
            self.offset += 1;
            if self
                .peek()
                .is_some_and(|value| matches!(value, b'+' | b'-'))
            {
                self.offset += 1;
            }
            let digits = self.offset;
            self.digits();
            if digits == self.offset {
                return Err(self.error("Missing exponent digits"));
            }
        }
        self.text[start..self.offset]
            .parse()
            .map(JsValue::Number)
            .map_err(|_| self.error("Invalid JSON number"))
    }
    fn digits(&mut self) {
        while self.peek().is_some_and(|value| value.is_ascii_digit()) {
            self.offset += 1;
        }
    }
}
