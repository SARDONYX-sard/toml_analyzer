//! The `[features]` of a `Cargo.toml`: checks of the values and their
//! completion.
//!
//! A value is one of
//! - `"std"`: another feature, or the implicit feature of an optional
//!   dependency;
//! - `"dep:tracing"`: an optional dependency;
//! - `"serde/derive"`: a feature of a dependency;
//! - `"serde?/derive"`: a feature of a dependency, without enabling it.
//!
//! The features of a dependency come from the registry: see
//! [`crate::cargo_context`].

use toml_dom::{Item, PathSegment, TableKind, ValueKind};
use toml_parser::{TextRange, TextSize};

use crate::{
    CompletionItem, CompletionKind, Diagnostic, Document, Fix, PositionKind, Severity, key,
    position_at, quote,
};

/// The tables whose dependencies features can name: not the
/// `dev-dependencies`, which cannot be optional.
const TABLES: &[&str] = &["dependencies", "build-dependencies"];

/// A dependency of the package.
pub(crate) struct Dependency {
    /// The key.
    pub(crate) name: String,
    /// Where it is defined.
    pub(crate) path: Vec<PathSegment>,
    /// `optional = true`.
    pub(crate) optional: bool,
}

/// The dependencies features can name, the first definition of each.
pub(crate) fn dependencies(doc: &Document) -> Vec<Dependency> {
    let k = |s: &str| PathSegment::Key(s.to_owned());
    let mut tables: Vec<Vec<PathSegment>> = TABLES.iter().map(|t| vec![k(t)]).collect();
    if let Some(Item::Table(targets)) = doc.dom.root.get("target") {
        for target in targets.entries() {
            tables.extend(TABLES.iter().map(|t| vec![k("target"), k(&target.key), k(t)]));
        }
    }
    let mut out: Vec<Dependency> = Vec::new();
    for table in tables {
        let Some(Item::Table(deps)) = doc.dom.get(&table) else { continue };
        for entry in deps.entries() {
            let mut path = table.clone();
            path.push(k(&entry.key));
            let mut optional_path = path.clone();
            optional_path.push(k("optional"));
            let optional = matches!(
                doc.dom.get(&optional_path),
                Some(Item::Value(v)) if matches!(v.kind, ValueKind::Bool(true))
            );
            match out.iter_mut().find(|d| d.name == entry.key) {
                // Optional in one table is enough.
                Some(d) => d.optional |= optional,
                None => out.push(Dependency { name: entry.key.clone(), path, optional }),
            }
        }
    }
    out
}

/// The strings of the `[features]` arrays: feature, value and its range.
fn values(doc: &Document) -> Vec<(&str, &str, TextRange)> {
    let Some(Item::Table(features)) = doc.dom.root.get("features") else { return Vec::new() };
    let mut out = Vec::new();
    for e in features.entries() {
        let Item::Array(a) = &e.item else { continue };
        for item in &a.items {
            if let Item::Value(v) = item
                && let ValueKind::String(s) = &v.kind
            {
                out.push((e.key.as_str(), s.as_str(), v.range));
            }
        }
    }
    out
}

/// The features defined in `[features]`.
fn defined(doc: &Document) -> Vec<&str> {
    match doc.dom.root.get("features") {
        Some(Item::Table(t)) => t.entries().iter().map(|e| e.key.as_str()).collect(),
        _ => Vec::new(),
    }
}

/// The optional dependencies that are features: those no `dep:` names.
fn implicit<'d>(doc: &Document, deps: &'d [Dependency]) -> Vec<&'d str> {
    let hidden: Vec<&str> =
        values(doc).into_iter().filter_map(|(_, v, _)| v.strip_prefix("dep:")).collect();
    deps.iter()
        .filter(|d| d.optional && !hidden.contains(&d.name.as_str()))
        .map(|d| d.name.as_str())
        .collect()
}

/// The problems of the values of `[features]`.
pub fn cargo_feature_diagnostics(doc: &Document) -> Vec<Diagnostic> {
    let deps = dependencies(doc);
    let dep_names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
    let mut features = defined(doc);
    features.extend(implicit(doc, &deps));
    let mut out = Vec::new();
    let mut push = |range, severity, message: String, fixes: Vec<Fix>| {
        out.push(Diagnostic {
            range,
            severity,
            code: "cargo",
            message,
            related: Vec::new(),
            fixes,
            deprecated: false,
        });
    };
    // An unknown name, `start..end` of `value`: replaced by the closest known
    // one, if any.
    let unknown = |value: &str, (start, end): (usize, usize), known: &[&str], range| {
        let written = &value[start..end];
        match toml_schema::closest(written, known) {
            Some(similar) => {
                let new = quote(&format!("{}{similar}{}", &value[..start], &value[end..]));
                let fix = Fix::new(format!("Replace with `{new}`"), range, new).preferred();
                (format!(", did you mean `{similar}`?"), vec![fix])
            }
            None => (String::new(), Vec::new()),
        }
    };
    for (_, value, range) in values(doc) {
        if let Some(name) = value.strip_prefix("dep:") {
            match deps.iter().find(|d| d.name == name) {
                None => {
                    let (hint, fixes) = unknown(value, (4, value.len()), &dep_names, range);
                    push(
                        range,
                        Severity::Error,
                        format!("`{name}` is not a dependency{hint}"),
                        fixes,
                    );
                }
                Some(d) if !d.optional => push(
                    range,
                    Severity::Error,
                    format!("`dep:{name}` needs `{name}` to be an optional dependency"),
                    make_optional(doc, d).into_iter().collect(),
                ),
                Some(_) => {}
            }
        } else if let Some((dep, _)) = value.split_once('/') {
            let (name, weak) = match dep.strip_suffix('?') {
                Some(name) => (name, true),
                None => (dep, false),
            };
            match deps.iter().find(|d| d.name == name) {
                None => {
                    let (hint, fixes) = unknown(value, (0, name.len()), &dep_names, range);
                    push(
                        range,
                        Severity::Error,
                        format!("`{name}` is not a dependency{hint}"),
                        fixes,
                    );
                }
                Some(d) if weak && !d.optional => push(
                    range,
                    Severity::Warning,
                    format!("`{name}` is not optional: `{name}?/` is the same as `{name}/`"),
                    make_optional(doc, d).into_iter().collect(),
                ),
                Some(_) => {}
            }
        } else if !features.contains(&value) {
            let (hint, fixes) = unknown(value, (0, value.len()), &features, range);
            push(
                range,
                Severity::Error,
                format!("`{value}` is neither a feature nor an optional dependency{hint}"),
                fixes,
            );
        }
    }
    out
}

/// The fix adding `optional = true` to the dependency `dep`.
fn make_optional(doc: &Document, dep: &Dependency) -> Option<Fix> {
    let title = format!("Make `{}` optional", dep.name);
    let item = doc.dom.get(&dep.path)?;
    let fix = |range, text: String| Some(Fix::new(title.clone(), range, text).preferred());
    match item {
        // `dep = "1.0"`
        Item::Value(v) => {
            let ValueKind::String(version) = &v.kind else { return None };
            fix(v.range, format!("{{ version = {}, optional = true }}", quote(version)))
        }
        Item::Table(t) => match (t.kind, t.entries().last()) {
            (TableKind::Inline, None) => fix(t.range, "{ optional = true }".to_owned()),
            (TableKind::Inline, Some(last)) => {
                fix(TextRange::empty(last.item.range().end()), ", optional = true".to_owned())
            }
            // `dep.workspace = true`
            (TableKind::Dotted, Some(last)) => fix(
                TextRange::empty(last.item.range().end()),
                format!("\n{}.optional = true", key(&dep.name)),
            ),
            // `[dependencies.dep]`: at the end of the section.
            (TableKind::Header, _) => {
                let (range, _) = doc.dom.table_paths.iter().find(|(_, p)| *p == dep.path)?;
                let section = doc.text[*range].trim_end();
                let end = range.start() + TextSize::of(section);
                fix(TextRange::empty(end), "\noptional = true".to_owned())
            }
            _ => None,
        },
        Item::Array(_) => None,
    }
}

/// The completions of a value of `[features]` at `offset` that need no
/// registry: the other features, `dep:` and the optional dependencies.
/// After `"dep/`, the features of the dependency are asked from the
/// registry instead, see [`crate::cargo_context`].
pub fn cargo_feature_completions(doc: &Document, offset: TextSize) -> Vec<CompletionItem> {
    let position = position_at(doc, offset);
    let PositionKind::Value { range } = position.kind else { return Vec::new() };
    let [PathSegment::Key(table), PathSegment::Key(current), PathSegment::Index(_)] =
        position.path.as_slice()
    else {
        return Vec::new();
    };
    if table != "features" || written_dependency(doc, range, offset).is_some() {
        return Vec::new();
    }
    let existing: Vec<&str> =
        values(doc).into_iter().filter(|(f, _, _)| f == current).map(|(_, v, _)| v).collect();
    let deps = dependencies(doc);
    let mut candidates: Vec<(String, &str)> = Vec::new();
    for f in defined(doc).into_iter().filter(|f| f != current) {
        candidates.push((f.to_owned(), "feature"));
    }
    for d in implicit(doc, &deps) {
        candidates.push((d.to_owned(), "optional dependency"));
    }
    // Every dependency: a quick fix makes a non-optional one optional.
    for d in &deps {
        let detail =
            if d.optional { "optional dependency" } else { "dependency, not optional yet" };
        candidates.push((format!("dep:{}", d.name), detail));
    }
    candidates
        .into_iter()
        .filter(|(v, _)| !existing.contains(&v.as_str()))
        .enumerate()
        .map(|(i, (value, detail))| CompletionItem {
            filter: Some(quote(&value)),
            insert: quote(&value),
            label: value,
            snippet: false,
            range,
            kind: CompletionKind::Value,
            detail: Some(detail.to_owned()),
            documentation: None,
            sort: format!("0{i:05}"),
            deprecated: false,
        })
        .collect()
}

/// The `serde/` or `serde?/` written before `offset` in the string at
/// `range`: the dependency and the prefix.
pub(crate) fn written_dependency(
    doc: &Document,
    range: TextRange,
    offset: TextSize,
) -> Option<(String, String)> {
    let before = doc.text.get(usize::from(range.start())..usize::from(offset))?;
    let content = before.strip_prefix(['"', '\''])?;
    let (dep, _) = content.split_once('/')?;
    let name = dep.strip_suffix('?').unwrap_or(dep);
    if name.is_empty() || name.starts_with("dep:") {
        return None;
    }
    Some((name.to_owned(), format!("{dep}/")))
}
