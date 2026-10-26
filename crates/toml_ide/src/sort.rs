//! Sorting the entries of a table, as quick fixes.
//!
//! The entries of a table section are split into groups at blank lines.
//! Comment lines at the top of a group head it and stay there; comment
//! lines above any other entry move with it. Sorting keeps the groups and
//! their order, and sorts the entries of each.

use toml_dom::Path;
use toml_parser::{
    SyntaxKind, TextRange, TextSize,
    ast::{self, AstNode},
};

use crate::{Document, Fix};

/// An entry with the comment lines right above it.
pub(crate) struct Chunk {
    /// The first segment of the key, decoded.
    pub(crate) name: String,
    /// The comment lines above the entry.
    pub(crate) comments: String,
    /// The lines of the entry, line break included.
    pub(crate) lines: String,
}

/// Entries between blank lines, with the comment lines that head them.
pub(crate) struct Group {
    pub(crate) heading: String,
    pub(crate) chunks: Vec<Chunk>,
}

/// The entries of the table at the cursor.
pub(crate) struct Section {
    /// The path of the table.
    pub(crate) path: Path,
    /// From the end of the header line to the end of the last entry line.
    pub(crate) range: TextRange,
    pub(crate) groups: Vec<Group>,
}

/// The table section holding `offset`: a `[table]`, an `[[array]]`
/// element, or the entries before the first header.
pub(crate) fn section(doc: &Document, offset: TextSize) -> Option<Section> {
    let root = doc.root();
    let token = root.token_at_offset(offset).left_biased()?;
    let table = token.ancestors().find(|n| {
        matches!(n.kind(), SyntaxKind::TABLE | SyntaxKind::ARRAY_TABLE | SyntaxKind::ROOT)
    })?;
    let text = &*doc.text;
    let (path, start) = if table.kind() == SyntaxKind::ROOT {
        (Path::new(), 0)
    } else {
        let path = doc.dom.table_path(table.text_range().start())?.clone();
        let header = table.children().find(|n| {
            matches!(n.kind(), SyntaxKind::TABLE_HEADER | SyntaxKind::ARRAY_TABLE_HEADER)
        })?;
        let end = usize::from(header.text_range().end());
        (path, end + text[end..].find('\n').map_or(text.len() - end, |i| i + 1))
    };

    let mut groups: Vec<Group> = Vec::new();
    let mut prev_end = start;
    let mut region_end = None;
    for entry in table.children().filter(|n| n.kind() == SyntaxKind::ENTRY) {
        let range = entry.text_range();
        let line_start = text[..usize::from(range.start())].rfind('\n').map_or(0, |i| i + 1);
        let rest = &text[usize::from(range.end())..];
        let end = usize::from(range.end()) + rest.find('\n').map_or(rest.len(), |i| i + 1);
        let above = &text[prev_end.min(line_start)..line_start];
        let comments: String =
            above.split_inclusive('\n').filter(|l| !l.trim().is_empty()).collect();
        let new_group =
            groups.is_empty() || above.split_inclusive('\n').any(|l| l.trim().is_empty());
        let name = entry
            .child_node(SyntaxKind::KEY)
            .and_then(ast::Key::cast)
            .and_then(|k| k.path().into_iter().next())
            .unwrap_or_default();
        let mut lines = text[line_start..end].to_owned();
        if !lines.ends_with('\n') {
            lines.push('\n');
        }
        let comments = if new_group {
            groups.push(Group { heading: comments, chunks: Vec::new() });
            String::new()
        } else {
            comments
        };
        if let Some(g) = groups.last_mut() {
            g.chunks.push(Chunk { name, comments, lines });
        }
        prev_end = end;
        region_end = Some(end);
    }
    let end = region_end?;
    Some(Section {
        path,
        range: TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32)),
        groups,
    })
}

/// The order of names: case and `_`/`-` do not matter.
pub(crate) fn name_key(name: &str) -> String {
    name.to_ascii_lowercase().replace('_', "-")
}

/// The fixes replacing the section by each of `options` (title and text),
/// those changing something.
pub(crate) fn fixes(doc: &Document, section: &Section, options: Vec<(&str, String)>) -> Vec<Fix> {
    let text = &*doc.text;
    let end = usize::from(section.range.end());
    let mut current = text[section.range].to_owned();
    if !current.ends_with('\n') {
        current.push('\n');
    }
    options
        .into_iter()
        .filter(|(_, new)| *new != current)
        .map(|(title, mut new)| {
            if !text[..end].ends_with('\n') {
                new.pop();
            }
            Fix { title: title.to_owned(), edits: vec![(section.range, new)], preferred: false }
        })
        .collect()
}

/// Sorts the entries of each group of the table at `offset` by name.
pub fn sort_alphabetically_fix(doc: &Document, offset: TextSize) -> Option<Fix> {
    let section = section(doc, offset)?;
    let sorted = section
        .groups
        .iter()
        .map(|g| {
            let mut chunks: Vec<&Chunk> = g.chunks.iter().collect();
            chunks.sort_by_key(|c| name_key(&c.name));
            let body: String =
                chunks.iter().map(|c| format!("{}{}", c.comments, c.lines)).collect();
            format!("{}{body}", g.heading)
        })
        .collect::<Vec<_>>()
        .join("\n");
    fixes(doc, &section, vec![("Sort keys alphabetically", sorted)]).pop()
}
