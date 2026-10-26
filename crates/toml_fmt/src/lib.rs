//! Formatter for TOML.
//!
//! The formatter only changes whitespace and commas: keys, values and
//! comments are written as they are. It normalizes
//!
//! - `key = value`, with one space around `=` and none around the dots of
//!   keys and in headers;
//! - arrays: `[1, 2]` on one line, or one item per line with a trailing comma
//!   when the array spans lines or holds comments;
//! - inline tables: `{ a = 1, b = 2 }`;
//! - one space before trailing comments, no trailing whitespace, at most
//!   [`Options::max_blank_lines`] blank lines in a row, and one final newline.
//!
//! Documents with syntax errors are not formatted.

#[cfg(test)]
mod tests;

use toml_parser::{SyntaxElement, SyntaxKind, SyntaxNode};

/// Formatting options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// One level of indentation in multi-line arrays.
    pub indent: String,
    /// Blank lines allowed in a row.
    pub max_blank_lines: usize,
    /// Indent the entries of `[tables]` by their depth.
    pub indent_tables: bool,
    /// Use `\r\n` line breaks.
    pub crlf: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options { indent: "  ".to_owned(), max_blank_lines: 2, indent_tables: false, crlf: false }
    }
}

/// Formats `text`; `None` when it has syntax errors.
pub fn format(text: &str, options: &Options) -> Option<String> {
    let parse = toml_parser::parse(text);
    if !parse.errors().is_empty() {
        return None;
    }
    let mut f =
        Formatter { out: String::new(), options, blank_lines: 0, line_start: true, indent: 0 };
    f.container(parse.syntax_node());
    let mut out = f.out.trim_end().to_owned();
    out.push('\n');
    if options.crlf {
        out = out.replace('\n', "\r\n");
    }
    Some(out)
}

struct Formatter<'o> {
    out: String,
    options: &'o Options,
    /// Blank lines written in a row.
    blank_lines: usize,
    /// Nothing written on the current line yet.
    line_start: bool,
    /// The indentation of new lines, in levels.
    indent: usize,
}

impl Formatter<'_> {
    fn write(&mut self, s: &str) {
        if self.line_start {
            for _ in 0..self.indent {
                self.out.push_str(&self.options.indent);
            }
            self.line_start = false;
        }
        self.out.push_str(s);
        self.blank_lines = 0;
    }

    /// Ends the current line; on an empty line, writes a blank line within
    /// the limits.
    fn newline(&mut self) {
        if self.line_start {
            if self.out.is_empty() || self.blank_lines >= self.options.max_blank_lines {
                return;
            }
            self.blank_lines += 1;
        }
        let trimmed = self.out.trim_end_matches([' ', '\t']).len();
        self.out.truncate(trimmed);
        self.out.push('\n');
        self.line_start = true;
    }

    /// Ends the current line if something was written on it.
    fn end_line(&mut self) {
        if !self.line_start {
            self.newline();
        }
    }

    fn comment(&mut self, text: &str) {
        if !self.line_start {
            self.out.push(' ');
        }
        self.write(text.trim_end());
    }

    /// The root or a table: headers, entries, comments and newlines.
    fn container(&mut self, node: SyntaxNode<'_>) {
        for child in node.children_with_tokens() {
            match child {
                SyntaxElement::Token(t) => match t.kind() {
                    SyntaxKind::NEWLINE => self.newline(),
                    SyntaxKind::COMMENT => self.comment(t.text()),
                    _ => {}
                },
                SyntaxElement::Node(n) => match n.kind() {
                    SyntaxKind::TABLE | SyntaxKind::ARRAY_TABLE => {
                        let depth = if self.options.indent_tables { header_depth(n) } else { 0 };
                        self.indent = depth.saturating_sub(1);
                        self.container(n);
                        self.indent = 0;
                    }
                    SyntaxKind::TABLE_HEADER | SyntaxKind::ARRAY_TABLE_HEADER => {
                        let array = n.kind() == SyntaxKind::ARRAY_TABLE_HEADER;
                        self.write(if array { "[[" } else { "[" });
                        if let Some(key) = n.child_node(SyntaxKind::KEY) {
                            self.key(key);
                        }
                        self.write(if array { "]]" } else { "]" });
                        if self.options.indent_tables {
                            self.indent += 1;
                        }
                    }
                    SyntaxKind::ENTRY => self.entry(n),
                    _ => self.write(n.text()),
                },
            }
        }
    }

    fn key(&mut self, key: SyntaxNode<'_>) {
        let parts: Vec<&str> = key
            .child_tokens()
            .filter(|t| !t.kind().is_trivia() && t.kind() != SyntaxKind::DOT)
            .map(|t| t.text())
            .collect();
        self.write(&parts.join("."));
    }

    fn entry(&mut self, entry: SyntaxNode<'_>) {
        let mut after_eq = false;
        for child in entry.children_with_tokens() {
            match child {
                SyntaxElement::Node(n) if n.kind() == SyntaxKind::KEY => self.key(n),
                SyntaxElement::Token(t) if t.kind() == SyntaxKind::EQ => {
                    self.write(" = ");
                    after_eq = true;
                }
                SyntaxElement::Token(t) if t.kind() == SyntaxKind::COMMENT => {
                    self.comment(t.text())
                }
                child if after_eq => self.value(child),
                _ => {}
            }
        }
    }

    fn value(&mut self, value: SyntaxElement<'_>) {
        match value {
            SyntaxElement::Token(t) if !t.kind().is_trivia() && t.kind() != SyntaxKind::NEWLINE => {
                self.write(t.text());
            }
            SyntaxElement::Node(n) if n.kind() == SyntaxKind::ARRAY => self.array(n),
            SyntaxElement::Node(n) if n.kind() == SyntaxKind::INLINE_TABLE => self.inline_table(n),
            _ => {}
        }
    }

    fn array(&mut self, array: SyntaxNode<'_>) {
        let multiline = array
            .child_tokens()
            .any(|t| matches!(t.kind(), SyntaxKind::NEWLINE | SyntaxKind::COMMENT));
        let values: Vec<SyntaxElement<'_>> =
            array.children_with_tokens().filter(|e| is_value(*e)).collect();
        if !multiline {
            self.write("[");
            for (i, v) in values.iter().enumerate() {
                if i > 0 {
                    self.write(", ");
                }
                self.value(*v);
            }
            self.write("]");
            return;
        }
        self.write("[");
        self.indent += 1;
        self.end_line();
        // Blank lines inside arrays are dropped: nd_line ignores them.
        let mut item_on_line = false;
        for child in array.children_with_tokens() {
            match child {
                SyntaxElement::Token(t) => match t.kind() {
                    SyntaxKind::COMMENT => {
                        if !item_on_line {
                            self.end_line();
                        }
                        self.comment(t.text());
                        self.end_line();
                        item_on_line = false;
                    }
                    SyntaxKind::NEWLINE => {
                        self.end_line();
                        item_on_line = false;
                    }
                    _ if is_value(child) => {
                        self.end_line();
                        self.value(child);
                        self.write(",");
                        item_on_line = true;
                    }
                    _ => {}
                },
                SyntaxElement::Node(_) if is_value(child) => {
                    self.end_line();
                    self.value(child);
                    self.write(",");
                    item_on_line = true;
                }
                SyntaxElement::Node(_) => {}
            }
        }
        self.indent -= 1;
        self.end_line();
        self.write("]");
    }

    fn inline_table(&mut self, table: SyntaxNode<'_>) {
        let entries: Vec<SyntaxNode<'_>> =
            table.children().filter(|n| n.kind() == SyntaxKind::ENTRY).collect();
        let multiline = table.child_tokens().any(|t| t.kind() == SyntaxKind::COMMENT);
        if entries.is_empty() && !multiline {
            self.write("{}");
            return;
        }
        if !multiline {
            self.write("{ ");
            for (i, e) in entries.iter().enumerate() {
                if i > 0 {
                    self.write(", ");
                }
                self.entry(*e);
            }
            self.write(" }");
            return;
        }
        // Comments need lines of their own (TOML 1.1).
        self.write("{");
        self.indent += 1;
        let last = entries.last().copied();
        for child in table.children_with_tokens() {
            match child {
                SyntaxElement::Token(t) if t.kind() == SyntaxKind::COMMENT => {
                    self.comment(t.text());
                    self.end_line();
                }
                SyntaxElement::Node(n) if n.kind() == SyntaxKind::ENTRY => {
                    self.end_line();
                    self.entry(n);
                    if Some(n) != last {
                        self.write(",");
                    }
                }
                _ => {}
            }
        }
        self.indent -= 1;
        self.end_line();
        self.write("}");
    }
}

fn is_value(e: SyntaxElement<'_>) -> bool {
    match e {
        SyntaxElement::Token(t) => t.kind().is_scalar() || t.kind() == SyntaxKind::UNKNOWN,
        SyntaxElement::Node(n) => matches!(n.kind(), SyntaxKind::ARRAY | SyntaxKind::INLINE_TABLE),
    }
}

/// The number of keys of the header of a table.
fn header_depth(table: SyntaxNode<'_>) -> usize {
    table
        .children()
        .find(|n| matches!(n.kind(), SyntaxKind::TABLE_HEADER | SyntaxKind::ARRAY_TABLE_HEADER))
        .and_then(|h| h.child_node(SyntaxKind::KEY))
        .map_or(1, |k| k.child_tokens().filter(|t| t.kind() == SyntaxKind::DOT).count() + 1)
}
