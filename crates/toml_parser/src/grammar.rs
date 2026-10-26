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

/// Entries up to the next table header.
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

fn entry(p: &mut Parser<'_>) {
    let m = p.open();
    key(p);
    if p.expect(EQ) || at_value_start(p.current()) {
        value(p);
    }
    p.close(m, ENTRY);
}

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

/// The end of an entry or a header: a newline or the end of the input.
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
