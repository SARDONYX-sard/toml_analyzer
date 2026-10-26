//! Completion of keys, table names and values from the schema.

use serde_json::Value;
use toml_dom::{Item, PathSegment};
use toml_parser::{TextRange, TextSize};
use toml_schema::{SchemaInfo, expand, key_completions, schemas_at};

use crate::{Document, PositionKind, Schema, json_to_toml, key, position_at};

/// What a completion inserts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionKind {
    /// A key of a table.
    Key,
    /// A table name in a header.
    Table,
    /// A value.
    Value,
}

/// A completion proposal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionItem {
    /// What is shown.
    pub label: String,
    /// What is inserted, a snippet when `snippet` is set.
    pub insert: String,
    /// Whether `insert` is a snippet with `$1` placeholders.
    pub snippet: bool,
    /// The text replaced.
    pub range: TextRange,
    /// The kind.
    pub kind: CompletionKind,
    /// The type, shown next to the label.
    pub detail: Option<String>,
    /// Markdown documentation.
    pub documentation: Option<String>,
    /// Required keys first.
    pub sort: String,
    /// Marked deprecated by the schema.
    pub deprecated: bool,
    /// What the editor matches the typed text against, when not the label.
    pub filter: Option<String>,
}

/// The completions at `offset`. `snippets`: whether the editor supports
/// snippets.
pub fn completions(
    doc: &Document,
    offset: TextSize,
    schema: Schema<'_>,
    snippets: bool,
) -> Vec<CompletionItem> {
    let Some(root) = schema.root() else { return Vec::new() };
    let position = position_at(doc, offset);
    match position.kind {
        PositionKind::Key { range, segment, header } => {
            let schemas = schemas_at(schema.registry, &root, &position.path);
            let existing = existing_keys(doc, &position.path);
            let mut out = Vec::new();
            for k in key_completions(&schemas) {
                let info = SchemaInfo::of(&expand(schema.registry, vec![k.schema.clone()]));
                let is_table = info.types.iter().any(|t| t == "object")
                    || (info.types.iter().any(|t| t == "array")
                        && items_are_tables(schema, &k.schema));
                if header && !is_table {
                    continue;
                }
                // Already there, unless it is a table that a header may extend.
                let typing = segment.as_deref() == Some(k.key.as_str());
                if !typing && existing.iter().any(|e| e == &k.key) && !(header && is_table) {
                    continue;
                }
                let name = key(&k.key);
                let (insert, is_snippet) = if header || !snippets {
                    (name.clone(), false)
                } else {
                    (format!("{name} = {}", value_snippet(&info)), true)
                };
                out.push(CompletionItem {
                    label: k.key.clone(),
                    insert,
                    snippet: is_snippet,
                    range,
                    kind: if header { CompletionKind::Table } else { CompletionKind::Key },
                    detail: type_detail(&info),
                    documentation: info.description.clone(),
                    sort: format!("{}{}", if k.required { 0 } else { 1 }, k.key),
                    deprecated: info.deprecated,
                    filter: None,
                });
            }
            out
        }
        PositionKind::Value { range } => {
            let info = SchemaInfo::of(&schemas_at(schema.registry, &root, &position.path));
            value_items(&info, range)
        }
        PositionKind::None => Vec::new(),
    }
}

/// The values the schema suggests.
fn value_items(info: &SchemaInfo, range: TextRange) -> Vec<CompletionItem> {
    let mut values: Vec<Value> = info.values.clone();
    if info.types.iter().any(|t| t == "boolean") {
        values.extend([Value::Bool(true), Value::Bool(false)]);
    }
    values.extend(info.default.iter().cloned());
    values.extend(info.examples.iter().cloned());
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for (i, v) in values.into_iter().enumerate() {
        let text = json_to_toml(&v);
        if seen.contains(&text) {
            continue;
        }
        seen.push(text.clone());
        let is_default = info.default.as_ref() == Some(&v);
        out.push(CompletionItem {
            label: text.clone(),
            insert: text,
            snippet: false,
            range,
            kind: CompletionKind::Value,
            detail: is_default.then(|| "default".to_owned()),
            documentation: None,
            sort: format!("{i:04}"),
            deprecated: false,
            filter: None,
        });
    }
    out
}

/// A snippet for the value of a key.
fn value_snippet(info: &SchemaInfo) -> String {
    if !info.values.is_empty() && info.values.len() <= 20 {
        let choices: Vec<String> = info
            .values
            .iter()
            .map(|v| json_to_toml(v).replace('\\', "\\\\").replace(',', "\\,").replace('|', "\\|"))
            .collect();
        return format!("${{1|{}|}}", choices.join(","));
    }
    if let Some(default) = &info.default {
        return format!("${{1:{}}}", escape_snippet(&json_to_toml(default)));
    }
    match info.types.first().map(String::as_str) {
        Some("string") => "\"$1\"".to_owned(),
        Some("boolean") => "${1|true,false|}".to_owned(),
        Some("array") => "[$1]".to_owned(),
        Some("object") => "{ $1 }".to_owned(),
        _ => "$1".to_owned(),
    }
}

fn escape_snippet(s: &str) -> String {
    s.replace('\\', "\\\\").replace('$', "\\$").replace('}', "\\}")
}

fn type_detail(info: &SchemaInfo) -> Option<String> {
    (!info.types.is_empty()).then(|| info.types.join(" | "))
}

/// Whether the items of an array schema are tables: `[[key]]` headers.
fn items_are_tables(schema: Schema<'_>, array: &toml_schema::SchemaRef<'_>) -> bool {
    let items = schemas_at(schema.registry, array, &[PathSegment::Index(0)]);
    SchemaInfo::of(&items).types.iter().any(|t| t == "object")
}

/// The keys already defined in the table at `path`.
fn existing_keys(doc: &Document, path: &[PathSegment]) -> Vec<String> {
    let table = if path.is_empty() {
        Some(&doc.dom.root)
    } else {
        match doc.dom.get(path) {
            Some(Item::Table(t)) => Some(t),
            _ => None,
        }
    };
    table.map_or_else(Vec::new, |t| t.entries().iter().map(|e| e.key.clone()).collect())
}
