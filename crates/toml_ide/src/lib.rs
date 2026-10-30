//! Editor features for TOML documents.
//!
//! Everything here is pure: a [`Document`] holds the text and what is
//! derived from it, and the features take the schema, if any, from the
//! caller, who fetches it. Offsets are UTF-8 byte offsets; [`LineIndex`]
//! converts them to the line/UTF-16 column positions of the protocol.

mod cargo;
mod completion;
mod diagnostics;
mod features;
mod fixes;
mod folding;
mod hover;
mod line_index;
mod links;
mod position;
mod schema_location;
mod semantic_tokens;
mod sort;
mod symbols;

#[cfg(test)]
mod tests;

use std::sync::Arc;

pub use toml_dom::{Dom, Path, PathSegment};
pub use toml_parser::{SyntaxError, SyntaxTree, TextRange, TextSize};
pub use toml_schema::{Registry, SchemaRef};

pub use crate::{
    cargo::{
        CargoContext, CargoHover, CargoQuery, CargoTarget, CrateSummary, CrateVersion,
        DependencyStyle, WorkspaceDependency, WorkspaceRef, cargo_completions, cargo_context,
        cargo_dependency_fixes, cargo_dependency_key_completions, cargo_docs_links,
        cargo_hover_context, cargo_hover_markdown, cargo_local_crate, cargo_path_dependency,
        cargo_sort_fixes, cargo_workspace_dependency, cargo_workspace_hover,
        cargo_workspace_locals, cargo_workspace_ref,
    },
    completion::{CompletionItem, CompletionKind, completions},
    diagnostics::{Diagnostic, SchemaState, Severity, diagnostics},
    features::{cargo_feature_completions, cargo_feature_diagnostics},
    fixes::{Fix, key_style_fixes, sort_keys_fix},
    folding::{Fold, FoldKind, folding_ranges},
    hover::{Hover, hover},
    line_index::{LineCol, LineIndex},
    links::{PathString, path_strings},
    position::{Position, PositionKind, position_at},
    schema_location::{SchemaLocation, SchemaSource, resolve_location, schema_location},
    semantic_tokens::{SemanticToken, TokenType, semantic_tokens},
    sort::sort_alphabetically_fix,
    symbols::{Symbol, SymbolKind, document_symbols},
};

/// The schema of a document, ready for validation and lookups.
#[derive(Clone, Copy)]
pub struct Schema<'r> {
    /// The documents.
    pub registry: &'r Registry,
    /// The URL of the root schema.
    pub url: &'r str,
}

impl<'r> Schema<'r> {
    fn root(&self) -> Option<SchemaRef<'r>> {
        self.registry.root(self.url)
    }
}

/// A TOML document and everything derived from its text.
#[derive(Clone, Debug)]
pub struct Document {
    /// The text.
    pub text: Arc<str>,
    /// Positions of the text.
    pub line_index: Arc<LineIndex>,
    /// The syntax tree.
    pub tree: SyntaxTree,
    /// Lexical and syntax errors.
    pub errors: Vec<SyntaxError>,
    /// The data.
    pub dom: Dom,
}

impl Document {
    /// Parses `text`.
    pub fn new(text: impl Into<Arc<str>>) -> Document {
        let text: Arc<str> = text.into();
        let parse = toml_parser::parse(&text);
        let dom = Dom::build(&parse);
        let line_index = Arc::new(LineIndex::new(&text));
        let (tree, errors) = parse.into_parts();
        Document { text, line_index, tree, errors, dom }
    }

    /// The root of the syntax tree.
    pub fn root(&self) -> toml_parser::SyntaxNode<'_> {
        self.tree.root(&self.text)
    }

    /// The range of the key, or else of the item, at `path`.
    pub fn locate(&self, path: &[PathSegment], key: bool) -> TextRange {
        let mut range = TextRange::empty(TextSize::new(0));
        let mut item: Option<&toml_dom::Item>;
        let mut table = Some(&self.dom.root);
        let mut array: Option<&toml_dom::Array> = None;
        for (i, seg) in path.iter().enumerate() {
            let last = i + 1 == path.len();
            match seg {
                PathSegment::Key(k) => {
                    let Some(entry) = table.and_then(|t| t.entry(k)) else { break };
                    range = if last && key { entry.key_range } else { entry.item.range() };
                    item = Some(&entry.item);
                }
                PathSegment::Index(n) => {
                    let Some(it) = array.and_then(|a| a.items.get(*n)) else { break };
                    range = it.range();
                    item = Some(it);
                }
            }
            table = None;
            array = None;
            match item {
                Some(toml_dom::Item::Table(t)) => table = Some(t),
                Some(toml_dom::Item::Array(a)) => array = Some(a),
                _ => {}
            }
        }
        range
    }
}

/// Writes a JSON value as a TOML value.
pub fn json_to_toml(value: &serde_json::Value) -> String {
    use serde_json::Value;
    match value {
        Value::Null => "\"\"".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => quote(s),
        Value::Array(items) => {
            format!("[{}]", items.iter().map(json_to_toml).collect::<Vec<_>>().join(", "))
        }
        Value::Object(map) if map.is_empty() => "{}".to_owned(),
        Value::Object(map) => format!(
            "{{ {} }}",
            map.iter()
                .map(|(k, v)| format!("{} = {}", key(k), json_to_toml(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// A key as TOML writes it: bare when possible.
pub fn key(k: &str) -> String {
    if toml_lexer::is_bare_key(k) { k.to_owned() } else { quote(k) }
}

/// A basic string.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
