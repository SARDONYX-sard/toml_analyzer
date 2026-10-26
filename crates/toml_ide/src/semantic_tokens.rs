//! Semantic highlighting: what the grammar cannot tell, such as table names
//! versus keys.

use toml_parser::{SyntaxKind, TextRange};

use crate::Document;

/// The token types, in the order of the legend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum TokenType {
    /// A table name in a header.
    Namespace,
    /// A key.
    Property,
    /// A string.
    String,
    /// A number.
    Number,
    /// `true`, `false`.
    Keyword,
    /// A date or time.
    Date,
    /// A comment.
    Comment,
    /// `#:schema` directives.
    Macro,
}

impl TokenType {
    /// The types and their protocol names, in legend order.
    pub const ALL: &[(TokenType, &str)] = &[
        (TokenType::Namespace, "namespace"),
        (TokenType::Property, "property"),
        (TokenType::String, "string"),
        (TokenType::Number, "number"),
        (TokenType::Keyword, "keyword"),
        (TokenType::Date, "regexp"),
        (TokenType::Comment, "comment"),
        (TokenType::Macro, "macro"),
    ];
}

/// A highlighted token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SemanticToken {
    /// Where.
    pub range: TextRange,
    /// What.
    pub ty: TokenType,
}

/// The semantic tokens of `doc` within `range`, in order.
pub fn semantic_tokens(doc: &Document, range: Option<TextRange>) -> Vec<SemanticToken> {
    let mut out = Vec::new();
    for token in doc.root().descendant_tokens() {
        let r = token.text_range();
        if range.is_some_and(|range| r.end() < range.start() || r.start() > range.end()) {
            continue;
        }
        let in_key = token.parent().is_some_and(|p| p.kind() == SyntaxKind::KEY);
        let in_header = in_key
            && token.parent().and_then(|k| k.parent()).is_some_and(|h| {
                matches!(h.kind(), SyntaxKind::TABLE_HEADER | SyntaxKind::ARRAY_TABLE_HEADER)
            });
        let ty = match token.kind() {
            SyntaxKind::DOT => continue,
            _ if in_header => TokenType::Namespace,
            _ if in_key => TokenType::Property,
            k if k.is_string() => TokenType::String,
            SyntaxKind::INTEGER | SyntaxKind::FLOAT => TokenType::Number,
            SyntaxKind::BOOL => TokenType::Keyword,
            k if k.is_date_time() => TokenType::Date,
            SyntaxKind::COMMENT if token.text().starts_with("#:schema") => TokenType::Macro,
            SyntaxKind::COMMENT => TokenType::Comment,
            _ => continue,
        };
        out.push(SemanticToken { range: r, ty });
    }
    out
}
