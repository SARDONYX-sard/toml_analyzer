//! Classification and decoding of value tokens.

use std::borrow::Cow;

use crate::{LexError, TokenKind, is_date_prefix};

/// Classifies an unquoted value: a number, a boolean or a date-time.
pub(crate) fn classify(text: &str) -> (TokenKind, Option<LexError>) {
    match text {
        "true" | "false" => return (TokenKind::Bool, None),
        "inf" | "+inf" | "-inf" | "nan" | "+nan" | "-nan" => return (TokenKind::Float, None),
        _ => {}
    }
    let b = text.as_bytes();
    if is_date_prefix(text) {
        return date_time(text);
    }
    if b.len() >= 3 && b[0].is_ascii_digit() && b[1].is_ascii_digit() && b[2] == b':' {
        return match time(text) {
            Some((len, None)) if len == text.len() => (TokenKind::LocalTime, None),
            _ => (TokenKind::LocalTime, Some(LexError::InvalidTime)),
        };
    }
    if let Some(radix) = radix_prefix(text) {
        return (TokenKind::Integer, check_radix_integer(&text[2..], radix).err());
    }
    let first = b[0];
    if !(first.is_ascii_digit() || first == b'+' || first == b'-' || first == b'.') {
        return (TokenKind::Unknown, Some(LexError::InvalidValue));
    }
    match check_decimal(text) {
        Ok(kind) => (kind, None),
        Err((kind, e)) => (kind, Some(e)),
    }
}

fn radix_prefix(text: &str) -> Option<u32> {
    match text.as_bytes() {
        [b'0', b'x', ..] => Some(16),
        [b'0', b'o', ..] => Some(8),
        [b'0', b'b', ..] => Some(2),
        _ => None,
    }
}

/// Digits separated by single underscores.
fn check_digits(digits: &str, radix: u32) -> Result<(), LexError> {
    if digits.is_empty() {
        return Err(LexError::InvalidNumber);
    }
    let mut prev_digit = false;
    for c in digits.chars() {
        if c == '_' {
            if !prev_digit {
                return Err(LexError::InvalidUnderscore);
            }
            prev_digit = false;
        } else if c.is_digit(radix) {
            prev_digit = true;
        } else {
            return Err(LexError::InvalidNumber);
        }
    }
    if prev_digit { Ok(()) } else { Err(LexError::InvalidUnderscore) }
}

fn check_radix_integer(digits: &str, radix: u32) -> Result<(), LexError> {
    check_digits(digits, radix)?;
    let clean: String = digits.chars().filter(|&c| c != '_').collect();
    i64::from_str_radix(&clean, radix).map(|_| ()).map_err(|_| LexError::IntegerOverflow)
}

/// A decimal integer or a float.
fn check_decimal(text: &str) -> Result<TokenKind, (TokenKind, LexError)> {
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
        Some(i) => (&unsigned[..i], Some(&unsigned[i + 1..])),
        None => (unsigned, None),
    };
    let (int, frac) = match mantissa.find('.') {
        Some(i) => (&mantissa[..i], Some(&mantissa[i + 1..])),
        None => (mantissa, None),
    };
    let kind =
        if frac.is_some() || exponent.is_some() { TokenKind::Float } else { TokenKind::Integer };
    let fail = |e| Err((kind, e));
    if let Err(e) = check_digits(int, 10) {
        return fail(e);
    }
    if int.len() > 1 && int.starts_with('0') {
        return fail(LexError::LeadingZero);
    }
    if let Some(frac) = frac
        && let Err(e) = check_digits(frac, 10)
    {
        return fail(e);
    }
    if let Some(exp) = exponent {
        let exp = exp.strip_prefix(['+', '-']).unwrap_or(exp);
        if let Err(e) = check_digits(exp, 10) {
            return fail(e);
        }
    }
    if kind == TokenKind::Integer && parse_integer(text).is_none() {
        return fail(LexError::IntegerOverflow);
    }
    Ok(kind)
}

/// `1979-05-27`, optionally followed by a time and an offset.
fn date_time(text: &str) -> (TokenKind, Option<LexError>) {
    let b = text.as_bytes();
    let num = |r: std::ops::Range<usize>| text[r].parse::<u32>().unwrap_or(0);
    let (year, month, day) = (num(0..4), num(5..7), num(8..10));
    let date_ok = (1..=12).contains(&month) && day >= 1 && day <= days_in_month(year, month);
    let date_error = (!date_ok).then_some(LexError::InvalidDate);
    if b.len() == 10 {
        return (TokenKind::LocalDate, date_error);
    }
    if !matches!(b[10], b'T' | b't' | b' ') {
        return (TokenKind::LocalDate, Some(LexError::InvalidDate));
    }
    match time(&text[11..]) {
        Some((len, offset)) if 11 + len == text.len() => {
            let kind =
                if offset.is_some() { TokenKind::OffsetDateTime } else { TokenKind::LocalDateTime };
            (kind, date_error)
        }
        _ => (TokenKind::LocalDateTime, Some(LexError::InvalidTime)),
    }
}

/// Parses `HH:MM[:SS[.frac]]` and an optional offset at the start of `text`.
/// Returns the length read and the offset, if any.
fn time(text: &str) -> Option<(usize, Option<&str>)> {
    let b = text.as_bytes();
    let two = |i: usize| -> Option<u32> {
        let d = b.get(i..i + 2)?;
        d.iter()
            .all(u8::is_ascii_digit)
            .then(|| u32::from(d[0] - b'0') * 10 + u32::from(d[1] - b'0'))
    };
    let hour = two(0)?;
    if b.get(2) != Some(&b':') {
        return None;
    }
    let minute = two(3)?;
    let mut len = 5;
    if b.get(5) == Some(&b':') {
        let second = two(6)?;
        // 60: leap second.
        if second > 60 {
            return None;
        }
        len = 8;
        if b.get(8) == Some(&b'.') {
            let digits = b[9..].iter().take_while(|c| c.is_ascii_digit()).count();
            if digits == 0 {
                return None;
            }
            len = 9 + digits;
        }
    }
    if hour > 23 || minute > 59 {
        return None;
    }
    let rest = &text[len..];
    if rest.starts_with(['Z', 'z']) {
        return Some((len + 1, Some(&text[len..len + 1])));
    }
    if rest.starts_with(['+', '-']) {
        let rb = rest.as_bytes();
        let ok = rb.len() >= 6
            && rb[1..3].iter().all(u8::is_ascii_digit)
            && rb[3] == b':'
            && rb[4..6].iter().all(u8::is_ascii_digit);
        if !ok {
            return None;
        }
        let (oh, om) = (two(len + 1)?, two(len + 4)?);
        if oh > 23 || om > 59 {
            return None;
        }
        return Some((len + 6, Some(&text[len..len + 6])));
    }
    Some((len, None))
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        2 if (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400) => {
            29
        }
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The value of an integer token, `None` when it is malformed or too large.
pub fn parse_integer(text: &str) -> Option<i64> {
    let clean: String = text.chars().filter(|&c| c != '_').collect();
    if let Some(radix) = radix_prefix(&clean) {
        return i64::from_str_radix(&clean[2..], radix).ok();
    }
    clean.parse().ok()
}

/// The value of a float token, `None` when it is malformed.
pub fn parse_float(text: &str) -> Option<f64> {
    match text {
        "inf" | "+inf" => return Some(f64::INFINITY),
        "-inf" => return Some(f64::NEG_INFINITY),
        "nan" | "+nan" | "-nan" => return Some(f64::NAN),
        _ => {}
    }
    let clean: String = text.chars().filter(|&c| c != '_').collect();
    clean.parse().ok()
}

/// The value of a string token (of any [`TokenKind::is_string`] kind) or of
/// a bare key. Invalid escapes are kept as written.
pub fn unquote(kind: TokenKind, text: &str) -> Cow<'_, str> {
    match kind {
        TokenKind::BasicString => {
            let inner = strip(text, "\"", "\"");
            unescape(inner)
        }
        TokenKind::MultilineBasicString => {
            let inner = strip(text, "\"\"\"", "\"\"\"");
            unescape(trim_first_newline(inner))
        }
        TokenKind::LiteralString => Cow::Borrowed(strip(text, "'", "'")),
        TokenKind::MultilineLiteralString => {
            Cow::Borrowed(trim_first_newline(strip(text, "'''", "'''")))
        }
        _ => Cow::Borrowed(text),
    }
}

/// Removes the delimiters; an unterminated string keeps its end.
fn strip<'a>(text: &'a str, open: &str, close: &str) -> &'a str {
    let inner = text.strip_prefix(open).unwrap_or(text);
    if inner.len() >= close.len() { inner.strip_suffix(close).unwrap_or(inner) } else { inner }
}

fn trim_first_newline(s: &str) -> &str {
    s.strip_prefix("\r\n").or_else(|| s.strip_prefix('\n')).unwrap_or(s)
}

fn unescape(s: &str) -> Cow<'_, str> {
    if !s.contains('\\') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('\\') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let mut chars = rest[1..].chars();
        let Some(c) = chars.next() else {
            out.push('\\');
            break;
        };
        let simple = match c {
            'b' => Some('\u{8}'),
            't' => Some('\t'),
            'n' => Some('\n'),
            'f' => Some('\u{C}'),
            'r' => Some('\r'),
            'e' => Some('\u{1B}'),
            '"' => Some('"'),
            '\\' => Some('\\'),
            _ => None,
        };
        if let Some(simple) = simple {
            out.push(simple);
            rest = &rest[2..];
            continue;
        }
        let digits = match c {
            'x' => 2,
            'u' => 4,
            'U' => 8,
            _ => 0,
        };
        if digits > 0 {
            let decoded = rest
                .get(2..2 + digits)
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .and_then(char::from_u32);
            match decoded {
                Some(ch) => {
                    out.push(ch);
                    rest = &rest[2 + digits..];
                }
                None => {
                    out.push('\\');
                    rest = &rest[1..];
                }
            }
            continue;
        }
        // Line ending backslash: drop blanks and line breaks up to the next
        // character.
        let after = &rest[1..];
        let blanks = after.bytes().take_while(|&b| b == b' ' || b == b'\t').count();
        if after[blanks..].starts_with('\n') || after[blanks..].starts_with("\r\n") {
            let skipped =
                after.bytes().take_while(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'\r')).count();
            rest = &after[skipped..];
            continue;
        }
        out.push('\\');
        rest = &rest[1..];
    }
    out.push_str(rest);
    Cow::Owned(out)
}
