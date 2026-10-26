//! Column alignment, on formatted text: `=` of entries on consecutive lines,
//! and comments ending consecutive lines.
//!
//! Widths are display widths: East Asian wide characters count twice, so
//! that the columns line up in the editor.

use toml_parser::{SyntaxKind, SyntaxNode, TextSize};

/// Aligns the `=` of runs of consecutive one-line entries of the same
/// table. A blank line, a comment line, a header or a multi-line entry
/// ends a run.
pub(crate) fn entries(text: &str) -> String {
    let parse = toml_parser::parse(text);
    let root = parse.syntax_node();
    // (line, key end, `=` start, width of the line up to the key end)
    let mut found: Vec<(usize, TextSize, TextSize, usize)> = Vec::new();
    let mut containers = vec![root];
    containers.extend(
        root.children().filter(|n| matches!(n.kind(), SyntaxKind::TABLE | SyntaxKind::ARRAY_TABLE)),
    );
    for container in containers {
        for entry in container.children().filter(|n| n.kind() == SyntaxKind::ENTRY) {
            if entry.text().contains('\n') {
                continue;
            }
            let (Some(key), Some(eq)) =
                (entry.child_node(SyntaxKind::KEY), entry.child_token(SyntaxKind::EQ))
            else {
                continue;
            };
            let key_end = key.text_range().end();
            let line_start = line_start(text, key_end);
            let width = display_width(&text[line_start..usize::from(key_end)]);
            found.push((line_of(text, key_end), key_end, eq.text_range().start(), width));
        }
    }
    found.sort_by_key(|f| f.0);
    let mut edits = Vec::new();
    for run in runs(&found, |f| f.0) {
        let target = run.iter().map(|f| f.3).max().unwrap_or(0);
        for &(_, key_end, eq, width) in run {
            edits.push((key_end, eq, " ".repeat(target - width + 1)));
        }
    }
    apply(text, edits)
}

/// Aligns the comments ending runs of consecutive lines.
pub(crate) fn comments(text: &str) -> String {
    let parse = toml_parser::parse(text);
    let root: SyntaxNode<'_> = parse.syntax_node();
    // (line, end of the code, comment start, width of the code)
    let mut found: Vec<(usize, TextSize, TextSize, usize)> = Vec::new();
    for token in root.descendant_tokens().filter(|t| t.kind() == SyntaxKind::COMMENT) {
        let start = token.text_range().start();
        let line_start = line_start(text, start);
        let code = text[line_start..usize::from(start)].trim_end();
        if code.trim().is_empty() {
            continue;
        }
        let code_end = TextSize::new((line_start + code.len()) as u32);
        found.push((line_of(text, start), code_end, start, display_width(code)));
    }
    let mut edits = Vec::new();
    for run in runs(&found, |f| f.0) {
        let target = run.iter().map(|f| f.3).max().unwrap_or(0);
        for &(_, code_end, start, width) in run {
            edits.push((code_end, start, " ".repeat(target - width + 1)));
        }
    }
    apply(text, edits)
}

/// Splits `items` (sorted by line) into runs of consecutive lines, keeping
/// runs of two lines or more.
fn runs<T>(items: &[T], line: impl Fn(&T) -> usize) -> Vec<&[T]> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..=items.len() {
        if i == items.len() || line(&items[i]) != line(&items[i - 1]) + 1 {
            if i - start > 1 {
                out.push(&items[start..i]);
            }
            start = i;
        }
    }
    out
}

fn line_start(text: &str, offset: TextSize) -> usize {
    text[..usize::from(offset)].rfind('\n').map_or(0, |i| i + 1)
}

fn line_of(text: &str, offset: TextSize) -> usize {
    text[..usize::from(offset)].bytes().filter(|&b| b == b'\n').count()
}

/// Replaces the text between each pair of offsets.
fn apply(text: &str, mut edits: Vec<(TextSize, TextSize, String)>) -> String {
    edits.sort_by_key(|e| e.0);
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for (start, end, new) in edits {
        out.push_str(&text[pos..usize::from(start)]);
        out.push_str(&new);
        pos = usize::from(end);
    }
    out.push_str(&text[pos..]);
    out
}

/// The display width of `s`: wide East Asian characters count twice.
fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| match c {
            '\t' => 4,
            '\u{1100}'..='\u{115F}'
            | '\u{2E80}'..='\u{303E}'
            | '\u{3041}'..='\u{33FF}'
            | '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{A000}'..='\u{A4CF}'
            | '\u{AC00}'..='\u{D7A3}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{FE30}'..='\u{FE4F}'
            | '\u{FF00}'..='\u{FF60}'
            | '\u{FFE0}'..='\u{FFE6}'
            | '\u{1F300}'..='\u{1F64F}'
            | '\u{1F900}'..='\u{1F9FF}'
            | '\u{20000}'..='\u{3FFFD}' => 2,
            _ => 1,
        })
        .sum()
}
