//! Where a document says its schema is.
//!
//! Two forms are recognized, the first one winning:
//!
//! - a `#:schema <url or path>` comment before the first table;
//! - a root `"$schema" = "<url or path>"` entry.
//!
//! The location is a URL (`https://...`, `file:///...`) or a file path,
//! absolute or relative to the directory of the document.

use toml_dom::{Item, ValueKind};
use toml_parser::{SyntaxElement, SyntaxKind, TextRange, TextSize};
use url::Url;

use crate::Document;

/// Which form named the schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaSource {
    /// `#:schema ...`
    Directive,
    /// `"$schema" = "..."`
    Key,
}

/// The schema named by a document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaLocation {
    /// The URL or path, as written (decoded for the key).
    pub text: String,
    /// The range of the text: the comment part, or the string value.
    pub range: TextRange,
    /// The form.
    pub source: SchemaSource,
}

/// The prefix of the schema directive.
const DIRECTIVE: &str = "#:schema";

/// The schema named by `doc`, if any.
pub fn schema_location(doc: &Document) -> Option<SchemaLocation> {
    for child in doc.root().children_with_tokens() {
        match child {
            SyntaxElement::Node(n)
                if matches!(n.kind(), SyntaxKind::TABLE | SyntaxKind::ARRAY_TABLE) =>
            {
                break;
            }
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::COMMENT => {
                let Some(rest) = t.text().strip_prefix(DIRECTIVE) else { continue };
                if !rest.starts_with([' ', '\t']) {
                    continue;
                }
                let lead = rest.len() - rest.trim_start().len();
                let text = rest.trim();
                if text.is_empty() {
                    continue;
                }
                let start = t.text_range().start() + TextSize::new((DIRECTIVE.len() + lead) as u32);
                return Some(SchemaLocation {
                    text: text.to_owned(),
                    range: TextRange::at(start, TextSize::of(text)),
                    source: SchemaSource::Directive,
                });
            }
            _ => {}
        }
    }
    let entry = doc.dom.root.entry("$schema")?;
    match &entry.item {
        Item::Value(v) => match &v.kind {
            ValueKind::String(s) if !s.trim().is_empty() => Some(SchemaLocation {
                text: s.trim().to_owned(),
                range: v.range,
                source: SchemaSource::Key,
            }),
            _ => None,
        },
        _ => None,
    }
}

/// Resolves a schema location against the URL of the document.
pub fn resolve_location(text: &str, doc_url: Option<&Url>) -> Result<Url, String> {
    let text = text.trim();
    if has_scheme(text) {
        return Url::parse(text).map_err(|e| format!("invalid schema URL `{text}`: {e}"));
    }
    let path = std::path::Path::new(text);
    if path.is_absolute() || text.starts_with('/') {
        return Url::from_file_path(path).map_err(|()| format!("invalid schema path `{text}`"));
    }
    let Some(base) = doc_url else {
        return Err(format!(
            "cannot resolve the relative schema path `{text}` of an unsaved document"
        ));
    };
    base.join(&text.replace('\\', "/")).map_err(|e| format!("invalid schema path `{text}`: {e}"))
}

/// `scheme:` with a scheme longer than one letter: `C:\` is a path.
fn has_scheme(text: &str) -> bool {
    match text.split_once(':') {
        Some((scheme, _)) => {
            scheme.len() > 1
                && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        }
        None => false,
    }
}
