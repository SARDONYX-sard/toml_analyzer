//! Zero-copy lexer for TOML (v1.1, which accepts every v1.0 document).
//!
//! Every [`Token`] borrows its text from the source string, and the token
//! stream covers the input losslessly: concatenating the text of all tokens
//! yields the original source.
//!
//! TOML cannot be split into tokens without context: `1979-05-27` is a date
//! after `=` but a bare key before it, `inf` is a float or a key, and `a.b` is
//! a dotted key while `1.5` is a float. The lexer therefore tracks whether it
//! is reading keys or values, following the brackets that open table headers,
//! arrays and inline tables. Line breaks are significant (they end key/value
//! pairs and table headers) and are emitted as [`TokenKind::Newline`] tokens.
//!
//! Malformed input never stops the lexer: the token carries a [`LexError`]
//! and lexing goes on.
//!
//! # Example
//!
//! ```
//! use toml_lexer::{Lexer, TokenKind};
//!
//! let kinds: Vec<_> = Lexer::new("a = 1\n").map(|t| t.kind).collect();
//! assert_eq!(
//!     kinds,
//!     [
//!         TokenKind::BareKey,
//!         TokenKind::Whitespace,
//!         TokenKind::Eq,
//!         TokenKind::Whitespace,
//!         TokenKind::Integer,
//!         TokenKind::Newline,
//!     ]
//! );
//! ```

mod value;

#[cfg(test)]
mod tests;

use std::fmt;

pub use crate::value::{parse_float, parse_integer, unquote};

/// The kind of a lexical token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TokenKind {
    // === Trivia ===
    /// Spaces and tabs.
    Whitespace,
    /// A line break: `\n` or `\r\n`.
    Newline,
    /// `# comment` up to, but excluding, the line break.
    Comment,

    // === Keys ===
    /// An unquoted key: `[A-Za-z0-9_-]+`.
    BareKey,

    // === Strings ===
    /// `"text"`
    BasicString,
    /// `"""text"""`
    MultilineBasicString,
    /// `'text'`
    LiteralString,
    /// `'''text'''`
    MultilineLiteralString,

    // === Other values ===
    /// `42`, `0xFF`, `1_000`
    Integer,
    /// `1.5`, `2e10`, `inf`, `nan`
    Float,
    /// `true` or `false`
    Bool,
    /// `1979-05-27T07:32:00Z`
    OffsetDateTime,
    /// `1979-05-27T07:32:00`
    LocalDateTime,
    /// `1979-05-27`
    LocalDate,
    /// `07:32:00`
    LocalTime,

    // === Punctuation ===
    /// `=`
    Eq,
    /// `.`
    Dot,
    /// `,`
    Comma,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `[[`, opening an array of tables header.
    DoubleLBracket,
    /// `]]`, closing an array of tables header.
    DoubleRBracket,
    /// `{`
    LBrace,
    /// `}`
    RBrace,

    /// Anything else. Always carries an error.
    Unknown,
}

impl TokenKind {
    /// Whitespace or a comment.
    pub fn is_trivia(self) -> bool {
        matches!(self, TokenKind::Whitespace | TokenKind::Comment)
    }

    /// A string of any of the four flavors.
    pub fn is_string(self) -> bool {
        matches!(
            self,
            TokenKind::BasicString
                | TokenKind::MultilineBasicString
                | TokenKind::LiteralString
                | TokenKind::MultilineLiteralString
        )
    }
}

/// A problem found while lexing a token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LexError {
    /// A string without its closing quotes.
    UnterminatedString,
    /// `\q` and the like in a basic string.
    InvalidEscape,
    /// `\u` or `\U` not naming a Unicode scalar value.
    InvalidUnicodeEscape,
    /// A control character in a string or a comment.
    ControlChar,
    /// A malformed number.
    InvalidNumber,
    /// `01`
    LeadingZero,
    /// `1__0`, `_1`, `1_`
    InvalidUnderscore,
    /// An integer outside of the 64 bit signed range.
    IntegerOverflow,
    /// A malformed or impossible date.
    InvalidDate,
    /// A malformed or impossible time.
    InvalidTime,
    /// A word that is no value, usually an unquoted string.
    InvalidValue,
    /// A key with characters only allowed in quoted keys.
    InvalidKey,
    /// A character that cannot appear here.
    UnexpectedChar,
}

impl LexError {
    /// A human readable description.
    pub fn message(self) -> &'static str {
        match self {
            LexError::UnterminatedString => "unterminated string",
            LexError::InvalidEscape => "invalid escape sequence",
            LexError::InvalidUnicodeEscape => "invalid unicode escape: not a unicode scalar value",
            LexError::ControlChar => "control characters must be escaped",
            LexError::InvalidNumber => "invalid number",
            LexError::LeadingZero => "leading zeros are not allowed",
            LexError::InvalidUnderscore => "underscores must be surrounded by digits",
            LexError::IntegerOverflow => "integer does not fit in 64 bits",
            LexError::InvalidDate => "invalid date",
            LexError::InvalidTime => "invalid time",
            LexError::InvalidValue => "invalid value: strings must be quoted",
            LexError::InvalidKey => {
                "invalid bare key: quote keys with other characters than `A-Za-z0-9_-`"
            }
            LexError::UnexpectedChar => "unexpected character",
        }
    }
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

/// A token, borrowing its text from the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token<'a> {
    /// The kind of the token.
    pub kind: TokenKind,
    /// The source text of the token.
    pub text: &'a str,
    /// The byte offset of the token in the source.
    pub offset: u32,
    /// What is wrong with the token, if anything.
    pub error: Option<LexError>,
}

impl Token<'_> {
    /// The length of the token in bytes.
    pub fn len(&self) -> u32 {
        self.text.len() as u32
    }
}

/// Whether keys or values are expected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Key,
    Value,
}

/// An open bracket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ctx {
    /// `[`, of a table header.
    Header,
    /// `[[`, of an array of tables header.
    ArrayHeader,
    /// `[`, of an array value.
    Array,
    /// `{`
    InlineTable,
}

/// An iterator over the tokens of a TOML document.
pub struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    mode: Mode,
    stack: Vec<Ctx>,
}

impl<'a> Lexer<'a> {
    /// Creates a lexer for `src`, which must be smaller than 4 GiB.
    pub fn new(src: &'a str) -> Lexer<'a> {
        Lexer { src, pos: 0, mode: Mode::Key, stack: Vec::new() }
    }

    fn bytes(&self) -> &'a [u8] {
        self.src.as_bytes()
    }

    fn peek(&self, n: usize) -> Option<u8> {
        self.bytes().get(self.pos + n).copied()
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn lex(&mut self) -> (TokenKind, Option<LexError>) {
        let b = self.bytes()[self.pos];
        match b {
            b' ' | b'\t' => {
                self.pos += 1;
                while matches!(self.peek(0), Some(b' ' | b'\t')) {
                    self.pos += 1;
                }
                (TokenKind::Whitespace, None)
            }
            b'\n' => {
                self.pos += 1;
                self.newline();
                (TokenKind::Newline, None)
            }
            b'\r' if self.peek(1) == Some(b'\n') => {
                self.pos += 2;
                self.newline();
                (TokenKind::Newline, None)
            }
            b'#' => {
                let rest = self.rest();
                let mut end = rest.find('\n').unwrap_or(rest.len());
                if end < rest.len() && rest[..end].ends_with('\r') {
                    end -= 1;
                }
                let error = rest[..end].chars().any(is_control).then_some(LexError::ControlChar);
                self.pos += end;
                (TokenKind::Comment, error)
            }
            b'=' => {
                self.pos += 1;
                self.mode = Mode::Value;
                (TokenKind::Eq, None)
            }
            b'.' if self.mode == Mode::Key => {
                self.pos += 1;
                (TokenKind::Dot, None)
            }
            b',' => {
                self.pos += 1;
                match self.stack.last() {
                    Some(Ctx::InlineTable) => self.mode = Mode::Key,
                    Some(Ctx::Array) => self.mode = Mode::Value,
                    _ => {}
                }
                (TokenKind::Comma, None)
            }
            b'[' => {
                self.pos += 1;
                if self.mode == Mode::Value {
                    self.stack.push(Ctx::Array);
                    (TokenKind::LBracket, None)
                } else if self.stack.is_empty() {
                    if self.peek(0) == Some(b'[') {
                        self.pos += 1;
                        self.stack.push(Ctx::ArrayHeader);
                        (TokenKind::DoubleLBracket, None)
                    } else {
                        self.stack.push(Ctx::Header);
                        (TokenKind::LBracket, None)
                    }
                } else {
                    (TokenKind::LBracket, None)
                }
            }
            b']' => {
                self.pos += 1;
                match self.stack.last() {
                    Some(Ctx::ArrayHeader) if self.peek(0) == Some(b']') => {
                        self.pos += 1;
                        self.stack.pop();
                        self.mode = Mode::Key;
                        (TokenKind::DoubleRBracket, None)
                    }
                    Some(Ctx::Header | Ctx::ArrayHeader) => {
                        self.stack.pop();
                        self.mode = Mode::Key;
                        (TokenKind::RBracket, None)
                    }
                    Some(Ctx::Array) => {
                        self.stack.pop();
                        (TokenKind::RBracket, None)
                    }
                    _ => (TokenKind::RBracket, None),
                }
            }
            b'{' => {
                self.pos += 1;
                if self.mode == Mode::Value {
                    self.stack.push(Ctx::InlineTable);
                    self.mode = Mode::Key;
                }
                (TokenKind::LBrace, None)
            }
            b'}' => {
                self.pos += 1;
                if self.stack.last() == Some(&Ctx::InlineTable) {
                    self.stack.pop();
                    self.mode = Mode::Value;
                }
                (TokenKind::RBrace, None)
            }
            b'"' => {
                if self.rest().starts_with("\"\"\"") {
                    self.multiline_basic_string()
                } else {
                    self.basic_string()
                }
            }
            b'\'' => {
                if self.rest().starts_with("'''") {
                    self.multiline_literal_string()
                } else {
                    self.literal_string()
                }
            }
            _ => self.word(),
        }
    }

    fn newline(&mut self) {
        // Arrays and inline tables span lines; headers do not.
        match self.stack.last() {
            Some(Ctx::Array | Ctx::InlineTable) => {}
            _ => {
                self.stack.clear();
                self.mode = Mode::Key;
            }
        }
    }

    /// A run of characters up to the next delimiter: a key, a value or junk.
    fn word(&mut self) -> (TokenKind, Option<LexError>) {
        let start = self.pos;
        let is_delimiter = |c: char, mode: Mode| {
            matches!(
                c,
                ' ' | '\t' | '\n' | '\r' | '#' | '=' | ',' | '[' | ']' | '{' | '}' | '"' | '\''
            ) || (c == '.' && mode == Mode::Key)
        };
        let end = self
            .rest()
            .char_indices()
            .find(|&(_, c)| is_delimiter(c, self.mode))
            .map_or(self.src.len(), |(i, _)| self.pos + i);
        // A lone `\r` is a delimiter that no other rule takes.
        let end = if end == start {
            start + self.rest().chars().next().map_or(1, char::len_utf8)
        } else {
            end
        };
        self.pos = end;
        let text = &self.src[start..end];
        if text == "\r" {
            return (TokenKind::Unknown, Some(LexError::UnexpectedChar));
        }
        match self.mode {
            Mode::Key => {
                if text.bytes().all(is_bare_key_byte) {
                    (TokenKind::BareKey, None)
                } else {
                    (TokenKind::Unknown, Some(LexError::InvalidKey))
                }
            }
            Mode::Value => {
                // `1979-05-27 07:32:00`: a space may separate date and time.
                if is_date_prefix(text)
                    && text.len() == 10
                    && self.peek(0) == Some(b' ')
                    && self.peek(1).is_some_and(|b| b.is_ascii_digit())
                    && self.peek(2).is_some_and(|b| b.is_ascii_digit())
                    && self.peek(3) == Some(b':')
                {
                    self.pos += 1;
                    let more = self
                        .rest()
                        .char_indices()
                        .find(|&(_, c)| is_delimiter(c, Mode::Value))
                        .map_or(self.src.len(), |(i, _)| self.pos + i);
                    self.pos = more;
                }
                let (kind, error) = value::classify(&self.src[start..self.pos]);
                (kind, error)
            }
        }
    }

    fn basic_string(&mut self) -> (TokenKind, Option<LexError>) {
        self.pos += 1;
        let mut error = None;
        loop {
            let Some(c) = self.rest().chars().next() else {
                return (TokenKind::BasicString, Some(LexError::UnterminatedString));
            };
            match c {
                '"' => {
                    self.pos += 1;
                    return (TokenKind::BasicString, error);
                }
                '\n' => return (TokenKind::BasicString, Some(LexError::UnterminatedString)),
                '\r' if self.peek(1) == Some(b'\n') => {
                    return (TokenKind::BasicString, Some(LexError::UnterminatedString));
                }
                '\\' => {
                    if let Err(e) = self.escape(false) {
                        error.get_or_insert(e);
                    }
                }
                _ => {
                    if is_control(c) {
                        error.get_or_insert(LexError::ControlChar);
                    }
                    self.pos += c.len_utf8();
                }
            }
        }
    }

    fn multiline_basic_string(&mut self) -> (TokenKind, Option<LexError>) {
        self.pos += 3;
        let mut error = None;
        loop {
            let Some(c) = self.rest().chars().next() else {
                return (TokenKind::MultilineBasicString, Some(LexError::UnterminatedString));
            };
            match c {
                '"' if self.rest().starts_with("\"\"\"") => {
                    // Up to two quotes may precede the closing delimiter.
                    let quotes = self.rest().bytes().take_while(|&b| b == b'"').count().min(5);
                    self.pos += quotes;
                    return (TokenKind::MultilineBasicString, error);
                }
                '\\' => {
                    if let Err(e) = self.escape(true) {
                        error.get_or_insert(e);
                    }
                }
                '\n' | '\t' => self.pos += 1,
                '\r' if self.peek(1) == Some(b'\n') => self.pos += 2,
                _ => {
                    if is_control(c) {
                        error.get_or_insert(LexError::ControlChar);
                    }
                    self.pos += c.len_utf8();
                }
            }
        }
    }

    fn literal_string(&mut self) -> (TokenKind, Option<LexError>) {
        self.pos += 1;
        let mut error = None;
        loop {
            let Some(c) = self.rest().chars().next() else {
                return (TokenKind::LiteralString, Some(LexError::UnterminatedString));
            };
            match c {
                '\'' => {
                    self.pos += 1;
                    return (TokenKind::LiteralString, error);
                }
                '\n' => return (TokenKind::LiteralString, Some(LexError::UnterminatedString)),
                '\r' if self.peek(1) == Some(b'\n') => {
                    return (TokenKind::LiteralString, Some(LexError::UnterminatedString));
                }
                _ => {
                    if is_control(c) {
                        error.get_or_insert(LexError::ControlChar);
                    }
                    self.pos += c.len_utf8();
                }
            }
        }
    }

    fn multiline_literal_string(&mut self) -> (TokenKind, Option<LexError>) {
        self.pos += 3;
        let mut error = None;
        loop {
            let Some(c) = self.rest().chars().next() else {
                return (TokenKind::MultilineLiteralString, Some(LexError::UnterminatedString));
            };
            match c {
                '\'' if self.rest().starts_with("'''") => {
                    let quotes = self.rest().bytes().take_while(|&b| b == b'\'').count().min(5);
                    self.pos += quotes;
                    return (TokenKind::MultilineLiteralString, error);
                }
                '\n' | '\t' => self.pos += 1,
                '\r' if self.peek(1) == Some(b'\n') => self.pos += 2,
                _ => {
                    if is_control(c) {
                        error.get_or_insert(LexError::ControlChar);
                    }
                    self.pos += c.len_utf8();
                }
            }
        }
    }

    /// Consumes an escape sequence at `\`. In multi-line strings, a `\` ending
    /// a line is allowed.
    fn escape(&mut self, multiline: bool) -> Result<(), LexError> {
        self.pos += 1;
        let Some(c) = self.rest().chars().next() else { return Err(LexError::InvalidEscape) };
        match c {
            'b' | 't' | 'n' | 'f' | 'r' | 'e' | '"' | '\\' => {
                self.pos += 1;
                Ok(())
            }
            'x' => self.hex_escape(2),
            'u' => self.hex_escape(4),
            'U' => self.hex_escape(8),
            ' ' | '\t' | '\n' | '\r' if multiline => {
                // Line ending backslash: only blanks up to the line break.
                let blanks = self.rest().bytes().take_while(|&b| b == b' ' || b == b'\t').count();
                let after = &self.rest()[blanks..];
                if after.starts_with('\n') || after.starts_with("\r\n") {
                    self.pos += blanks;
                    Ok(())
                } else {
                    self.pos += blanks;
                    Err(LexError::InvalidEscape)
                }
            }
            '\n' | '\r' => Err(LexError::InvalidEscape),
            _ => {
                self.pos += c.len_utf8();
                Err(LexError::InvalidEscape)
            }
        }
    }

    fn hex_escape(&mut self, digits: usize) -> Result<(), LexError> {
        self.pos += 1;
        let hex = self.rest().bytes().take(digits).take_while(u8::is_ascii_hexdigit).count();
        let text = &self.rest()[..hex];
        self.pos += hex;
        if hex != digits {
            return Err(LexError::InvalidEscape);
        }
        let value = u32::from_str_radix(text, 16).map_err(|_| LexError::InvalidEscape)?;
        char::from_u32(value).map(|_| ()).ok_or(LexError::InvalidUnicodeEscape)
    }
}

impl<'a> Iterator for Lexer<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Token<'a>> {
        if self.pos >= self.src.len() {
            return None;
        }
        let start = self.pos;
        let (kind, error) = self.lex();
        debug_assert!(self.pos > start, "the lexer made no progress at {start}");
        Some(Token { kind, text: &self.src[start..self.pos], offset: start as u32, error })
    }
}

/// Whether `b` may appear in a bare key.
pub fn is_bare_key_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// Whether `key` can be written without quotes.
pub fn is_bare_key(key: &str) -> bool {
    !key.is_empty() && key.bytes().all(is_bare_key_byte)
}

/// Control characters, which TOML only allows escaped (tab excepted).
fn is_control(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{8}' | '\u{A}'..='\u{1F}' | '\u{7F}')
}

/// `YYYY-MM-DD` at the start of `text`.
fn is_date_prefix(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
}
