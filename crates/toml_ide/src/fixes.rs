//! Quick fixes of diagnostics.

use serde_json::Value;
use toml_dom::{Item, Path, TableKind};
use toml_lexer::LexError;
use toml_parser::{SyntaxError, SyntaxKind, SyntaxNode, TextRange, TextSize};
use toml_schema::{Suggestion, Target, ValidationError};

use crate::{Document, json_to_toml, key, quote};

/// A fix: text edits, applied together.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    /// What the fix does.
    pub title: String,
    /// The edits, not overlapping.
    pub edits: Vec<(TextRange, String)>,
    /// The fix the editor may apply on its own ("auto fix").
    pub preferred: bool,
}

impl Fix {
    pub(crate) fn new(title: impl Into<String>, range: TextRange, text: impl Into<String>) -> Fix {
        Fix { title: title.into(), edits: vec![(range, text.into())], preferred: false }
    }

    pub(crate) fn preferred(mut self) -> Fix {
        self.preferred = true;
        self
    }
}

/// The fixes of a syntax error.
pub(crate) fn syntax_fixes(doc: &Document, e: &SyntaxError) -> Vec<Fix> {
    let text = &doc.text[e.range()];
    let quoted = || quote(text);
    if e.message() == LexError::InvalidValue.message() {
        vec![Fix::new(format!("Quote as {}", quoted()), e.range(), quoted()).preferred()]
    } else if e.message() == LexError::InvalidKey.message() {
        vec![Fix::new(format!("Quote the key as {}", quoted()), e.range(), quoted()).preferred()]
    } else {
        Vec::new()
    }
}

/// The fix of a duplicate key.
pub(crate) fn remove_duplicate(doc: &Document, range: TextRange) -> Vec<Fix> {
    removal(doc, range).map(|r| Fix::new("Remove the duplicate key", r, "")).into_iter().collect()
}

/// The fixes of a schema error.
pub(crate) fn schema_fixes(doc: &Document, e: &ValidationError) -> Vec<Fix> {
    let key_range = doc.locate(&e.path, true);
    let value_range = doc.locate(&e.path, false);
    let name = match e.path.last() {
        Some(toml_dom::PathSegment::Key(k)) => k.clone(),
        _ => String::new(),
    };
    let is_scalar = matches!(doc.dom.get(&e.path), Some(Item::Value(_)));
    let is_inline = match doc.dom.get(&e.path) {
        Some(Item::Table(t)) => t.kind == TableKind::Inline,
        Some(Item::Array(a)) => a.kind == toml_dom::ArrayKind::Inline,
        _ => false,
    };
    let value_text = &doc.text[value_range];
    let mut out = Vec::new();
    for (i, s) in e.suggestions.iter().enumerate() {
        let fix = match s {
            // Renaming onto an existing key would define it twice.
            Suggestion::RenameKey(new) if sibling_exists(doc, &e.path, new) => None,
            Suggestion::RenameKey(new)
                if e.target == Target::Key && !name.is_empty() && e.deprecated =>
            {
                Some(Fix::new(format!("Replace with `{new}`"), key_range, key(new)))
            }
            Suggestion::RenameKey(new) if e.target == Target::Key && !name.is_empty() => {
                Some(Fix::new(format!("Rename to `{new}`"), key_range, key(new)))
            }
            Suggestion::RemoveKey if !name.is_empty() => {
                let what = if e.deprecated { "deprecated " } else { "" };
                removal(doc, key_range).map(|r| Fix::new(format!("Remove {what}`{name}`"), r, ""))
            }
            Suggestion::AddKeys(keys) => add_keys(doc, &e.path, keys),
            Suggestion::ReplaceValue(v) if is_scalar => {
                let new = json_to_toml(v);
                Some(Fix::new(format!("Replace with `{new}`"), value_range, new))
            }
            Suggestion::Quote if is_scalar => Some(Fix::new(
                format!("Convert to the string {}", quote(value_text)),
                value_range,
                quote(value_text),
            )),
            Suggestion::Unquote(v) if is_scalar => {
                let new = json_to_toml(v);
                Some(Fix::new(format!("Convert to `{new}`"), value_range, new))
            }
            Suggestion::WrapInArray if is_scalar || is_inline => {
                Some(Fix::new("Wrap in an array", value_range, format!("[{value_text}]")))
            }
            _ => None,
        };
        // The first suggestion is the most likely; removing is never automatic.
        let auto = i == 0 && !matches!(s, Suggestion::RemoveKey);
        out.extend(fix.map(|f| if auto { f.preferred() } else { f }));
    }
    // A single choice among many values is no obvious fix.
    let replacements =
        e.suggestions.iter().filter(|s| matches!(s, Suggestion::ReplaceValue(_))).count();
    if replacements > 1 {
        for f in &mut out {
            f.preferred = false;
        }
    }
    out
}

/// Whether the table holding the item at `path` has the key `name`.
fn sibling_exists(doc: &Document, path: &Path, name: &str) -> bool {
    let parent = &path[..path.len().saturating_sub(1)];
    let table = if parent.is_empty() {
        Some(&doc.dom.root)
    } else {
        match doc.dom.get(parent) {
            Some(Item::Table(t)) => Some(t),
            _ => None,
        }
    };
    table.is_some_and(|t| t.get(name).is_some())
}

/// The range to delete to remove the entry, or the `[table]`, whose key is
/// at `key_range`: whole lines when the entry is alone on its line.
fn removal(doc: &Document, key_range: TextRange) -> Option<TextRange> {
    let root = doc.root();
    let token = root.token_at_offset(key_range.start()).right_biased()?;
    let node = token.ancestors().find(|n| {
        matches!(n.kind(), SyntaxKind::ENTRY | SyntaxKind::TABLE | SyntaxKind::ARRAY_TABLE)
    })?;
    match node.kind() {
        SyntaxKind::ENTRY => {
            let parent = node.parent()?;
            if parent.kind() == SyntaxKind::INLINE_TABLE {
                return Some(inline_removal(node));
            }
            Some(line_removal(doc, node.text_range()))
        }
        // An unknown table: its section.
        _ => Some(line_removal(doc, node.trimmed_range())),
    }
}

/// `range` with its whole lines, when nothing else is on them.
fn line_removal(doc: &Document, range: TextRange) -> TextRange {
    let text = &*doc.text;
    let (start, end) = (usize::from(range.start()), usize::from(range.end()));
    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    if !text[line_start..start].trim().is_empty() {
        return range;
    }
    // A trailing comment goes too.
    let rest = &text[end..];
    let line_len = rest.find('\n').map_or(rest.len(), |i| i + 1);
    let tail = rest[..line_len].trim();
    if !tail.is_empty() && !tail.starts_with('#') {
        return range;
    }
    TextRange::new(TextSize::new(line_start as u32), TextSize::new((end + line_len) as u32))
}

/// An entry of an inline table and one of its commas.
fn inline_removal(entry: SyntaxNode<'_>) -> TextRange {
    let range = entry.text_range();
    let mut next = entry.next_sibling_or_token();
    while let Some(el) = next {
        match el.kind() {
            SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE | SyntaxKind::COMMENT => {
                next = el_next(el)
            }
            SyntaxKind::COMMA => {
                // Up to the next entry.
                let mut end = el.text_range().end();
                let mut after = el_next(el);
                while let Some(a) = after {
                    if a.kind() != SyntaxKind::WHITESPACE {
                        break;
                    }
                    end = a.text_range().end();
                    after = el_next(a);
                }
                return TextRange::new(range.start(), end);
            }
            _ => break,
        }
    }
    // The last entry: the comma before it.
    let mut prev = entry.prev_sibling_or_token();
    while let Some(el) = prev {
        match el.kind() {
            SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => prev = el_prev(el),
            SyntaxKind::COMMA => return TextRange::new(el.text_range().start(), range.end()),
            _ => break,
        }
    }
    range
}

fn el_next(el: toml_parser::SyntaxElement<'_>) -> Option<toml_parser::SyntaxElement<'_>> {
    match el {
        toml_parser::SyntaxElement::Node(n) => n.next_sibling_or_token(),
        toml_parser::SyntaxElement::Token(t) => t.next_sibling_or_token(),
    }
}

fn el_prev(el: toml_parser::SyntaxElement<'_>) -> Option<toml_parser::SyntaxElement<'_>> {
    match el {
        toml_parser::SyntaxElement::Node(n) => n.prev_sibling_or_token(),
        toml_parser::SyntaxElement::Token(t) => t.prev_sibling_or_token(),
    }
}

/// A value for a new key: its default, or an empty value of its type.
fn placeholder(default: &Option<Value>, ty: &Option<String>) -> String {
    if let Some(d) = default {
        return json_to_toml(d);
    }
    match ty.as_deref() {
        Some("integer") => "0",
        Some("number") => "0.0",
        Some("boolean") => "false",
        Some("array") => "[]",
        Some("object") => "{}",
        _ => "\"\"",
    }
    .to_owned()
}

/// Adds `keys` to the table at `path`.
fn add_keys(
    doc: &Document,
    path: &Path,
    keys: &[(String, Option<Value>, Option<String>)],
) -> Option<Fix> {
    if keys.is_empty() {
        return None;
    }
    let entries: Vec<String> =
        keys.iter().map(|(k, d, t)| format!("{} = {}", key(k), placeholder(d, t))).collect();
    let names: Vec<String> = keys.iter().map(|(k, ..)| format!("`{k}`")).collect();
    let title = format!("Add {}", names.join(", "));
    let root = doc.root();
    let text = &*doc.text;

    // An inline table.
    if let Some(Item::Table(t)) = doc.dom.get(path)
        && t.kind == TableKind::Inline
    {
        let node = root.covering_node(t.range);
        if node.kind() != SyntaxKind::INLINE_TABLE {
            return None;
        }
        let last = node.children().filter(|n| n.kind() == SyntaxKind::ENTRY).last();
        return Some(match last {
            Some(last) => {
                let at = TextRange::empty(last.text_range().end());
                Fix::new(title, at, format!(", {}", entries.join(", ")))
            }
            None => Fix::new(title, t.range, format!("{{ {} }}", entries.join(", "))),
        });
    }

    // The root or a `[table]`: after its last entry, or its header.
    let container = if path.is_empty() {
        Some(root)
    } else {
        let range = doc.dom.table_paths.iter().find(|(_, p)| p == path).map(|(r, _)| *r)?;
        root.descendants().find(|n| {
            n.text_range() == range
                && matches!(n.kind(), SyntaxKind::TABLE | SyntaxKind::ARRAY_TABLE)
        })
    }?;
    let after = container
        .children()
        .filter(|n| {
            matches!(
                n.kind(),
                SyntaxKind::ENTRY | SyntaxKind::TABLE_HEADER | SyntaxKind::ARRAY_TABLE_HEADER
            )
        })
        .last()
        .map(|n| n.text_range().end());
    let block: String = entries.iter().map(|e| format!("{e}\n")).collect();
    let fix = match after {
        Some(end) => {
            let rest = &text[usize::from(end)..];
            match rest.find('\n') {
                Some(i) => {
                    Fix::new(title, TextRange::empty(end + TextSize::new(i as u32 + 1)), block)
                }
                None => Fix::new(
                    title,
                    TextRange::empty(TextSize::of(text)),
                    format!("\n{}", block.trim_end()),
                ),
            }
        }
        // An empty root: before the first table.
        None => {
            let at = root
                .children()
                .find(|n| matches!(n.kind(), SyntaxKind::TABLE | SyntaxKind::ARRAY_TABLE))
                .map_or(TextSize::of(text), |n| n.text_range().start());
            let sep = if usize::from(at) > 0 && !text[..usize::from(at)].ends_with('\n') {
                "\n"
            } else {
                ""
            };
            Fix::new(title, TextRange::empty(at), format!("{sep}{block}"))
        }
    };
    Some(fix.preferred())
}

/// Sorts the keys of the table at `offset` in the order of its schema:
/// a fix of no diagnostic, offered last. Comment lines above an entry move
/// with it; blank lines separate groups that are sorted each on its own.
pub fn sort_keys_fix(doc: &Document, offset: TextSize, schema: crate::Schema<'_>) -> Option<Fix> {
    let root = doc.root();
    let token = root.token_at_offset(offset).left_biased()?;
    let container = token.ancestors().find(|n| {
        matches!(n.kind(), SyntaxKind::TABLE | SyntaxKind::ARRAY_TABLE | SyntaxKind::ROOT)
    })?;
    let path = match container.kind() {
        SyntaxKind::ROOT => Path::new(),
        _ => doc.dom.table_path(container.text_range().start())?.clone(),
    };
    let schema_root = schema.root()?;
    let order: Vec<String> = toml_schema::key_completions(&toml_schema::schemas_at(
        schema.registry,
        &schema_root,
        &path,
    ))
    .into_iter()
    .map(|k| k.key)
    .collect();
    if order.is_empty() {
        return None;
    }
    let text = &*doc.text;
    // (start, end, rank) of each entry with its comment lines.
    let mut chunks: Vec<(usize, usize, usize)> = Vec::new();
    for entry in container.children().filter(|n| n.kind() == SyntaxKind::ENTRY) {
        let range = entry.text_range();
        let mut start = text[..usize::from(range.start())].rfind('\n').map_or(0, |i| i + 1);
        while start > 0 {
            let prev = text[..start - 1].rfind('\n').map_or(0, |i| i + 1);
            if !text[prev..start].trim_start().starts_with('#') {
                break;
            }
            start = prev;
        }
        let rest = &text[usize::from(range.end())..];
        let end = usize::from(range.end()) + rest.find('\n').map_or(rest.len(), |i| i + 1);
        let first = entry
            .child_node(SyntaxKind::KEY)
            .and_then(<toml_parser::ast::Key<'_> as toml_parser::ast::AstNode<'_>>::cast)
            .and_then(|k| k.path().into_iter().next())
            .unwrap_or_default();
        let rank = order.iter().position(|k| *k == first).unwrap_or(usize::MAX);
        chunks.push((start, end, rank));
    }
    let mut edits = Vec::new();
    let mut i = 0;
    while i < chunks.len() {
        let mut j = i + 1;
        while j < chunks.len() && chunks[j].0 == chunks[j - 1].1 {
            j += 1;
        }
        let group = &chunks[i..j];
        let mut sorted = group.to_vec();
        // Stable: unknown keys keep their order, after the known ones.
        sorted.sort_by_key(|c| c.2);
        if sorted != group {
            let (start, end) = (group[0].0, group[group.len() - 1].1);
            let mut new = String::new();
            for &(s, e, _) in &sorted {
                new.push_str(&text[s..e]);
                if !new.ends_with('\n') {
                    new.push('\n');
                }
            }
            if !text[..end].ends_with('\n') {
                new.pop();
            }
            edits.push((
                TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32)),
                new,
            ));
        }
        i = j;
    }
    (!edits.is_empty()).then(|| Fix {
        title: "Sort keys in schema order".to_owned(),
        edits,
        preferred: false,
    })
}
