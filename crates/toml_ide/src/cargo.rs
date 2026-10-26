//! Completion of the dependencies of `Cargo.toml` from a crate registry:
//! crate names, versions and features.
//!
//! The registry is queried by the caller: [`cargo_context`] says what is
//! needed at the cursor, and [`cargo_completions`] turns the answer into
//! completion items.

use toml_dom::{Item, PathSegment, ValueKind};
use toml_parser::{TextRange, TextSize};

use crate::{CompletionItem, CompletionKind, Document, PositionKind, position_at, quote};

/// A crate found by a search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrateSummary {
    /// The name.
    pub name: String,
    /// The newest stable version (or the newest one).
    pub version: String,
    /// The description.
    pub description: Option<String>,
}

/// A published version of a crate, from the registry index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrateVersion {
    /// The version.
    pub version: String,
    /// Withdrawn by the authors.
    pub yanked: bool,
    /// The features, optional dependencies included.
    pub features: Vec<String>,
}

/// What the registry must answer.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CargoQuery {
    /// The crates matching a name.
    Search(String),
    /// The versions of a crate.
    Versions(String),
}

/// What is written at the cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CargoTarget {
    /// The name of a dependency: a key of a dependency table, or a table
    /// name in a header.
    Name {
        /// In a `[dependencies.name]` header: only the name is inserted.
        header: bool,
        /// The key is followed by `=` or `.`: only the name is inserted.
        has_value: bool,
    },
    /// The version requirement of a dependency.
    Version {
        /// The crate.
        name: String,
    },
    /// An item of the `features` of a dependency.
    Feature {
        /// The crate.
        name: String,
        /// The version requirement, if any.
        requirement: Option<String>,
        /// The features already listed.
        existing: Vec<String>,
    },
}

/// A completion of a dependency, see [`cargo_context`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CargoContext {
    /// What is written.
    pub target: CargoTarget,
    /// The text replaced: the key segment or the string under the cursor.
    pub range: TextRange,
    /// What the registry must answer.
    pub query: CargoQuery,
}

const DEPENDENCY_TABLES: &[&str] = &["dependencies", "dev-dependencies", "build-dependencies"];

/// The length of the path of a dependency table: `[dependencies]`,
/// `[workspace.dependencies]`, `[target.'cfg(..)'.dependencies]`, ...
fn dependency_table(path: &[PathSegment]) -> Option<usize> {
    let key = |i: usize| match path.get(i) {
        Some(PathSegment::Key(k)) => Some(k.as_str()),
        _ => None,
    };
    let is_deps = |i: usize| key(i).is_some_and(|k| DEPENDENCY_TABLES.contains(&k));
    if is_deps(0) {
        Some(1)
    } else if key(0) == Some("workspace") && key(1) == Some("dependencies") {
        Some(2)
    } else if key(0) == Some("target") && key(1).is_some() && is_deps(2) {
        Some(3)
    } else {
        None
    }
}

/// What the dependency completion at `offset` needs, if the cursor is on
/// one. `doc` must be a `Cargo.toml`.
pub fn cargo_context(doc: &Document, offset: TextSize) -> Option<CargoContext> {
    let position = position_at(doc, offset);
    let path = &position.path;
    let table = dependency_table(path)?;
    match &position.kind {
        PositionKind::Key { range, header, .. } if path.len() == table => {
            let prefix = doc.text.get(usize::from(range.start())..usize::from(offset))?;
            if prefix.is_empty()
                || !prefix.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                return None;
            }
            let after = doc.text[usize::from(range.end())..].trim_start_matches([' ', '\t']);
            let has_value = after.starts_with(['=', '.']);
            Some(CargoContext {
                target: CargoTarget::Name { header: *header, has_value },
                range: *range,
                query: CargoQuery::Search(prefix.to_owned()),
            })
        }
        PositionKind::Value { range } => {
            let Some(PathSegment::Key(name)) = path.get(table) else { return None };
            let rest = &path[table + 1..];
            let target = match rest {
                [] => CargoTarget::Version { name: name.clone() },
                [PathSegment::Key(k)] if k == "version" => {
                    CargoTarget::Version { name: name.clone() }
                }
                [PathSegment::Key(k), PathSegment::Index(_)] if k == "features" => {
                    let dep = &path[..=table];
                    CargoTarget::Feature {
                        name: name.clone(),
                        requirement: requirement(doc, dep),
                        existing: features(doc, dep),
                    }
                }
                _ => return None,
            };
            // A `package = "real-name"` rename queries the real crate.
            let real = package_name(doc, &path[..=table]).unwrap_or_else(|| name.clone());
            let target = match target {
                CargoTarget::Version { .. } => CargoTarget::Version { name: real.clone() },
                CargoTarget::Feature { requirement, existing, .. } => {
                    CargoTarget::Feature { name: real.clone(), requirement, existing }
                }
                other => other,
            };
            Some(CargoContext { target, range: *range, query: CargoQuery::Versions(real) })
        }
        _ => None,
    }
}

fn string_at(doc: &Document, path: &[PathSegment]) -> Option<String> {
    match doc.dom.get(path)? {
        Item::Value(v) => match &v.kind {
            ValueKind::String(s) => Some(s.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn with(path: &[PathSegment], key: &str) -> Vec<PathSegment> {
    let mut p = path.to_vec();
    p.push(PathSegment::Key(key.to_owned()));
    p
}

/// `dep = "1.0"` or `dep = { version = "1.0" }`.
fn requirement(doc: &Document, dep: &[PathSegment]) -> Option<String> {
    string_at(doc, dep).or_else(|| string_at(doc, &with(dep, "version")))
}

fn package_name(doc: &Document, dep: &[PathSegment]) -> Option<String> {
    string_at(doc, &with(dep, "package"))
}

fn features(doc: &Document, dep: &[PathSegment]) -> Vec<String> {
    match doc.dom.get(&with(dep, "features")) {
        Some(Item::Array(a)) => a
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Value(v) => match &v.kind {
                    ValueKind::String(s) => Some(s.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The completions of `context`, with the answer of the registry.
pub fn cargo_completions(
    context: &CargoContext,
    found: Option<&[CrateSummary]>,
    versions: Option<&[CrateVersion]>,
) -> Vec<CompletionItem> {
    let item =
        |label: String, insert: String, kind, detail, documentation, i: usize| CompletionItem {
            filter: Some(insert.clone()),
            label,
            insert,
            snippet: false,
            range: context.range,
            kind,
            detail,
            documentation,
            // Before the items of the schema.
            sort: format!("0{i:05}"),
            deprecated: false,
        };
    match &context.target {
        CargoTarget::Name { header, has_value } => found
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let insert = if *header || *has_value {
                    c.name.clone()
                } else {
                    format!("{} = {}", c.name, quote(&c.version))
                };
                let mut it = item(
                    c.name.clone(),
                    insert,
                    CompletionKind::Key,
                    Some(c.version.clone()),
                    c.description.clone(),
                    i,
                );
                it.filter = Some(c.name.clone());
                it
            })
            .collect(),
        CargoTarget::Version { name } => {
            let mut list: Vec<&CrateVersion> =
                versions.unwrap_or_default().iter().filter(|v| !v.yanked).collect();
            list.sort_by(|a, b| compare_versions(&b.version, &a.version));
            list.iter()
                .enumerate()
                .map(|(i, v)| {
                    item(
                        v.version.clone(),
                        quote(&v.version),
                        CompletionKind::Value,
                        Some(name.clone()),
                        None,
                        i,
                    )
                })
                .collect()
        }
        CargoTarget::Feature { name, requirement, existing } => {
            let Some(version) =
                select_version(versions.unwrap_or_default(), requirement.as_deref())
            else {
                return Vec::new();
            };
            version
                .features
                .iter()
                .filter(|f| !existing.contains(f))
                .enumerate()
                .map(|(i, f)| {
                    let detail = format!("{name} {}", version.version);
                    item(f.clone(), quote(f), CompletionKind::Value, Some(detail), None, i)
                })
                .collect()
        }
    }
}

/// `major.minor.patch` and whether it is a pre-release.
fn parse_version(v: &str) -> (u64, u64, u64, bool) {
    let (core, pre) = match v.split_once(['-', '+']) {
        Some((core, _)) => (core, v.contains('-')),
        None => (v, false),
    };
    let mut parts = core.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0), pre)
}

fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let (a, b) = (parse_version(a), parse_version(b));
    // A pre-release comes before its release.
    (a.0, a.1, a.2, !a.3).cmp(&(b.0, b.1, b.2, !b.3))
}

/// The newest version matching `requirement` (a simplified Cargo version
/// requirement: its first comparator), else the newest stable one.
fn select_version<'v>(
    versions: &'v [CrateVersion],
    requirement: Option<&str>,
) -> Option<&'v CrateVersion> {
    let usable: Vec<&CrateVersion> = versions.iter().filter(|v| !v.yanked).collect();
    let newest = |pred: &dyn Fn(&CrateVersion) -> bool| {
        usable
            .iter()
            .copied()
            .filter(|v| pred(v))
            .max_by(|a, b| compare_versions(&a.version, &b.version))
    };
    if let Some(req) = requirement
        && let Some(found) = newest(&|v| matches_requirement(req, &v.version))
    {
        return Some(found);
    }
    newest(&|v| !parse_version(&v.version).3).or_else(|| newest(&|_| true))
}

/// Whether `version` matches the first comparator of `req`.
pub(crate) fn matches_requirement(req: &str, version: &str) -> bool {
    let first = req.split(',').next().unwrap_or("").trim();
    if first == "*" || first.is_empty() {
        return true;
    }
    let op_len = first.find(|c: char| c.is_ascii_digit()).unwrap_or(first.len());
    let (op, rest) = first.split_at(op_len);
    let op = op.trim();
    let wanted: Vec<u64> = rest
        .split(['-', '+'])
        .next()
        .unwrap_or("")
        .split('.')
        .filter_map(|p| p.parse().ok())
        .collect();
    let Some(&major) = wanted.first() else { return false };
    let minor = wanted.get(1).copied();
    let (vmaj, vmin, vpatch, _) = parse_version(version);
    let req_tuple = (major, minor.unwrap_or(0), wanted.get(2).copied().unwrap_or(0));
    let at_least = (vmaj, vmin, vpatch) >= req_tuple;
    match op {
        "=" => {
            vmaj == major
                && minor.is_none_or(|m| vmin == m)
                && wanted.get(2).is_none_or(|&p| vpatch == p)
        }
        "~" => vmaj == major && minor.is_none_or(|m| vmin == m) && at_least,
        ">=" => at_least,
        ">" => (vmaj, vmin, vpatch) > req_tuple,
        "<" => (vmaj, vmin, vpatch) < req_tuple,
        "<=" => (vmaj, vmin, vpatch) <= req_tuple,
        // Caret, the default.
        _ => {
            if vmaj != major {
                false
            } else if major == 0 && minor.is_some_and(|m| m > 0 || wanted.len() > 2) {
                Some(vmin) == minor && at_least
            } else {
                at_least
            }
        }
    }
}
