//! Folding ranges: tables, multi-line arrays, inline tables and strings,
//! and blocks of comments.

use toml_parser::{SyntaxElement, SyntaxKind, TextRange};

use crate::Document;

/// What is folded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FoldKind {
    /// A table section or a value.
    Region,
    /// Consecutive comment lines.
    Comment,
}

/// A foldable range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fold {
    /// The range; the caller keeps those spanning several lines.
    pub range: TextRange,
    /// The kind.
    pub kind: FoldKind,
}

/// The folding ranges of `doc`.
pub fn folding_ranges(doc: &Document) -> Vec<Fold> {
    let mut out = Vec::new();
    let mut comments: Option<TextRange> = None;
    let mut newlines = 0;
    for element in doc.root().descendants_with_tokens() {
        match element {
            SyntaxElement::Node(n) => {
                let range = match n.kind() {
                    SyntaxKind::TABLE | SyntaxKind::ARRAY_TABLE => n.trimmed_range(),
                    SyntaxKind::ARRAY | SyntaxKind::INLINE_TABLE => n.text_range(),
                    _ => continue,
                };
                out.push(Fold { range, kind: FoldKind::Region });
            }
            SyntaxElement::Token(t) => match t.kind() {
                SyntaxKind::COMMENT => {
                    comments = Some(match comments {
                        Some(c) if newlines <= 1 => c.cover(t.text_range()),
                        Some(c) => {
                            out.push(Fold { range: c, kind: FoldKind::Comment });
                            t.text_range()
                        }
                        None => t.text_range(),
                    });
                    newlines = 0;
                }
                SyntaxKind::NEWLINE => newlines += 1,
                SyntaxKind::WHITESPACE => {}
                kind => {
                    if let Some(c) = comments.take() {
                        out.push(Fold { range: c, kind: FoldKind::Comment });
                    }
                    if matches!(
                        kind,
                        SyntaxKind::MULTILINE_BASIC_STRING | SyntaxKind::MULTILINE_LITERAL_STRING
                    ) {
                        out.push(Fold { range: t.text_range(), kind: FoldKind::Region });
                    }
                    newlines = 2;
                }
            },
        }
    }
    if let Some(c) = comments {
        out.push(Fold { range: c, kind: FoldKind::Comment });
    }
    out
}
