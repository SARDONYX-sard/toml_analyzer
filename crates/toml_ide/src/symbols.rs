//! The outline of a document.

use toml_dom::{Item, Path, PathSegment, Table, TableKind, ValueKind};
use toml_parser::TextRange;

use crate::Document;

/// The kind of a symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolKind {
    /// A table.
    Table,
    /// An array.
    Array,
    /// A string.
    String,
    /// A number.
    Number,
    /// A boolean.
    Boolean,
    /// A date or time.
    DateTime,
    /// A malformed value.
    Invalid,
}

/// A key of the outline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    /// The key, or `[n]` for elements of arrays of tables.
    pub name: String,
    /// The kind.
    pub kind: SymbolKind,
    /// The whole definition.
    pub range: TextRange,
    /// The key.
    pub selection_range: TextRange,
    /// The keys of tables.
    pub children: Vec<Symbol>,
}

/// The outline of `doc`.
pub fn document_symbols(doc: &Document) -> Vec<Symbol> {
    table_symbols(doc, &doc.dom.root, &mut Path::new())
}

fn table_symbols(doc: &Document, table: &Table, path: &mut Path) -> Vec<Symbol> {
    table
        .entries()
        .iter()
        .map(|e| {
            path.push(PathSegment::Key(e.key.clone()));
            let symbol = item_symbol(doc, e.key.clone(), &e.item, e.key_range, path);
            path.pop();
            symbol
        })
        .collect()
}

fn item_symbol(
    doc: &Document,
    name: String,
    item: &Item,
    key_range: TextRange,
    path: &mut Path,
) -> Symbol {
    let (kind, children) = match item {
        Item::Table(t) => (SymbolKind::Table, table_symbols(doc, t, path)),
        Item::Array(a) => {
            let children = a
                .items
                .iter()
                .enumerate()
                .filter(|(_, i)| matches!(i, Item::Table(t) if t.kind == TableKind::ArrayElement))
                .map(|(n, i)| {
                    path.push(PathSegment::Index(n));
                    let s = item_symbol(doc, format!("[{n}]"), i, i.range(), path);
                    path.pop();
                    s
                })
                .collect();
            (SymbolKind::Array, children)
        }
        Item::Value(v) => (
            match v.kind {
                ValueKind::String(_) => SymbolKind::String,
                ValueKind::Integer(_) | ValueKind::Float(_) => SymbolKind::Number,
                ValueKind::Bool(_) => SymbolKind::Boolean,
                ValueKind::DateTime(..) => SymbolKind::DateTime,
                ValueKind::Invalid => SymbolKind::Invalid,
            },
            Vec::new(),
        ),
    };
    let mut range = key_range.cover(item.range());
    // A `[table]` spans its section.
    for (node, p) in &doc.dom.table_paths {
        if p == path {
            range = range.cover(*node);
        }
    }
    for child in &children {
        range = range.cover(child.range);
    }
    Symbol { name, kind, range, selection_range: key_range, children }
}
