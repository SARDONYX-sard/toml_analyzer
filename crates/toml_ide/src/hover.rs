//! Hover: what the schema says about a key or a value.

use toml_dom::display_path;
use toml_parser::{TextRange, TextSize};
use toml_schema::{SchemaInfo, schemas_at};

use crate::{Document, PositionKind, Schema, json_to_toml, position_at, schema_location};

/// A hover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hover {
    /// The hovered key or value.
    pub range: TextRange,
    /// Markdown.
    pub markdown: String,
}

/// The hover at `offset`.
pub fn hover(doc: &Document, offset: TextSize, schema: Option<Schema<'_>>) -> Option<Hover> {
    if let Some(location) = schema_location(doc)
        && location.range.contains_inclusive(offset)
    {
        let target = schema.map_or("not loaded", |s| s.url);
        return Some(Hover { range: location.range, markdown: format!("JSON Schema: <{target}>") });
    }
    let schema = schema?;
    let root = schema.root()?;
    let position = position_at(doc, offset);
    let (path, range) = match &position.kind {
        PositionKind::Key { range, segment: Some(_), .. } => (position.key_path()?, *range),
        PositionKind::Value { range } if !range.is_empty() => (position.path.clone(), *range),
        _ => return None,
    };
    let info = SchemaInfo::of(&schemas_at(schema.registry, &root, &path));
    let markdown = render(&display_path(&path), &info)?;
    Some(Hover { range, markdown })
}

fn render(name: &str, info: &SchemaInfo) -> Option<String> {
    if info.title.is_none()
        && info.description.is_none()
        && info.types.is_empty()
        && info.values.is_empty()
    {
        return None;
    }
    let mut out = String::new();
    match &info.title {
        Some(title) => out.push_str(&format!("**{title}**")),
        None => out.push_str(&format!("`{name}`")),
    }
    if !info.types.is_empty() {
        out.push_str(&format!(": `{}`", info.types.join(" | ")));
    }
    if info.deprecated {
        out.push_str(" *(deprecated)*");
    }
    if let Some(d) = &info.description {
        out.push_str("\n\n");
        out.push_str(d);
    }
    if let Some(default) = &info.default {
        out.push_str(&format!("\n\nDefault: `{}`", json_to_toml(default)));
    }
    if !info.values.is_empty() {
        let values: Vec<String> =
            info.values.iter().take(20).map(|v| format!("`{}`", json_to_toml(v))).collect();
        out.push_str(&format!("\n\nAllowed values: {}", values.join(", ")));
    }
    Some(out)
}
