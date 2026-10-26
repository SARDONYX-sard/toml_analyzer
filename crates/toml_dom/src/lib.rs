//! The data model of a TOML document.
//!
//! [`Dom::build`] turns a syntax tree into nested tables, arrays and values,
//! remembering where each one is defined, and reports what the grammar alone
//! cannot see: keys defined twice, tables defined twice, dotted keys extending
//! a table defined by a header, headers extending inline tables or static
//! arrays, ...
//!
//! The model is lenient: on an error, the offending definition is dropped and
//! everything else is kept, so that a single mistake does not hide the rest
//! of the document from schema validation and editor features.

mod build;

#[cfg(test)]
mod tests;

use std::fmt;

use rustc_hash::FxHashMap;
use toml_parser::{Parse, TextRange};

/// A semantic error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DomError {
    /// What is wrong.
    pub message: String,
    /// Where.
    pub range: TextRange,
    /// The previous definition, for duplicates.
    pub related: Option<(TextRange, String)>,
    /// The definition at `range` is a duplicate entry that can be removed.
    pub removable: bool,
}

/// The data of a document, see the crate documentation.
#[derive(Clone, Debug, Default)]
pub struct Dom {
    /// The root table.
    pub root: Table,
    /// The semantic errors, in document order.
    pub errors: Vec<DomError>,
    /// The path of each `[table]` and `[[array.of.tables]]` node of the tree,
    /// by the range of the node. Headers in error are missing.
    pub table_paths: Vec<(TextRange, Path)>,
}

impl Dom {
    /// Builds the model of a parsed document.
    pub fn build(parse: &Parse<'_>) -> Dom {
        build::build(parse)
    }

    /// The item at `path`.
    pub fn get(&self, path: &[PathSegment]) -> Option<&Item> {
        let mut item = None;
        let mut current: ItemRef<'_> = ItemRef::Table(&self.root);
        for seg in path {
            let next = match (current, seg) {
                (ItemRef::Table(t), PathSegment::Key(k)) => t.get(k)?,
                (ItemRef::Array(a), PathSegment::Index(i)) => a.items.get(*i)?,
                _ => return None,
            };
            item = Some(next);
            current = next.as_ref();
        }
        item
    }

    /// The path of the `[table]` node whose range starts at `start`.
    pub fn table_path(&self, start: toml_parser::TextSize) -> Option<&Path> {
        self.table_paths.iter().find(|(r, _)| r.start() == start).map(|(_, p)| p)
    }

    /// The whole document as JSON.
    pub fn to_json(&self) -> serde_json::Value {
        self.root.to_json()
    }
}

/// A step of a path into the document.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PathSegment {
    /// A key of a table.
    Key(String),
    /// An index of an array.
    Index(usize),
}

impl fmt::Display for PathSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathSegment::Key(k) if toml_lexer::is_bare_key(k) => f.write_str(k),
            PathSegment::Key(k) => write!(f, "{k:?}"),
            PathSegment::Index(i) => write!(f, "[{i}]"),
        }
    }
}

/// A path from the root of the document.
pub type Path = Vec<PathSegment>;

/// Renders a path as TOML would write it: `a."b c"[0].d`.
pub fn display_path(path: &[PathSegment]) -> String {
    let mut out = String::new();
    for seg in path {
        if matches!(seg, PathSegment::Key(_)) && !out.is_empty() {
            out.push('.');
        }
        out.push_str(&seg.to_string());
    }
    out
}

/// A table, an array or a value.
#[derive(Clone, Debug)]
pub enum Item {
    /// A table, of any kind.
    Table(Table),
    /// An array or an array of tables.
    Array(Array),
    /// Anything else.
    Value(Value),
}

/// A borrowed [`Item`], to walk the document.
#[derive(Clone, Copy, Debug)]
pub enum ItemRef<'a> {
    /// A table.
    Table(&'a Table),
    /// An array.
    Array(&'a Array),
    /// A value.
    Value(&'a Value),
}

impl Item {
    /// The borrowed variant.
    pub fn as_ref(&self) -> ItemRef<'_> {
        match self {
            Item::Table(t) => ItemRef::Table(t),
            Item::Array(a) => ItemRef::Array(a),
            Item::Value(v) => ItemRef::Value(v),
        }
    }

    /// Where the item is defined: the value, the inline table, or the key
    /// of the header that defines it.
    pub fn range(&self) -> TextRange {
        match self {
            Item::Table(t) => t.range,
            Item::Array(a) => a.range,
            Item::Value(v) => v.range,
        }
    }

    /// The item as JSON. Dates and times become strings, invalid values
    /// `null`.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Item::Table(t) => t.to_json(),
            Item::Array(a) => serde_json::Value::Array(a.items.iter().map(Item::to_json).collect()),
            Item::Value(v) => v.to_json(),
        }
    }

    /// The JSON type name of the item, as JSON Schema uses it.
    pub fn type_name(&self) -> &'static str {
        match self {
            Item::Table(_) => "object",
            Item::Array(_) => "array",
            Item::Value(v) => v.kind.type_name(),
        }
    }
}

/// How a table came to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TableKind {
    /// The document itself.
    #[default]
    Root,
    /// A parent created by a header (`a` of `[a.b]`), not defined yet.
    Implicit,
    /// `[table]`
    Header,
    /// A parent created by a dotted key (`a` of `a.b = 1`).
    Dotted,
    /// `{ inline = "table" }`, which cannot be extended.
    Inline,
    /// An element of an `[[array.of.tables]]`.
    ArrayElement,
}

/// A table.
#[derive(Clone, Debug, Default)]
pub struct Table {
    /// How the table was defined.
    pub kind: TableKind,
    /// Where it is defined, see [`Item::range`]. Empty for the root.
    pub range: TextRange,
    entries: Vec<Entry>,
    index: FxHashMap<String, usize>,
}

/// A key and its item.
#[derive(Clone, Debug)]
pub struct Entry {
    /// The decoded key.
    pub key: String,
    /// The first definition of the key: the key segment of an entry or a
    /// header.
    pub key_range: TextRange,
    /// The item.
    pub item: Item,
}

impl Table {
    fn new(kind: TableKind, range: TextRange) -> Table {
        Table { kind, range, entries: Vec::new(), index: FxHashMap::default() }
    }

    /// The entries, in definition order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The entry of `key`.
    pub fn entry(&self, key: &str) -> Option<&Entry> {
        self.index.get(key).map(|&i| &self.entries[i])
    }

    /// The item of `key`.
    pub fn get(&self, key: &str) -> Option<&Item> {
        self.entry(key).map(|e| &e.item)
    }

    fn get_mut(&mut self, key: &str) -> Option<&mut Entry> {
        self.index.get(key).map(|&i| &mut self.entries[i])
    }

    fn insert(&mut self, key: String, key_range: TextRange, item: Item) -> &mut Item {
        let i = self.entries.len();
        self.index.insert(key.clone(), i);
        self.entries.push(Entry { key, key_range, item });
        &mut self.entries[i].item
    }

    /// The number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The table as a JSON object.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Object(
            self.entries.iter().map(|e| (e.key.clone(), e.item.to_json())).collect(),
        )
    }
}

/// How an array came to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrayKind {
    /// `[1, 2]`, which cannot be extended.
    Inline,
    /// `[[array.of.tables]]`
    OfTables,
}

/// An array.
#[derive(Clone, Debug)]
pub struct Array {
    /// How the array was defined.
    pub kind: ArrayKind,
    /// The inline array, or the key of the first `[[header]]`.
    pub range: TextRange,
    /// The elements.
    pub items: Vec<Item>,
}

/// A scalar value.
#[derive(Clone, Debug)]
pub struct Value {
    /// The value.
    pub kind: ValueKind,
    /// The token.
    pub range: TextRange,
}

/// The kinds of dates and times.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateTimeKind {
    /// `1979-05-27T07:32:00Z`
    OffsetDateTime,
    /// `1979-05-27T07:32:00`
    LocalDateTime,
    /// `1979-05-27`
    LocalDate,
    /// `07:32:00`
    LocalTime,
}

/// The decoded value.
#[derive(Clone, Debug)]
pub enum ValueKind {
    /// A string.
    String(String),
    /// An integer.
    Integer(i64),
    /// A float.
    Float(f64),
    /// A boolean.
    Bool(bool),
    /// A date, a time or both, as written.
    DateTime(DateTimeKind, String),
    /// A malformed or missing value, already reported as a syntax error.
    Invalid,
}

impl ValueKind {
    /// The JSON type name, as JSON Schema uses it. Dates are strings.
    pub fn type_name(&self) -> &'static str {
        match self {
            ValueKind::String(_) | ValueKind::DateTime(..) => "string",
            ValueKind::Integer(_) => "integer",
            ValueKind::Float(_) => "number",
            ValueKind::Bool(_) => "boolean",
            ValueKind::Invalid => "null",
        }
    }
}

impl Value {
    /// The value as JSON.
    pub fn to_json(&self) -> serde_json::Value {
        match &self.kind {
            ValueKind::String(s) => serde_json::Value::String(s.clone()),
            ValueKind::Integer(i) => serde_json::Value::from(*i),
            ValueKind::Float(f) => serde_json::Number::from_f64(*f)
                .map_or(serde_json::Value::Null, serde_json::Value::Number),
            ValueKind::Bool(b) => serde_json::Value::Bool(*b),
            ValueKind::DateTime(_, s) => serde_json::Value::String(s.clone()),
            ValueKind::Invalid => serde_json::Value::Null,
        }
    }
}
