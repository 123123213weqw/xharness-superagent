//! Validate the bounded JSON substring using an iterative, selective parser.
//! JSON.parse accepts lone UTF16 surrogate escapes and deep irrelevant values;
//! serde_json::Value's stricter strings/depth are not the same predicate. Only
//! root/error object type/code equality is retained; no body is rewritten.
#[derive(Clone, Copy, Default)]
struct Flags {
    kind: bool,
    code: bool,
}
#[derive(Clone, Copy, Default)]
struct Object {
    flags: Flags,
    error: Option<Flags>,
}
#[derive(Clone, Copy)]
enum Value {
    Other,
    BudgetString,
    Object(Object),
}
#[derive(Clone, Copy)]
enum Key {
    Other,
    Type,
    Code,
    Error,
}
enum Frame {
    Object { value: Object, key: Key, phase: u8 },
    Array { phase: u8 },
}
struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Parser<'_> {
    fn space(&mut self) {
        while self
            .bytes
            .get(self.at)
            .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
        {
            self.at += 1;
        }
    }
    fn take(&mut self, b: u8) -> bool {
        if self.bytes.get(self.at) == Some(&b) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn string(&mut self) -> Option<Option<String>> {
        if !self.take(b'"') {
            return None;
        }
        let mut ascii = Some(String::new());
        loop {
            let byte = *self.bytes.get(self.at)?;
            self.at += 1;
            let decoded = match byte {
                b'"' => return Some(ascii),
                0..=31 => return None,
                b'\\' => {
                    let escape = *self.bytes.get(self.at)?;
                    self.at += 1;
                    match escape {
                        b'"' | b'\\' | b'/' => Some(escape),
                        b'b' => Some(8),
                        b'f' => Some(12),
                        b'n' => Some(10),
                        b'r' => Some(13),
                        b't' => Some(9),
                        b'u' => {
                            let raw = self.bytes.get(self.at..self.at + 4)?;
                            self.at += 4;
                            let mut unit = 0u16;
                            for b in raw {
                                unit = unit * 16
                                    + match b {
                                        b'0'..=b'9' => u16::from(*b - b'0'),
                                        b'a'..=b'f' => u16::from(*b - b'a' + 10),
                                        b'A'..=b'F' => u16::from(*b - b'A' + 10),
                                        _ => return None,
                                    };
                            }
                            if unit < 128 {
                                Some(unit as u8)
                            } else {
                                None
                            }
                        }
                        _ => return None,
                    }
                }
                128..=255 => None,
                _ => Some(byte),
            };
            if let Some(byte) = decoded {
                if let Some(text) = &mut ascii {
                    text.push(char::from(byte));
                }
            } else {
                ascii = None;
            }
        }
    }
    fn number(&mut self) -> Option<()> {
        self.take(b'-');
        if !self.take(b'0') {
            if !self
                .bytes
                .get(self.at)
                .is_some_and(|b| matches!(b, b'1'..=b'9'))
            {
                return None;
            }
            while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                self.at += 1;
            }
        }
        if self.take(b'.') {
            let start = self.at;
            while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                self.at += 1;
            }
            if self.at == start {
                return None;
            }
        }
        if self.take(b'e') || self.take(b'E') {
            if !self.take(b'+') {
                self.take(b'-');
            }
            let start = self.at;
            while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                self.at += 1;
            }
            if self.at == start {
                return None;
            }
        }
        Some(())
    }
}
fn accept(stack: &mut [Frame], value: Value, root: &mut Option<Value>) -> Option<()> {
    match stack.last_mut() {
        Some(Frame::Object {
            value: object,
            key,
            phase,
        }) if *phase == 3 => {
            match key {
                Key::Type => object.flags.kind = matches!(value, Value::BudgetString),
                Key::Code => object.flags.code = matches!(value, Value::BudgetString),
                Key::Error => {
                    object.error = if let Value::Object(o) = value {
                        Some(o.flags)
                    } else {
                        None
                    }
                }
                Key::Other => {}
            }
            *phase = 4;
        }
        Some(Frame::Array { phase }) if *phase <= 1 => *phase = 2,
        None if root.is_none() => *root = Some(value),
        _ => return None,
    }
    Some(())
}
fn parse(text: &str) -> Option<Value> {
    let mut p = Parser {
        bytes: text.as_bytes(),
        at: 0,
    };
    let mut stack = Vec::new();
    let mut root = None;
    loop {
        p.space();
        if stack.is_empty() && root.is_some() {
            return (p.at == p.bytes.len()).then_some(root).flatten();
        }
        let mut complete = None;
        match stack.last_mut() {
            Some(Frame::Object { value, key, phase }) if *phase == 0 || *phase == 1 => {
                if *phase == 0 && p.take(b'}') {
                    complete = Some(Value::Object(*value));
                } else {
                    *key = match p.string()?.as_deref() {
                        Some("type") => Key::Type,
                        Some("code") => Key::Code,
                        Some("error") => Key::Error,
                        _ => Key::Other,
                    };
                    *phase = 2;
                    continue;
                }
            }
            Some(Frame::Object { phase, .. }) if *phase == 2 => {
                if !p.take(b':') {
                    return None;
                }
                *phase = 3;
                continue;
            }
            Some(Frame::Object { value, phase, .. }) if *phase == 4 => {
                if p.take(b',') {
                    *phase = 1;
                    continue;
                }
                if !p.take(b'}') {
                    return None;
                }
                complete = Some(Value::Object(*value));
            }
            Some(Frame::Array { phase }) if *phase == 2 => {
                if p.take(b',') {
                    *phase = 1;
                    continue;
                }
                if !p.take(b']') {
                    return None;
                }
                complete = Some(Value::Other);
            }
            Some(Frame::Array { phase }) if *phase == 0 && p.take(b']') => {
                complete = Some(Value::Other)
            }
            _ => {}
        }
        if let Some(value) = complete {
            stack.pop();
            accept(&mut stack, value, &mut root)?;
            continue;
        }
        let value = match *p.bytes.get(p.at)? {
            b'{' => {
                p.at += 1;
                stack.push(Frame::Object {
                    value: Object::default(),
                    key: Key::Other,
                    phase: 0,
                });
                continue;
            }
            b'[' => {
                p.at += 1;
                stack.push(Frame::Array { phase: 0 });
                continue;
            }
            b'"' => {
                if p.string()?.as_deref() == Some("budget_exceeded") {
                    Value::BudgetString
                } else {
                    Value::Other
                }
            }
            b't' if p.bytes.get(p.at..p.at + 4) == Some(b"true") => {
                p.at += 4;
                Value::Other
            }
            b'f' if p.bytes.get(p.at..p.at + 5) == Some(b"false") => {
                p.at += 5;
                Value::Other
            }
            b'n' if p.bytes.get(p.at..p.at + 4) == Some(b"null") => {
                p.at += 4;
                Value::Other
            }
            b'-' | b'0'..=b'9' => {
                p.number()?;
                Value::Other
            }
            _ => return None,
        };
        accept(&mut stack, value, &mut root)?;
    }
}
pub(super) fn matches(text: &str) -> bool {
    if let Some(Value::Object(object)) = parse(text) {
        let flags = object.error.unwrap_or(object.flags);
        flags.kind || flags.code
    } else {
        false
    }
}
