//! The `format` keyword, and ECMAScript regular expressions.

use toml_dom::{DateTimeKind, ValueKind};

/// Whether a TOML date-time of `kind` has the format `name`. Unknown formats
/// accept everything.
pub(crate) fn date_time_fits(name: &str, kind: DateTimeKind) -> bool {
    match name {
        "date-time" => kind == DateTimeKind::OffsetDateTime,
        "date" => kind == DateTimeKind::LocalDate,
        "time" | "partial-time" => kind == DateTimeKind::LocalTime,
        "local-date-time" | "partial-date-time" => kind == DateTimeKind::LocalDateTime,
        _ => true,
    }
}

/// Describes what `name` expects, for messages.
pub(crate) fn describe(name: &str, value: &ValueKind) -> String {
    match (name, value) {
        ("date-time", ValueKind::DateTime(..)) => {
            "date-time: expected an offset date-time".to_owned()
        }
        ("date", ValueKind::DateTime(..)) => "date: expected a local date".to_owned(),
        ("time" | "partial-time", ValueKind::DateTime(..)) => {
            "time: expected a local time".to_owned()
        }
        _ => name.to_owned(),
    }
}

/// Whether the string `s` has the format `name`. Unknown formats accept
/// everything.
pub(crate) fn check(name: &str, s: &str) -> bool {
    match name {
        "date-time" => date_time(s, true),
        "local-date-time" | "partial-date-time" => date_time(s, false),
        "date" => date(s),
        "time" => time(s, true) || time(s, false),
        "partial-time" => time(s, false),
        "email" | "idn-email" => email(s),
        "hostname" | "idn-hostname" => hostname(s),
        "ipv4" => s.parse::<std::net::Ipv4Addr>().is_ok(),
        "ipv6" => s.parse::<std::net::Ipv6Addr>().is_ok(),
        "uri" | "iri" => url::Url::parse(s).is_ok(),
        "uri-reference" | "iri-reference" | "uri-template" => !s.chars().any(char::is_whitespace),
        "uuid" => uuid(s),
        "regex" => regex::Regex::new(&translate_regex(s)).is_ok(),
        "json-pointer" => s.is_empty() || s.starts_with('/'),
        _ => true,
    }
}

fn digits(s: &str, n: usize) -> Option<(u32, &str)> {
    let d = s.get(..n)?;
    d.bytes().all(|b| b.is_ascii_digit()).then(|| (d.parse().unwrap_or(0), &s[n..]))
}

fn date(s: &str) -> bool {
    date_prefix(s).is_some_and(str::is_empty)
}

fn date_prefix(s: &str) -> Option<&str> {
    let (y, s) = digits(s, 4)?;
    let s = s.strip_prefix('-')?;
    let (m, s) = digits(s, 2)?;
    let s = s.strip_prefix('-')?;
    let (d, s) = digits(s, 2)?;
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let max = match m {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1..=12 => 31,
        _ => return None,
    };
    (1..=max).contains(&d).then_some(s)
}

fn time(s: &str, offset: bool) -> bool {
    let Some((h, s)) = digits(s, 2) else { return false };
    let Some(s) = s.strip_prefix(':') else { return false };
    let Some((m, s)) = digits(s, 2) else { return false };
    let Some(s) = s.strip_prefix(':') else { return false };
    let Some((sec, mut s)) = digits(s, 2) else { return false };
    if h > 23 || m > 59 || sec > 60 {
        return false;
    }
    if let Some(frac) = s.strip_prefix('.') {
        let n = frac.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 {
            return false;
        }
        s = &frac[n..];
    }
    if !offset {
        return s.is_empty();
    }
    if s == "Z" || s == "z" {
        return true;
    }
    let Some(rest) = s.strip_prefix(['+', '-']) else { return false };
    let Some((oh, rest)) = digits(rest, 2) else { return false };
    let Some(rest) = rest.strip_prefix(':') else { return false };
    matches!(digits(rest, 2), Some((om, "")) if oh <= 23 && om <= 59)
}

fn date_time(s: &str, offset: bool) -> bool {
    let Some(rest) = date_prefix(s) else { return false };
    let Some(rest) = rest.strip_prefix(['T', 't', ' ']) else { return false };
    time(rest, offset)
}

fn email(s: &str) -> bool {
    match s.rsplit_once('@') {
        Some((local, domain)) => {
            !local.is_empty() && !local.contains(char::is_whitespace) && hostname(domain)
        }
        None => false,
    }
}

fn hostname(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && s.trim_end_matches('.').split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_alphanumeric() || c == '-')
        })
}

fn uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// Rewrites the ECMAScript constructs that the `regex` crate reads
/// differently: `\d` is ASCII only there, `\/` is an unknown escape here.
pub(crate) fn translate_regex(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('d') => out.push_str("[0-9]"),
            Some('D') => out.push_str("[^0-9]"),
            Some('/') => out.push('/'),
            Some('-') => out.push_str("\\-"),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}
