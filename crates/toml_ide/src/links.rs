//! Strings that look like file paths, which the editor can open.

use toml_parser::{SyntaxKind, TextRange, TextSize, ast};

use crate::Document;

/// A string value that may name a file or a directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathString {
    /// The path, decoded.
    pub path: String,
    /// The text between the quotes.
    pub range: TextRange,
}

/// The string values containing `/` or `\`, URLs excepted. The caller
/// checks that they exist.
pub fn path_strings(doc: &Document) -> Vec<PathString> {
    let mut out = Vec::new();
    for token in doc.root().descendant_tokens() {
        if !matches!(token.kind(), SyntaxKind::BASIC_STRING | SyntaxKind::LITERAL_STRING) {
            continue;
        }
        // Keys are not paths.
        if token.parent().is_some_and(|p| p.kind() == SyntaxKind::KEY) {
            continue;
        }
        let path = ast::decode(token).into_owned();
        if !path.contains(['/', '\\']) || path.contains("://") || path.contains('\n') {
            continue;
        }
        let r = token.text_range();
        let range = TextRange::new(
            r.start() + TextSize::new(1),
            (r.end() - TextSize::new(1)).max(r.start()),
        );
        out.push(PathString { path, range });
    }
    out
}
