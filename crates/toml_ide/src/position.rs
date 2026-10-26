//! What the cursor is on: a key being written, a value, or nothing of
//! interest, and the path of the document there.

use toml_dom::{Path, PathSegment};
use toml_parser::{
    SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, TextRange, TextSize,
    ast::{self, AstNode},
};

use crate::Document;

/// The role of the cursor position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PositionKind {
    /// A key of the table at the path.
    Key {
        /// The key segment under the cursor, or an empty range at the cursor.
        range: TextRange,
        /// The decoded segment under the cursor.
        segment: Option<String>,
        /// In a `[header]` or `[[header]]`.
        header: bool,
    },
    /// The value at the path.
    Value {
        /// The scalar under the cursor, or an empty range at the cursor.
        range: TextRange,
    },
    /// A comment, a complete key before its `=`, ...
    None,
}

/// See [`position_at`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Position {
    /// For keys, the table the key belongs to (with the dotted segments
    /// before the cursor); for values, the value.
    pub path: Path,
    /// The role of the position.
    pub kind: PositionKind,
}

impl Position {
    /// The path of the key under the cursor, for a key position on a key.
    pub fn key_path(&self) -> Option<Path> {
        match &self.kind {
            PositionKind::Key { segment: Some(s), .. } => {
                let mut path = self.path.clone();
                path.push(PathSegment::Key(s.clone()));
                Some(path)
            }
            _ => None,
        }
    }
}

fn token_priority(t: &SyntaxToken<'_>) -> u8 {
    match t.kind() {
        k if k.is_trivia() || k == SyntaxKind::NEWLINE => 0,
        SyntaxKind::R_BRACK | SyntaxKind::R_BRACK2 | SyntaxKind::R_BRACE | SyntaxKind::COMMA => 1,
        _ => 2,
    }
}

/// Describes the cursor at `offset`.
pub fn position_at(doc: &Document, offset: TextSize) -> Position {
    let root = doc.root();
    let none = |path| Position { path, kind: PositionKind::None };
    let Some(token) = root.token_at_offset(offset).max_by_key(token_priority) else {
        return Position {
            path: Path::new(),
            kind: PositionKind::Key {
                range: TextRange::empty(offset),
                segment: None,
                header: false,
            },
        };
    };
    if token.kind() == SyntaxKind::COMMENT && offset > token.text_range().start() {
        return none(Path::new());
    }
    // `key = |`: the blanks after a missing value are not in the entry.
    let token =
        if token_priority(&token) == 0 { equals_before(token).unwrap_or(token) } else { token };
    let mut nodes: Vec<SyntaxNode<'_>> = token.ancestors().collect();
    nodes.reverse();
    let mut path = Path::new();
    for node in &nodes {
        match node.kind() {
            SyntaxKind::TABLE | SyntaxKind::ARRAY_TABLE => {
                path = doc
                    .dom
                    .table_path(node.text_range().start())
                    .cloned()
                    .unwrap_or_else(|| header_path(*node));
            }
            SyntaxKind::TABLE_HEADER | SyntaxKind::ARRAY_TABLE_HEADER => {
                let closed = node
                    .child_tokens()
                    .find(|t| matches!(t.kind(), SyntaxKind::R_BRACK | SyntaxKind::R_BRACK2))
                    .is_some_and(|t| offset >= t.text_range().end());
                if closed {
                    return none(path);
                }
                return key_position(node.child_node(SyntaxKind::KEY), offset, Path::new(), true);
            }
            SyntaxKind::ENTRY => {
                let key = node.child_node(SyntaxKind::KEY);
                match node.child_token(SyntaxKind::EQ) {
                    Some(eq) if offset >= eq.text_range().end() => {
                        if let Some(key) = key.and_then(ast::Key::cast) {
                            path.extend(key.path().into_iter().map(PathSegment::Key));
                        }
                    }
                    _ => return key_position(key, offset, path, false),
                }
            }
            SyntaxKind::ARRAY => {
                let values: Vec<TextRange> = node
                    .children_with_tokens()
                    .filter(|e| is_value(*e))
                    .map(|e| e.text_range())
                    .collect();
                let index = values
                    .iter()
                    .position(|r| r.contains_inclusive(offset))
                    .unwrap_or_else(|| values.iter().filter(|r| r.end() < offset).count());
                path.push(PathSegment::Index(index));
            }
            _ => {}
        }
    }
    let innermost = nodes.last().map_or(SyntaxKind::ROOT, |n| n.kind());
    match innermost {
        SyntaxKind::ENTRY | SyntaxKind::ARRAY => {
            let range = if token.kind().is_scalar() || token.kind() == SyntaxKind::UNKNOWN {
                token.text_range()
            } else {
                TextRange::empty(offset)
            };
            Position { path, kind: PositionKind::Value { range } }
        }
        SyntaxKind::ROOT
        | SyntaxKind::TABLE
        | SyntaxKind::ARRAY_TABLE
        | SyntaxKind::INLINE_TABLE => {
            // After something on the line, e.g. `[table] |`: nothing to write.
            if before_on_line(token, offset) {
                return none(path);
            }
            Position {
                path,
                kind: PositionKind::Key {
                    range: TextRange::empty(offset),
                    segment: None,
                    header: false,
                },
            }
        }
        _ => none(path),
    }
}

/// Whether a significant token precedes `offset` on its line, outside of
/// the token itself.
fn before_on_line(token: SyntaxToken<'_>, offset: TextSize) -> bool {
    let mut t = if token.text_range().start() < offset { Some(token) } else { token.prev_token() };
    while let Some(tok) = t {
        match tok.kind() {
            SyntaxKind::NEWLINE => return false,
            SyntaxKind::WHITESPACE => t = tok.prev_token(),
            SyntaxKind::L_BRACE | SyntaxKind::COMMA => return false,
            _ => return true,
        }
    }
    false
}

/// The `=` before `offset` on its line, with only blanks between.
fn equals_before(token: SyntaxToken<'_>) -> Option<SyntaxToken<'_>> {
    let mut t = token.prev_token();
    while let Some(tok) = t {
        match tok.kind() {
            SyntaxKind::WHITESPACE => t = tok.prev_token(),
            SyntaxKind::EQ => return Some(tok),
            _ => return None,
        }
    }
    None
}

/// A position in a key: the dotted segments before the cursor extend `path`.
fn key_position(
    key: Option<SyntaxNode<'_>>,
    offset: TextSize,
    mut path: Path,
    header: bool,
) -> Position {
    let key_kind = |range, segment| PositionKind::Key { range, segment, header };
    let Some(key) = key else {
        return Position { path, kind: key_kind(TextRange::empty(offset), None) };
    };
    let mut last: Option<String> = None;
    for token in key.child_tokens() {
        let range = token.text_range();
        if range.start() > offset {
            break;
        }
        match token.kind() {
            SyntaxKind::DOT if range.start() < offset => {
                if let Some(seg) = last.take() {
                    path.push(PathSegment::Key(seg));
                }
            }
            k if k.is_trivia() => {}
            SyntaxKind::DOT => {}
            _ => {
                let value = ast::decode(token).into_owned();
                if range.contains_inclusive(offset) {
                    return Position { path, kind: key_kind(range, Some(value)) };
                }
                last = Some(value);
            }
        }
    }
    if last.is_some() {
        // `key |`: the key is complete.
        return Position { path, kind: PositionKind::None };
    }
    Position { path, kind: key_kind(TextRange::empty(offset), None) }
}

/// The path of a table header, when the model has none for it.
fn header_path(table: SyntaxNode<'_>) -> Path {
    ast::Table::cast(table)
        .and_then(|t| t.header())
        .and_then(|h| h.key())
        .map(|k| k.path().into_iter().map(PathSegment::Key).collect())
        .unwrap_or_default()
}

fn is_value(e: SyntaxElement<'_>) -> bool {
    match e {
        SyntaxElement::Token(t) => t.kind().is_scalar() || t.kind() == SyntaxKind::UNKNOWN,
        SyntaxElement::Node(n) => matches!(n.kind(), SyntaxKind::ARRAY | SyntaxKind::INLINE_TABLE),
    }
}
