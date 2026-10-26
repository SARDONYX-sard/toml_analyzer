//! Completion of the dependencies of `Cargo.toml` from a crate registry:
//! crate names, versions and features.
//!
//! The registry is queried by the caller: [`cargo_context`] says what is
//! needed at the cursor, and [`cargo_completions`] turns the answer into
//! completion items.

use toml_dom::{Item, Path, PathSegment, ValueKind};
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
    /// The `path` of a local dependency: its `Cargo.toml` answers instead
    /// of the registry, see [`cargo_local_crate`].
    pub local_path: Option<String>,
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
                local_path: None,
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
            let local_path = string_at(doc, &with(&path[..=table], "path"));
            Some(CargoContext {
                target,
                range: *range,
                query: CargoQuery::Versions(real),
                local_path,
            })
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

/// How a completed dependency is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DependencyStyle {
    /// `serde = { version = "1.0" }`, ready for `features`.
    #[default]
    Table,
    /// `serde = "1.0"`
    String,
}

/// The completions of `context`, with the answer of the registry.
/// `snippets`: whether the editor takes snippets, to leave the cursor
/// inside `{ version = "..." }`.
pub fn cargo_completions(
    context: &CargoContext,
    found: Option<&[CrateSummary]>,
    versions: Option<&[CrateVersion]>,
    style: DependencyStyle,
    snippets: bool,
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
                let version = quote(&c.version);
                let (insert, snippet) = match style {
                    _ if *header || *has_value => (c.name.clone(), false),
                    DependencyStyle::String => (format!("{} = {version}", c.name), false),
                    // The cursor after the version: `, features = [...]` next.
                    DependencyStyle::Table if snippets => (
                        format!("{} = {{ version = {}$0 }}", c.name, version.replace('$', "\\$")),
                        true,
                    ),
                    DependencyStyle::Table => {
                        (format!("{} = {{ version = {version} }}", c.name), false)
                    }
                };
                let mut it = item(
                    c.name.clone(),
                    insert,
                    CompletionKind::Key,
                    Some(c.version.clone()),
                    c.description.clone(),
                    i,
                );
                it.snippet = snippet;
                it.filter = Some(c.name.clone());
                it
            })
            .collect(),
        CargoTarget::Version { name } => {
            let mut list: Vec<&CrateVersion> = versions
                .unwrap_or_default()
                .iter()
                .filter(|v| !v.yanked && !v.version.is_empty())
                .collect();
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

/// What a hover on the key of a dependency shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CargoHover {
    /// The hovered key.
    pub range: TextRange,
    /// The crate.
    pub name: String,
    /// The version requirement, if any.
    pub requirement: Option<String>,
}

/// The dependency whose key is hovered at `offset` in a `Cargo.toml`,
/// unless it comes from a path or git.
pub fn cargo_hover_context(doc: &Document, offset: TextSize) -> Option<CargoHover> {
    let position = position_at(doc, offset);
    let PositionKind::Key { range, segment: Some(_), .. } = &position.kind else { return None };
    let path = position.key_path()?;
    let table = dependency_table(&path)?;
    if path.len() <= table {
        return None;
    }
    let dep = &path[..=table];
    if string_at(doc, &with(dep, "path")).is_some() || string_at(doc, &with(dep, "git")).is_some() {
        return None;
    }
    let PathSegment::Key(key) = dep.last()? else { return None };
    let name = package_name(doc, dep).unwrap_or_else(|| key.clone());
    Some(CargoHover { range: *range, name, requirement: requirement(doc, dep) })
}

/// The docs.rs URL of a crate, for the version a requirement selects.
fn docs_url(name: &str, requirement: Option<&str>) -> String {
    match requirement.map(str::trim).filter(|r| !r.is_empty() && *r != "*") {
        Some(req) => format!("https://docs.rs/{name}/{}", encode(req)),
        None => format!("https://docs.rs/{name}"),
    }
}

/// Links from the keys of registry dependencies to their documentation.
/// Dependencies from a path or git have none.
pub fn cargo_docs_links(doc: &Document) -> Vec<(TextRange, String)> {
    let root = &doc.dom.root;
    let mut tables: Vec<Path> = Vec::new();
    let key = |k: &str| PathSegment::Key(k.to_owned());
    for t in DEPENDENCY_TABLES {
        tables.push(vec![key(t)]);
    }
    tables.push(vec![key("workspace"), key("dependencies")]);
    if let Some(Item::Table(targets)) = root.get("target") {
        for target in targets.entries() {
            for t in DEPENDENCY_TABLES {
                tables.push(vec![key("target"), key(&target.key), key(t)]);
            }
        }
    }
    let mut out = Vec::new();
    for table in tables {
        let Some(Item::Table(deps)) = doc.dom.get(&table) else { continue };
        for entry in deps.entries() {
            let dep = with(&table, &entry.key);
            if string_at(doc, &with(&dep, "path")).is_some()
                || string_at(doc, &with(&dep, "git")).is_some()
            {
                continue;
            }
            // `serde.workspace = true` takes the version of the workspace.
            if matches!(doc.dom.get(&with(&dep, "workspace")), Some(Item::Value(_))) {
                continue;
            }
            let name = package_name(doc, &dep).unwrap_or_else(|| entry.key.clone());
            out.push((entry.key_range, docs_url(&name, requirement(doc, &dep).as_deref())));
        }
    }
    out
}

/// The Markdown of a dependency hover: links to the documentation of the
/// version the requirement selects (docs.rs resolves requirements).
pub fn cargo_hover_markdown(hover: &CargoHover) -> String {
    let name = &hover.name;
    let version =
        hover.requirement.as_deref().map(str::trim).filter(|r| !r.is_empty() && *r != "*");
    let docs = docs_url(name, version);
    let shown = version.map_or_else(String::new, |v| format!(" `{v}`"));
    format!("**{name}**{shown}\n\n[docs.rs]({docs}) · [crates.io](https://crates.io/crates/{name})")
}

/// Percent-encodes the characters of a requirement that URLs do not take.
fn encode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '-' | '_' | '~' | '^' | '*' => c.to_string(),
            c => c.to_string().bytes().map(|b| format!("%{b:02X}")).collect(),
        })
        .collect()
}
/// The `path` of the dependency whose key is at `offset`: go to definition
/// opens its `Cargo.toml`.
pub fn cargo_path_dependency(doc: &Document, offset: TextSize) -> Option<String> {
    let position = position_at(doc, offset);
    let path = position.key_path()?;
    let table = dependency_table(&path)?;
    if path.len() <= table {
        return None;
    }
    string_at(doc, &with(&path[..=table], "path"))
}

/// The names of the `[workspace.dependencies]` that come from a path, when
/// `doc` is the root of a workspace: their `dep.workspace = true` users are
/// local too.
pub fn cargo_workspace_locals(doc: &Document) -> Option<Vec<String>> {
    let Some(Item::Table(workspace)) = doc.dom.root.get("workspace") else { return None };
    let deps = match workspace.get("dependencies") {
        Some(Item::Table(t)) => t,
        _ => return Some(Vec::new()),
    };
    let base =
        [PathSegment::Key("workspace".to_owned()), PathSegment::Key("dependencies".to_owned())];
    Some(
        deps.entries()
            .iter()
            .filter(|e| string_at(doc, &with(&with(&base, &e.key), "path")).is_some())
            .map(|e| e.key.clone())
            .collect(),
    )
}

/// Ways to sort the dependency table at `offset`, with the local
/// dependencies (`path`, or `workspace = true` naming one of
/// `workspace_locals`) first or last, each kind in one group. Alphabetical
/// order is [`crate::sort_alphabetically_fix`].
///
/// No heading is added: a group heading goes with the kind of its entries
/// (one starting with `# local` with the local ones), and a `# local`
/// comment above an entry heads the local dependencies.
pub fn cargo_sort_fixes(
    doc: &Document,
    offset: TextSize,
    workspace_locals: &[String],
) -> Vec<crate::Fix> {
    use crate::sort::{Chunk, name_key};

    let Some(section) = crate::sort::section(doc, offset) else { return Vec::new() };
    let path = &section.path;
    if path.is_empty() || dependency_table(path) != Some(path.len()) {
        return Vec::new();
    }
    let is_local = |c: &Chunk| {
        let dep = with(path, &c.name);
        let from_workspace = matches!(
            doc.dom.get(&with(&dep, "workspace")),
            Some(Item::Value(v)) if matches!(v.kind, ValueKind::Bool(true))
        );
        string_at(doc, &with(&dep, "path")).is_some()
            || (from_workspace && workspace_locals.contains(&c.name))
    };
    let is_local_heading = |h: &str| h.trim_start().to_ascii_lowercase().starts_with("# local");
    // `# local` comment lines above an entry become the heading.
    let sorted = |chunks: Vec<&Chunk>| {
        let mut chunks = chunks;
        chunks.sort_by_key(|c| name_key(&c.name));
        chunks
            .iter()
            .map(|c| {
                let comments = if is_local_heading(&c.comments) { "" } else { c.comments.as_str() };
                format!("{comments}{}", c.lines)
            })
            .collect::<String>()
    };

    let groups = &section.groups;
    let all: Vec<&Chunk> = groups.iter().flat_map(|g| &g.chunks).collect();
    let (local, other): (Vec<&Chunk>, Vec<&Chunk>) = all.iter().partition(|c| is_local(c));
    if local.is_empty() || other.is_empty() {
        return Vec::new();
    }
    let mut local_heading = String::new();
    let mut other_heading = String::new();
    for g in groups.iter().filter(|g| !g.heading.is_empty()) {
        let local_group = is_local_heading(&g.heading)
            || (!g.chunks.is_empty() && g.chunks.iter().all(&is_local));
        let target = if local_group { &mut local_heading } else { &mut other_heading };
        target.push_str(&g.heading);
    }
    if local_heading.is_empty()
        && let Some(c) = all.iter().find(|c| is_local_heading(&c.comments))
    {
        local_heading = c.comments.clone();
    }
    let local_block = format!("{local_heading}{}", sorted(local));
    let other_block = format!("{other_heading}{}", sorted(other));
    let options = vec![
        ("Sort dependencies, local first", format!("{local_block}\n{other_block}")),
        ("Sort dependencies, local last", format!("{other_block}\n{local_block}")),
    ];
    crate::sort::fixes(doc, &section, options)
}

/// The version and the features of a local crate, from its `Cargo.toml`
/// (`child`), and the root of its workspace for `version.workspace = true`.
pub fn cargo_local_crate(child: &Document, workspace: Option<&Document>) -> CrateVersion {
    let key = |k: &str| PathSegment::Key(k.to_owned());
    let version = string_at(child, &[key("package"), key("version")])
        .or_else(|| {
            workspace
                .and_then(|w| string_at(w, &[key("workspace"), key("package"), key("version")]))
        })
        .unwrap_or_default();
    let mut features: Vec<String> = match child.dom.root.get("features") {
        Some(Item::Table(t)) => t.entries().iter().map(|e| e.key.clone()).collect(),
        _ => Vec::new(),
    };
    // Optional dependencies are features too, unless a `dep:` hides them.
    let hidden: Vec<String> = match child.dom.root.get("features") {
        Some(Item::Table(t)) => t
            .entries()
            .iter()
            .filter_map(|e| match &e.item {
                Item::Array(a) => Some(a.items.iter().filter_map(|i| match i {
                    Item::Value(v) => match &v.kind {
                        ValueKind::String(s) => s.strip_prefix("dep:").map(str::to_owned),
                        _ => None,
                    },
                    _ => None,
                })),
                _ => None,
            })
            .flatten()
            .collect(),
        _ => Vec::new(),
    };
    if let Some(Item::Table(deps)) = child.dom.root.get("dependencies") {
        for e in deps.entries() {
            let optional = matches!(
                child.dom.get(&[key("dependencies"), key(&e.key), key("optional")]),
                Some(Item::Value(v)) if matches!(v.kind, ValueKind::Bool(true))
            );
            if optional && !hidden.contains(&e.key) && !features.contains(&e.key) {
                features.push(e.key.clone());
            }
        }
    }
    features.sort_by(|a, b| (a != "default", a).cmp(&(b != "default", b)));
    CrateVersion { version, yanked: false, features }
}

/// The dependencies of every dependency table: their path and key range.
fn dependency_entries(doc: &Document) -> Vec<(Path, TextRange)> {
    let key = |k: &str| PathSegment::Key(k.to_owned());
    let mut tables: Vec<Path> = DEPENDENCY_TABLES.iter().map(|t| vec![key(t)]).collect();
    tables.push(vec![key("workspace"), key("dependencies")]);
    if let Some(Item::Table(targets)) = doc.dom.root.get("target") {
        for target in targets.entries() {
            for t in DEPENDENCY_TABLES {
                tables.push(vec![key("target"), key(&target.key), key(t)]);
            }
        }
    }
    let mut out = Vec::new();
    for table in tables {
        let Some(Item::Table(deps)) = doc.dom.get(&table) else { continue };
        for entry in deps.entries() {
            out.push((with(&table, &entry.key), entry.key_range));
        }
    }
    out
}

/// Rewrites dependencies between `dep = "1.0"` and `dep = { version = "1.0" }`
/// (when the table holds the version only): the one at the cursor for an
/// empty `range`, else every one the range touches.
pub fn cargo_dependency_fixes(doc: &Document, range: TextRange) -> Vec<crate::Fix> {
    let deps: Vec<Path> = if range.is_empty() {
        let position = position_at(doc, range.start());
        let path = match &position.kind {
            PositionKind::Key { segment: Some(_), .. } => position.key_path(),
            PositionKind::Value { .. } => Some(position.path.clone()),
            _ => None,
        };
        let Some(path) = path else { return Vec::new() };
        let Some(table) = dependency_table(&path) else { return Vec::new() };
        if path.len() <= table {
            return Vec::new();
        }
        vec![path[..=table].to_vec()]
    } else {
        dependency_entries(doc)
            .into_iter()
            .filter(|(dep, key_range)| {
                let item = doc.dom.get(dep).map_or(*key_range, |i| key_range.cover(i.range()));
                item.start() <= range.end() && range.start() <= item.end()
            })
            .map(|(dep, _)| dep)
            .collect()
    };

    let mut to_table: Vec<(TextRange, String)> = Vec::new();
    let mut to_string: Vec<(TextRange, String)> = Vec::new();
    for dep in &deps {
        match doc.dom.get(dep) {
            Some(Item::Value(v)) => {
                if let ValueKind::String(s) = &v.kind {
                    to_table.push((v.range, format!("{{ version = {} }}", quote(s))));
                }
            }
            Some(Item::Table(t)) if t.kind == toml_dom::TableKind::Inline && t.len() == 1 => {
                if let Some(version) = string_at(doc, &with(dep, "version")) {
                    to_string.push((t.range, quote(&version)));
                }
            }
            _ => {}
        }
    }
    let fix = |edits: Vec<(TextRange, String)>, one: String, many: &str| {
        let title = if edits.len() == 1 {
            one
        } else {
            format!("Convert {} dependencies to {many}", edits.len())
        };
        crate::Fix { title, edits, preferred: false }
    };
    let mut out = Vec::new();
    if let Some((_, text)) = to_table.first() {
        let one = format!("Convert to `{text}`");
        out.push(fix(to_table, one, "`{ version = ... }`"));
    }
    if let Some((_, text)) = to_string.first() {
        let one = format!("Convert to `{text}`");
        out.push(fix(to_string, one, "`\"...\"`"));
    }
    out
}
