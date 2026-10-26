//! The TOML grammar.
//!
//! ```text
//! root        = entries (table | array_table)*
//! table       = "[" key "]" eol entries
//! array_table = "[[" key "]]" eol entries
//! entries     = (entry eol | NEWLINE)*
//! entry       = key "=" value
//! key         = segment ("." segment)*
//! value       = scalar | array | inline_table
//! array       = "[" (value ("," value)* ","?)? "]"
//! inline_table = "{" (entry ("," entry)* ","?)? "}"
//! ```
//!
//! Newlines may appear anywhere inside arrays and inline tables (TOML 1.1).

use crate::{
    SyntaxKind::{self, *},
    parser::Parser,
};

/// The document: the entries before the first header, then the tables.
///
/// ```text
/// root = entries (table | array_table)*
/// ```
///
/// ```toml
/// title = "x"      # entries
/// [package]        # table
/// [[bin]]          # array_table
/// ```
///
/// Produces `ROOT`. Never fails: whatever cannot be parsed ends up in
/// `ERROR` nodes.
pub(crate) fn root(p: &mut Parser<'_>) {
    let m = p.open();
    entries(p);
    while !p.eof() {
        if p.at(L_BRACK) || p.at(L_BRACK2) {
            table(p);
        } else {
            // `entries` stops at headers and at the end of the input only.
            p.advance_with_error("expected a table header");
        }
    }
    p.close(m, ROOT);
}

/// The entries of a table section, up to the next header or the end of the
/// input. Blank lines are skipped.
///
/// ```text
/// entries = (entry eol | NEWLINE)*
/// ```
///
/// Produces no node of its own: the entries are children of the `ROOT` or
/// `TABLE`. A line starting with anything else than a key is wrapped in an
/// `ERROR` node.
fn entries(p: &mut Parser<'_>) {
    loop {
        match p.current() {
            EOF | L_BRACK | L_BRACK2 => return,
            NEWLINE => p.advance(),
            k if at_key_start(k) => {
                entry(p);
                eol(p);
            }
            _ => skip_line(p, "expected a key, a table header or a newline"),
        }
    }
}

/// A `[table]` or `[[array.of.tables]]` header and its entries.
///
/// ```text
/// table       = "[" key "]" eol entries
/// array_table = "[[" key "]]" eol entries
/// ```
///
/// ```toml
/// [dependencies.serde]
/// version = "1"
/// ```
///
/// Produces `TABLE` (or `ARRAY_TABLE`) holding a `TABLE_HEADER` (or
/// `ARRAY_TABLE_HEADER`) and the entries. A mismatched bracket (`[[a]`,
/// `[a]]`) is reported and consumed; a missing one is reported only.
fn table(p: &mut Parser<'_>) {
    let m = p.open();
    let array = p.at(L_BRACK2);
    let header = p.open();
    p.advance();
    if at_key_start(p.current()) {
        key(p);
    } else {
        p.error_expected("a table name");
    }
    if array {
        if p.at(R_BRACK) {
            p.advance_with_error("expected `]]`");
        } else {
            p.expect(R_BRACK2);
        }
        p.close(header, ARRAY_TABLE_HEADER);
    } else {
        if p.at(R_BRACK2) {
            p.advance_with_error("expected `]`");
        } else {
            p.expect(R_BRACK);
        }
        p.close(header, TABLE_HEADER);
    }
    eol(p);
    entries(p);
    p.close(m, if array { ARRAY_TABLE } else { TABLE });
}

/// A key/value pair.
///
/// ```text
/// entry = key "=" value
/// ```
///
/// ```toml
/// a."b c".d = 1
/// ```
///
/// Produces `ENTRY` holding a `KEY`, the `=` token and the value. A missing
/// `=` is reported, and the value is still parsed when one follows
/// (`a 1`); a missing value is reported after the `=`.
fn entry(p: &mut Parser<'_>) {
    let m = p.open();
    key(p);
    if p.expect(EQ) || at_value_start(p.current()) {
        value(p);
    }
    p.close(m, ENTRY);
}

/// A dotted key.
///
/// ```text
/// key = segment ("." segment)*
/// ```
///
/// Produces `KEY` holding the segment tokens and the dots. A trailing `.`
/// is reported.
fn key(p: &mut Parser<'_>) {
    let m = p.open();
    segment(p);
    while p.eat(DOT) {
        if at_key_start(p.current()) {
            segment(p);
        } else {
            p.error_expected("a key after `.`");
            break;
        }
    }
    p.close(m, KEY);
}

/// A part of a key.
///
/// ```text
/// segment = BARE_KEY | BASIC_STRING | LITERAL_STRING
/// ```
///
/// A single token. An invalid bare key (`UNKNOWN`, reported by the lexer)
/// is accepted as a segment; a multi-line string is wrapped in an `ERROR`
/// node.
fn segment(p: &mut Parser<'_>) {
    match p.current() {
        // UNKNOWN: an invalid bare key, already reported by the lexer.
        BARE_KEY | BASIC_STRING | LITERAL_STRING | UNKNOWN => p.advance(),
        MULTILINE_BASIC_STRING | MULTILINE_LITERAL_STRING => {
            p.advance_with_error("multi-line strings cannot be keys");
        }
        _ => p.error_expected("a key"),
    }
}

/// A value.
///
/// ```text
/// value  = scalar | array | inline_table
/// scalar = STRING | INTEGER | FLOAT | BOOL | DATE_TIME
/// ```
///
/// A scalar is a single token in the `ENTRY` or `ARRAY`. An unquoted word
/// (`UNKNOWN`, reported by the lexer) is accepted as a value.
fn value(p: &mut Parser<'_>) {
    match p.current() {
        k if k.is_scalar() => p.advance(),
        // An unquoted string, already reported by the lexer.
        UNKNOWN | BARE_KEY => p.advance(),
        L_BRACK => array(p),
        L_BRACE => inline_table(p),
        _ => p.error_expected("a value"),
    }
}

/// An array, which may span lines and end with a comma.
///
/// ```text
/// array = "[" NEWLINE* (value NEWLINE* ("," NEWLINE* value NEWLINE*)* ","?)? NEWLINE* "]"
/// ```
///
/// ```toml
/// a = [
///   1, # comment
///   2,
/// ]
/// ```
///
/// Produces `ARRAY`. A missing `,` between values is reported and the next
/// value parsed. An unclosed array stops before a line looking like
/// `key = ...`, see [`looks_like_entry`].
fn array(p: &mut Parser<'_>) {
    let m = p.open();
    p.advance();
    loop {
        skip_newlines(p);
        if p.at(R_BRACK) || p.eof() || looks_like_entry(p) {
            break;
        }
        if at_value_start(p.current()) {
            value(p);
        } else if p.at(COMMA) {
            p.advance_with_error("expected a value");
            continue;
        } else {
            p.advance_with_error("expected a value or `]`");
            continue;
        }
        skip_newlines(p);
        if p.eat(COMMA) {
            continue;
        }
        if p.at(R_BRACK) || p.eof() || looks_like_entry(p) {
            break;
        }
        p.error_expected("`,` or `]`");
        if !at_value_start(p.current()) {
            p.advance_with_error("unexpected token in array");
        }
    }
    p.expect(R_BRACK);
    p.close(m, ARRAY);
}

/// An inline table. TOML 1.1 allows newlines and a trailing comma.
///
/// ```text
/// inline_table = "{" NEWLINE* (entry NEWLINE* ("," NEWLINE* entry NEWLINE*)* ","?)? NEWLINE* "}"
/// ```
///
/// ```toml
/// serde = { version = "1", features = ["derive"] }
/// ```
///
/// Produces `INLINE_TABLE` holding `ENTRY` nodes. A missing `,` between
/// entries is reported and the next entry parsed.
fn inline_table(p: &mut Parser<'_>) {
    let m = p.open();
    p.advance();
    loop {
        skip_newlines(p);
        if p.at(R_BRACE) || p.eof() {
            break;
        }
        if at_key_start(p.current()) {
            entry(p);
        } else {
            p.advance_with_error("expected a key or `}`");
            continue;
        }
        skip_newlines(p);
        if p.eat(COMMA) {
            continue;
        }
        if p.at(R_BRACE) || p.eof() {
            break;
        }
        p.error_expected("`,` or `}`");
        if !at_key_start(p.current()) {
            p.advance_with_error("unexpected token in inline table");
        }
    }
    p.expect(R_BRACE);
    p.close(m, INLINE_TABLE);
}

/// The end of an entry or a header.
///
/// ```text
/// eol = NEWLINE | EOF
/// ```
///
/// Anything else up to the line break is reported and wrapped in an
/// `ERROR` node (`a = 1 b = 2`).
fn eol(p: &mut Parser<'_>) {
    // An unclosed array may have taken the newline already.
    if p.eat(NEWLINE) || p.eof() || p.prev() == NEWLINE {
        return;
    }
    skip_line(p, "expected a newline");
    p.eat(NEWLINE);
}

/// Wraps the rest of the line in an `ERROR` node.
fn skip_line(p: &mut Parser<'_>, message: &str) {
    p.error_at_current(message);
    let m = p.open();
    while !p.at_eol() {
        p.advance();
    }
    p.close(m, ERROR);
}

fn skip_newlines(p: &mut Parser<'_>) {
    while p.eat(NEWLINE) {}
}

/// An unclosed array followed by `key = ...` on a new line: the array most
/// likely misses its `]`.
fn looks_like_entry(p: &Parser<'_>) -> bool {
    matches!(p.current(), UNKNOWN | BARE_KEY | INTEGER | BOOL | FLOAT) && p.nth_at(1, EQ)
}

fn at_key_start(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        BARE_KEY
            | BASIC_STRING
            | LITERAL_STRING
            | MULTILINE_BASIC_STRING
            | MULTILINE_LITERAL_STRING
            | UNKNOWN
    )
}

fn at_value_start(kind: SyntaxKind) -> bool {
    kind.is_scalar() || matches!(kind, UNKNOWN | BARE_KEY | L_BRACK | L_BRACE)
}
