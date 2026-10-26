//! Building the [`Dom`] from the syntax tree.

use toml_parser::{
    Parse, SyntaxKind, TextRange, TextSize,
    ast::{self, AstNode},
};

use crate::{
    Array, ArrayKind, DateTimeKind, Dom, DomError, Item, Path, PathSegment, Table, TableKind,
    Value, ValueKind, display_path,
};

pub(crate) fn build(parse: &Parse<'_>) -> Dom {
    let mut b = Builder::default();
    let mut root = Table::new(TableKind::Root, TextRange::empty(TextSize::new(0)));
    let doc = parse.root();
    for entry in doc.entries() {
        b.entry(&mut root, entry);
    }
    for table in doc.tables() {
        let node_range = table.syntax().text_range();
        let path = table
            .header()
            .and_then(|h| h.key())
            .and_then(|k| b.header(&mut root, k, table.is_array()));
        let target = path.as_ref().and_then(|p| table_at_mut(&mut root, p));
        match target {
            Some(target) => {
                for entry in table.entries() {
                    b.entry(target, entry);
                }
            }
            None => {
                // Still check the entries, in a table of their own.
                let mut scratch = Table::new(TableKind::Header, node_range);
                for entry in table.entries() {
                    b.entry(&mut scratch, entry);
                }
            }
        }
        if let Some(path) = path {
            b.table_paths.push((node_range, path));
        }
    }
    b.errors.sort_by_key(|e| (e.range.start(), e.range.end()));
    Dom { root, errors: b.errors, table_paths: b.table_paths }
}

/// The table at `path`, following the last element of arrays of tables.
fn table_at_mut<'t>(mut table: &'t mut Table, path: &[PathSegment]) -> Option<&'t mut Table> {
    let mut i = 0;
    while i < path.len() {
        let PathSegment::Key(key) = &path[i] else { return None };
        let entry = table.get_mut(key)?;
        table = match &mut entry.item {
            Item::Table(t) => t,
            Item::Array(a) => {
                i += 1;
                let Some(PathSegment::Index(n)) = path.get(i) else { return None };
                match a.items.get_mut(*n)? {
                    Item::Table(t) => t,
                    _ => return None,
                }
            }
            Item::Value(_) => return None,
        };
        i += 1;
    }
    Some(table)
}

#[derive(Default)]
struct Builder {
    errors: Vec<DomError>,
    table_paths: Vec<(TextRange, Path)>,
}

fn segments(key: ast::Key<'_>) -> Vec<(String, TextRange)> {
    key.segments().map(|s| (s.value().into_owned(), s.text_range())).collect()
}

impl Builder {
    fn error(&mut self, range: TextRange, message: String, related: Option<(TextRange, &str)>) {
        self.errors.push(DomError {
            message,
            range,
            related: related.map(|(r, m)| (r, m.to_owned())),
            removable: false,
        });
    }

    /// Defines the table of a header; returns its path.
    fn header(&mut self, root: &mut Table, key: ast::Key<'_>, array: bool) -> Option<Path> {
        let segs = segments(key);
        let ((name, range), parents) = segs.split_last()?;
        let mut path = Path::new();
        let mut table = root;
        for (parent, prange) in parents {
            if table.get(parent).is_none() {
                table.insert(
                    parent.clone(),
                    *prange,
                    Item::Table(Table::new(TableKind::Implicit, *prange)),
                );
            }
            path.push(PathSegment::Key(parent.clone()));
            let entry = table.get_mut(parent)?;
            let first = entry.key_range;
            table = match &mut entry.item {
                Item::Table(t) if t.kind == TableKind::Inline => {
                    let message =
                        format!("cannot extend the inline table `{}`", display_path(&path));
                    self.error(*prange, message, Some((first, "defined here")));
                    return None;
                }
                Item::Table(t) => t,
                Item::Array(a) if a.kind == ArrayKind::OfTables => {
                    path.push(PathSegment::Index(a.items.len().saturating_sub(1)));
                    match a.items.last_mut() {
                        Some(Item::Table(t)) => t,
                        _ => return None,
                    }
                }
                Item::Array(_) => {
                    let message = format!("cannot extend the array `{}`", display_path(&path));
                    self.error(*prange, message, Some((first, "defined here")));
                    return None;
                }
                Item::Value(_) => {
                    let message = format!("`{}` is a value, not a table", display_path(&path));
                    self.error(*prange, message, Some((first, "defined here")));
                    return None;
                }
            };
        }
        path.push(PathSegment::Key(name.clone()));
        let shown = display_path(&path);

        if array {
            if table.get(name).is_none() {
                let array = Array { kind: ArrayKind::OfTables, range: *range, items: Vec::new() };
                table.insert(name.clone(), *range, Item::Array(array));
            }
            let entry = table.get_mut(name)?;
            let first = entry.key_range;
            match &mut entry.item {
                Item::Array(a) if a.kind == ArrayKind::OfTables => {
                    a.items.push(Item::Table(Table::new(TableKind::ArrayElement, *range)));
                    path.push(PathSegment::Index(a.items.len() - 1));
                    Some(path)
                }
                Item::Array(_) => {
                    let message = format!("cannot append to the static array `{shown}`");
                    self.error(*range, message, Some((first, "defined here")));
                    None
                }
                Item::Table(_) => {
                    let message = format!("`{shown}` is already defined as a table");
                    self.error(*range, message, Some((first, "defined here")));
                    None
                }
                Item::Value(_) => {
                    let message = format!("`{shown}` is already defined as a value");
                    self.error(*range, message, Some((first, "defined here")));
                    None
                }
            }
        } else {
            if table.get(name).is_none() {
                table.insert(
                    name.clone(),
                    *range,
                    Item::Table(Table::new(TableKind::Header, *range)),
                );
                return Some(path);
            }
            let entry = table.get_mut(name)?;
            let first = entry.key_range;
            let message = match &mut entry.item {
                Item::Table(t) if t.kind == TableKind::Implicit => {
                    t.kind = TableKind::Header;
                    t.range = *range;
                    return Some(path);
                }
                Item::Table(t) if t.kind == TableKind::Dotted => {
                    format!("the table `{shown}` is already defined by dotted keys")
                }
                Item::Table(t) if t.kind == TableKind::Inline => {
                    format!("the table `{shown}` is already defined as an inline table")
                }
                Item::Table(_) => format!("the table `{shown}` is defined twice"),
                Item::Array(a) if a.kind == ArrayKind::OfTables => {
                    format!("`{shown}` is already defined as an array of tables")
                }
                Item::Array(_) | Item::Value(_) => {
                    format!("`{shown}` is already defined as a value")
                }
            };
            self.error(*range, message, Some((first, "first defined here")));
            None
        }
    }

    /// Adds `key = value` to `table`.
    fn entry(&mut self, table: &mut Table, entry: ast::Entry<'_>) {
        let Some(key) = entry.key() else { return };
        let segs = segments(key);
        let Some(((name, range), parents)) = segs.split_last() else { return };
        let item = match entry.value() {
            Some(value) => self.value(value),
            None => Item::Value(Value {
                kind: ValueKind::Invalid,
                range: TextRange::empty(range.end()),
            }),
        };

        let mut table = table;
        let mut path = Path::new();
        for (parent, prange) in parents {
            if table.get(parent).is_none() {
                table.insert(
                    parent.clone(),
                    *prange,
                    Item::Table(Table::new(TableKind::Dotted, *prange)),
                );
            }
            path.push(PathSegment::Key(parent.clone()));
            let Some(entry) = table.entry(parent) else { return };
            let first = entry.key_range;
            let message = match &entry.item {
                Item::Table(t) if t.kind == TableKind::Dotted => {
                    let Some(Item::Table(t)) = table.get_mut(parent).map(|e| &mut e.item) else {
                        return;
                    };
                    table = t;
                    continue;
                }
                Item::Table(t) if t.kind == TableKind::Inline => {
                    format!("cannot add keys to the inline table `{}`", display_path(&path))
                }
                Item::Table(_) => format!(
                    "the table `{}` is defined by a header and cannot be extended with dotted keys",
                    display_path(&path)
                ),
                Item::Array(_) => format!("`{}` is an array, not a table", display_path(&path)),
                Item::Value(_) => format!("`{}` is a value, not a table", display_path(&path)),
            };
            self.error(*prange, message, Some((first, "defined here")));
            return;
        }
        if let Some(previous) = table.entry(name) {
            path.push(PathSegment::Key(name.clone()));
            let message = format!("the key `{}` is defined twice", display_path(&path));
            self.error(*range, message, Some((previous.key_range, "first defined here")));
            if let Some(e) = self.errors.last_mut() {
                e.removable = true;
            }
            return;
        }
        table.insert(name.clone(), *range, item);
    }

    fn value(&mut self, value: ast::Value<'_>) -> Item {
        match value {
            ast::Value::Scalar(token) => {
                let text = token.text();
                let kind = match token.kind() {
                    k if k.is_string() => ValueKind::String(ast::decode(token).into_owned()),
                    SyntaxKind::INTEGER => toml_lexer::parse_integer(text)
                        .map_or(ValueKind::Invalid, ValueKind::Integer),
                    SyntaxKind::FLOAT => {
                        toml_lexer::parse_float(text).map_or(ValueKind::Invalid, ValueKind::Float)
                    }
                    SyntaxKind::BOOL => ValueKind::Bool(text == "true"),
                    SyntaxKind::OFFSET_DATE_TIME => {
                        ValueKind::DateTime(DateTimeKind::OffsetDateTime, text.to_owned())
                    }
                    SyntaxKind::LOCAL_DATE_TIME => {
                        ValueKind::DateTime(DateTimeKind::LocalDateTime, text.to_owned())
                    }
                    SyntaxKind::LOCAL_DATE => {
                        ValueKind::DateTime(DateTimeKind::LocalDate, text.to_owned())
                    }
                    SyntaxKind::LOCAL_TIME => {
                        ValueKind::DateTime(DateTimeKind::LocalTime, text.to_owned())
                    }
                    _ => ValueKind::Invalid,
                };
                Item::Value(Value { kind, range: token.text_range() })
            }
            ast::Value::Array(array) => {
                let items = array.values().map(|v| self.value(v)).collect();
                Item::Array(Array {
                    kind: ArrayKind::Inline,
                    range: array.syntax().text_range(),
                    items,
                })
            }
            ast::Value::InlineTable(inline) => {
                let mut table = Table::new(TableKind::Inline, inline.syntax().text_range());
                for entry in inline.entries() {
                    self.entry(&mut table, entry);
                }
                Item::Table(table)
            }
        }
    }
}
