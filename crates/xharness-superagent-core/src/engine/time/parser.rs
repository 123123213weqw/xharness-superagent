// Copyright 2011 the V8 project authors. All rights reserved.
// Rust adaptation of V8 13.6.233.10 src/date/dateparser{,-inl}.h and .cc.
// BSD-3-Clause notice: the crate NOTICE (V8 section).
// Grammar only; civil arithmetic and host-zone resolution are in the parent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    End,
    Unknown,
    Space,
    Number,
    Symbol,
    InvalidWord,
    Month,
    Zone,
    Separator,
    AmPm,
    Invalid,
}
#[derive(Clone, Copy, Debug)]
struct Token {
    kind: Kind,
    n: i32,
    len: usize,
}
impl Token {
    fn new(kind: Kind, n: i32, len: usize) -> Self {
        Self { kind, n, len }
    }
    fn symbol(self, ch: u8) -> bool {
        self.kind == Kind::Symbol && self.n == i32::from(ch)
    }
    fn fixed(self, len: usize) -> bool {
        self.kind == Kind::Number && self.len == len
    }
    fn sign(self) -> Option<i32> {
        if self.symbol(b'+') {
            Some(1)
        } else if self.symbol(b'-') {
            Some(-1)
        } else {
            None
        }
    }
    fn z(self) -> bool {
        self.kind == Kind::Zone && self.n == 0 && self.len == 1
    }
    fn keyword(self) -> bool {
        matches!(
            self.kind,
            Kind::InvalidWord | Kind::Month | Kind::Zone | Kind::Separator | Kind::AmPm
        )
    }
    fn ms(self) -> i32 {
        if self.len < 3 {
            self.n * 10_i32.pow((3 - self.len) as u32)
        } else {
            self.n / 10_i32.pow((self.len.min(9) - 3) as u32)
        }
    }
}
fn white(c: u16) -> bool {
    matches!(c,0x9..=0xD|0x20|0xA0|0x1680|0x2000..=0x200A|0x2028|0x2029|0x202F|0x205F|0x3000|0xFEFF)
}
fn word_space(c: u16) -> bool {
    white(c) && !matches!(c, 0xA | 0xD | 0x2028 | 0x2029)
}
fn digit(c: u16) -> bool {
    (u16::from(b'0')..=u16::from(b'9')).contains(&c)
}
struct Scanner {
    input: Vec<u16>,
    index: usize,
    peek: Token,
}
impl Scanner {
    fn new(s: &str) -> Self {
        let mut s = Self {
            input: s.encode_utf16().collect(),
            index: 0,
            peek: Token::new(Kind::End, 0, 0),
        };
        s.peek = s.scan();
        s
    }
    fn ch(&self) -> u16 {
        self.input.get(self.index).copied().unwrap_or(0)
    }
    fn scan(&mut self) -> Token {
        let start = self.index;
        let c = self.ch();
        if c == 0 {
            return Token::new(Kind::End, 0, 0);
        }
        if digit(c) {
            while self.ch() == u16::from(b'0') {
                self.index += 1;
            }
            let mut n = 0;
            let mut significant = 0;
            while digit(self.ch()) {
                if significant < 9 {
                    n = n * 10 + i32::from(self.ch() - u16::from(b'0'));
                }
                significant += 1;
                self.index += 1;
            }
            return Token::new(Kind::Number, n, self.index - start);
        }
        if b":-+.)".contains(&(c as u8)) && c < 128 {
            self.index += 1;
            return Token::new(Kind::Symbol, i32::from(c), 1);
        }
        if c >= u16::from(b'A') && !word_space(c) {
            let mut prefix = [0_u16; 3];
            while self.ch() >= u16::from(b'A') && !word_space(self.ch()) {
                let i = self.index - start;
                let c = self.ch();
                if i < 3 {
                    prefix[i] = if (u16::from(b'A')..=u16::from(b'Z')).contains(&c) {
                        c + 32
                    } else {
                        c
                    };
                }
                self.index += 1;
            }
            let len = self.index - start;
            let names = [
                ("jan", Kind::Month, 1),
                ("feb", Kind::Month, 2),
                ("mar", Kind::Month, 3),
                ("apr", Kind::Month, 4),
                ("may", Kind::Month, 5),
                ("jun", Kind::Month, 6),
                ("jul", Kind::Month, 7),
                ("aug", Kind::Month, 8),
                ("sep", Kind::Month, 9),
                ("oct", Kind::Month, 10),
                ("nov", Kind::Month, 11),
                ("dec", Kind::Month, 12),
                ("am", Kind::AmPm, 0),
                ("pm", Kind::AmPm, 12),
                ("ut", Kind::Zone, 0),
                ("utc", Kind::Zone, 0),
                ("z", Kind::Zone, 0),
                ("gmt", Kind::Zone, 0),
                ("cdt", Kind::Zone, -5),
                ("cst", Kind::Zone, -6),
                ("edt", Kind::Zone, -4),
                ("est", Kind::Zone, -5),
                ("mdt", Kind::Zone, -6),
                ("mst", Kind::Zone, -7),
                ("pdt", Kind::Zone, -7),
                ("pst", Kind::Zone, -8),
                ("t", Kind::Separator, 0),
            ];
            for (word, kind, n) in names {
                let mut expected = [0_u16; 3];
                for (i, c) in word.bytes().enumerate() {
                    expected[i] = u16::from(c);
                }
                if prefix == expected && (len <= 3 || kind == Kind::Month) {
                    return Token::new(kind, n, len);
                }
            }
            return Token::new(Kind::InvalidWord, 0, len);
        }
        self.index += 1;
        if white(c) {
            return Token::new(Kind::Space, 0, 1);
        }
        if c == u16::from(b'(') {
            let mut depth = 1;
            while self.ch() != 0 && depth > 0 {
                match self.ch() {
                    40 => depth += 1,
                    41 => depth -= 1,
                    _ => {}
                }
                self.index += 1;
            }
        }
        Token::new(Kind::Unknown, 0, 1)
    }
    fn next(&mut self) -> Token {
        let token = self.peek;
        self.peek = self.scan();
        token
    }
    fn skip(&mut self, ch: u8) -> bool {
        if self.peek.symbol(ch) {
            self.next();
            true
        } else {
            false
        }
    }
}
#[derive(Default)]
struct Day {
    values: Vec<i32>,
    month: Option<i32>,
    iso: bool,
}
impl Day {
    fn add(&mut self, n: i32) -> bool {
        if self.values.len() == 3 {
            false
        } else {
            self.values.push(n);
            true
        }
    }
    fn write(mut self) -> Option<(i32, i32, i32)> {
        if self.values.is_empty() {
            return None;
        }
        while self.values.len() < 3 {
            self.values.push(1)
        }
        let [a, b, c] = [self.values[0], self.values[1], self.values[2]];
        let (mut y, m, d) = if let Some(m) = self.month {
            if !(1..=31).contains(&a) {
                (a, m, b)
            } else {
                (b, m, a)
            }
        } else if self.iso || !(1..=31).contains(&a) {
            (a, b, c)
        } else {
            (c, a, b)
        };
        if !self.iso {
            if (0..=49).contains(&y) {
                y += 2000
            } else if (50..=99).contains(&y) {
                y += 1900
            }
        }
        if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
            None
        } else {
            Some((y, m, d))
        }
    }
}
#[derive(Default)]
struct Time {
    values: Vec<i32>,
    hour_offset: Option<i32>,
}
impl Time {
    fn add(&mut self, n: i32) -> bool {
        if self.values.len() == 4 {
            false
        } else {
            self.values.push(n);
            true
        }
    }
    fn final_value(&mut self, n: i32) -> bool {
        if !self.add(n) {
            return false;
        }
        self.values.resize(4, 0);
        true
    }
    fn expects(&self, n: i32) -> bool {
        match self.values.len() {
            1 | 2 => (0..=59).contains(&n),
            3 => (0..=999).contains(&n),
            _ => false,
        }
    }
    fn write(mut self) -> Option<i64> {
        self.values.resize(4, 0);
        let (mut h, m, s, ms) = (
            self.values[0],
            self.values[1],
            self.values[2],
            self.values[3],
        );
        if let Some(offset) = self.hour_offset {
            if !(0..=12).contains(&h) {
                return None;
            }
            h = h % 12 + offset;
        }
        if !((0..=23).contains(&h)
            && (0..=59).contains(&m)
            && (0..=59).contains(&s)
            && (0..=999).contains(&ms)
            || h == 24 && m == 0 && s == 0 && ms == 0)
        {
            return None;
        }
        Some(i64::from(h) * 3_600_000 + i64::from(m) * 60_000 + i64::from(s) * 1000 + i64::from(ms))
    }
}
#[derive(Default)]
struct Zone {
    sign: Option<i32>,
    hour: Option<i32>,
    minute: Option<i32>,
}
impl Zone {
    fn set(&mut self, h: i32) {
        self.sign = Some(if h < 0 { -1 } else { 1 });
        self.hour = Some(h.abs());
        self.minute = Some(0);
    }
    fn utc(&self) -> bool {
        self.hour == Some(0) && self.minute == Some(0)
    }
    fn expects(&self, n: i32) -> bool {
        self.hour.is_some() && self.minute.is_none() && (0..=59).contains(&n)
    }
    fn write(&self) -> Option<Option<i64>> {
        let Some(sign) = self.sign else {
            return Some(None);
        };
        let seconds =
            i64::from(self.hour.unwrap_or(0)) * 3600 + i64::from(self.minute.unwrap_or(0)) * 60;
        if seconds > 1_073_741_823 {
            None
        } else {
            Some(Some(seconds * i64::from(sign) * 1000))
        }
    }
}
fn iso(scanner: &mut Scanner, day: &mut Day, time: &mut Time, zone: &mut Zone) -> Token {
    let invalid = Token::new(Kind::Invalid, 0, 0);
    if let Some(sign) = scanner.peek.sign() {
        let token = scanner.next();
        if !scanner.peek.fixed(6) {
            return token;
        }
        let y = scanner.next().n;
        if sign < 0 && y == 0 {
            return token;
        }
        day.add(sign * y);
    } else if scanner.peek.fixed(4) {
        day.add(scanner.next().n);
    } else {
        return scanner.next();
    }
    if scanner.skip(b'-') {
        if !scanner.peek.fixed(2) || !(1..=12).contains(&scanner.peek.n) {
            return scanner.next();
        }
        day.add(scanner.next().n);
        if scanner.skip(b'-') {
            if !scanner.peek.fixed(2) || !(1..=31).contains(&scanner.peek.n) {
                return scanner.next();
            }
            day.add(scanner.next().n);
        }
    }
    if scanner.peek.kind != Kind::Separator {
        if scanner.peek.kind != Kind::End {
            return scanner.next();
        }
    } else {
        scanner.next();
        if !scanner.peek.fixed(2) || !(0..=24).contains(&scanner.peek.n) {
            return invalid;
        }
        let end_day = scanner.peek.n == 24;
        time.add(scanner.next().n);
        if !scanner.skip(b':')
            || !scanner.peek.fixed(2)
            || !(0..=59).contains(&scanner.peek.n)
            || end_day && scanner.peek.n > 0
        {
            return invalid;
        }
        time.add(scanner.next().n);
        if scanner.skip(b':') {
            if !scanner.peek.fixed(2)
                || !(0..=59).contains(&scanner.peek.n)
                || end_day && scanner.peek.n > 0
            {
                return invalid;
            }
            time.add(scanner.next().n);
            if scanner.skip(b'.') {
                if scanner.peek.kind != Kind::Number || end_day && scanner.peek.n > 0 {
                    return invalid;
                }
                time.add(scanner.next().ms());
            }
        }
        if scanner.peek.z() {
            scanner.next();
            zone.set(0);
        } else if let Some(sign) = scanner.peek.sign() {
            scanner.next();
            zone.sign = Some(sign);
            if scanner.peek.fixed(4) {
                let n = scanner.next().n;
                let (h, m) = (n / 100, n % 100);
                if !(0..=23).contains(&h) || !(0..=59).contains(&m) {
                    return invalid;
                }
                zone.hour = Some(h);
                zone.minute = Some(m);
            } else {
                if !scanner.peek.fixed(2) || !(0..=23).contains(&scanner.peek.n) {
                    return invalid;
                }
                zone.hour = Some(scanner.next().n);
                if !scanner.skip(b':')
                    || !scanner.peek.fixed(2)
                    || !(0..=59).contains(&scanner.peek.n)
                {
                    return invalid;
                }
                zone.minute = Some(scanner.next().n);
            }
        }
        if scanner.peek.kind != Kind::End {
            return invalid;
        }
    }
    if zone.hour.is_none() && time.values.is_empty() {
        zone.set(0)
    }
    day.iso = true;
    Token::new(Kind::End, 0, 0)
}
/// Returns a civil day, time-of-day millis and an optional UTC offset. Local
/// resolution is deliberately separate from the grammar and never guesses UTC.
type Parsed = ((i32, i32, i32), i64, Option<i64>);
pub(super) fn parse(input: &str) -> Option<Parsed> {
    let mut scanner = Scanner::new(input);
    let mut day = Day::default();
    let mut time = Time::default();
    let mut zone = Zone::default();
    let mut token = iso(&mut scanner, &mut day, &mut time, &mut zone);
    if token.kind == Kind::Invalid {
        return None;
    }
    let mut read_number = !day.values.is_empty();
    while token.kind != Kind::End {
        if token.kind == Kind::Number {
            let n = token.n;
            read_number = true;
            if scanner.skip(b':') {
                if scanner.skip(b':') {
                    if !time.values.is_empty() {
                        return None;
                    }
                    time.add(n);
                    time.add(0);
                } else {
                    if !time.add(n) {
                        return None;
                    }
                    if scanner.peek.symbol(b'.') {
                        scanner.next();
                    }
                }
            } else if scanner.skip(b'.') && time.expects(n) {
                time.add(n);
                if scanner.peek.kind != Kind::Number {
                    return None;
                }
                let ms = scanner.next().ms();
                if !time.final_value(ms) {
                    return None;
                }
            } else if zone.expects(n) {
                zone.minute = Some(n);
            } else if time.expects(n) {
                time.final_value(n);
                let peek = scanner.peek;
                if !matches!(peek.kind, Kind::End | Kind::Space)
                    && !peek.z()
                    && peek.sign().is_none()
                {
                    return None;
                }
            } else {
                if !day.add(n) {
                    return None;
                }
                scanner.skip(b'-');
            }
        } else if token.keyword() {
            match token.kind {
                Kind::AmPm if !time.values.is_empty() => time.hour_offset = Some(token.n),
                Kind::Month => {
                    day.month = Some(token.n);
                    scanner.skip(b'-');
                }
                Kind::Zone if read_number => zone.set(token.n),
                _ => {
                    if read_number || scanner.peek.kind == Kind::Number {
                        return None;
                    }
                }
            }
        } else if let Some(sign) = token
            .sign()
            .filter(|_| zone.utc() || !time.values.is_empty())
        {
            zone.sign = Some(sign);
            let next = if scanner.peek.kind == Kind::Number {
                scanner.next()
            } else {
                Token::new(Kind::Number, 0, 0)
            };
            let (n, len) = (next.n, next.len);
            read_number = true;
            if scanner.peek.symbol(b':') {
                zone.hour = Some(n);
                zone.minute = None;
            } else if len == 1 || len == 2 {
                zone.hour = Some(n);
                zone.minute = Some(0);
            } else if len == 3 || len == 4 {
                zone.hour = Some(n / 100);
                zone.minute = Some(n % 100);
            } else {
                return None;
            }
        } else if (token.sign().is_some() || token.symbol(b')')) && read_number {
            return None;
        }
        token = scanner.next();
    }
    Some((day.write()?, time.write()?, zone.write()?))
}
