//! Validation of a document against a schema.

use std::cell::RefCell;

use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::Value;
use toml_dom::{ItemRef, Path, PathSegment, Table, ValueKind, display_path};

use crate::{Registry, SchemaRef, format};

/// How bad a problem is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// The document does not match the schema.
    Error,
    /// Allowed but discouraged, e.g. deprecated.
    Warning,
}

/// What a [`ValidationError`] points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// The item at the path.
    Value,
    /// The key of the item at the path.
    Key,
}

/// A mismatch between the document and its schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationError {
    /// The item in error.
    pub path: Path,
    /// Whether the key or the value is in error.
    pub target: Target,
    /// What is wrong.
    pub message: String,
    /// How bad.
    pub severity: Severity,
    /// Ways to fix the problem, best first.
    pub suggestions: Vec<Suggestion>,
    /// The key is deprecated: editors strike it through.
    pub deprecated: bool,
}

/// A fix of a [`ValidationError`], for quick fixes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Suggestion {
    /// Rename the key, e.g. a typo.
    RenameKey(String),
    /// Remove the key and its value.
    RemoveKey,
    /// Add missing keys to the table: their name, default value and type.
    AddKeys(Vec<(String, Option<Value>, Option<String>)>),
    /// Replace the value.
    ReplaceValue(Value),
    /// Write the value as a string: `1` to `"1"`.
    Quote,
    /// Write the string as this value: `"1"` to `1`.
    Unquote(Value),
    /// Make the value the single item of an array.
    WrapInArray,
}

/// Validates the document whose root table is `root` against `schema`.
///
/// `ignored_root_keys` are keys of the root table that the schema does not
/// see, such as `$schema`.
pub fn validate(
    registry: &Registry,
    schema: &SchemaRef<'_>,
    root: &Table,
    ignored_root_keys: &[&str],
) -> Vec<ValidationError> {
    let v = Validator { registry, regexes: RefCell::default(), ignored_root_keys };
    let mut out = Vec::new();
    v.validate(schema, ItemRef::Table(root), &mut Path::new(), &mut out, 0);
    // The same problem may be found through several subschemas.
    let mut seen = FxHashSet::default();
    out.retain(|e| seen.insert((e.path.clone(), e.message.clone())));
    out
}

/// Nested `$ref`s deeper than this are assumed to loop.
const MAX_DEPTH: usize = 64;

struct Validator<'r, 'k> {
    registry: &'r Registry,
    regexes: RefCell<FxHashMap<String, Option<Regex>>>,
    ignored_root_keys: &'k [&'k str],
}

/// What a schema evaluated, for `unevaluatedProperties` and
/// `unevaluatedItems`.
#[derive(Default)]
struct Evaluated {
    props: FxHashSet<String>,
    all_props: bool,
    items: usize,
    all_items: bool,
}

impl Evaluated {
    fn merge(&mut self, other: Evaluated) {
        self.props.extend(other.props);
        self.all_props |= other.all_props;
        self.items = self.items.max(other.items);
        self.all_items |= other.all_items;
    }
}

fn error(path: &Path, message: String) -> ValidationError {
    ValidationError {
        path: path.clone(),
        target: Target::Value,
        message,
        severity: Severity::Error,
        suggestions: Vec::new(),
        deprecated: false,
    }
}

/// The sentence of a description that mentions the deprecation.
fn deprecation_sentence(description: &str) -> Option<String> {
    let flat = description.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.split_inclusive(". ")
        .find(|s| {
            let l = s.to_lowercase();
            l.contains("deprecat")
                || l.contains("instead")
                || l.starts_with("use ")
                || l.contains("replaced by")
                || l.contains("renamed to")
        })
        .map(|s| {
            let s = s.trim();
            // `Deprecated: use x` after "`key` is deprecated: ".
            let rest = s
                .get(..11)
                .filter(|p| p.eq_ignore_ascii_case("deprecated:"))
                .map_or(s, |_| s[11..].trim());
            rest.to_owned()
        })
}

/// The key that replaces a deprecated one, from `use `x` instead`,
/// `replaced by `x``, `renamed to `x``, `in favor of `x``...
fn replacement_key(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    for phrase in
        ["replaced by", "renamed to", "in favor of", "in favour of", "superseded by", "use"]
    {
        let mut from = 0;
        while let Some(i) = lower[from..].find(phrase) {
            let after = &text[from + i + phrase.len()..];
            from += i + phrase.len();
            let after = after.trim_start();
            let Some(quote) = after.chars().next().filter(|c| matches!(c, '`' | '"' | '\'')) else {
                continue;
            };
            let inner = &after[1..];
            let Some(end) = inner.find(quote) else { continue };
            let key = &inner[..end];
            // A plain key: not a path, not a value.
            if !key.is_empty() && key.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
                return Some(key.to_owned());
            }
        }
    }
    None
}

impl ValidationError {
    fn suggest(mut self, suggestions: impl IntoIterator<Item = Suggestion>) -> ValidationError {
        self.suggestions.extend(suggestions);
        self
    }
}

/// How a value of the wrong type may be turned into one of `ty`.
fn type_suggestions(ty: &Value, item: ItemRef<'_>) -> Vec<Suggestion> {
    let allows = |t: &str| match ty {
        Value::String(s) => s == t,
        Value::Array(a) => a.iter().any(|v| v.as_str() == Some(t)),
        _ => false,
    };
    let mut out = Vec::new();
    if let ItemRef::Value(v) = item {
        match &v.kind {
            ValueKind::String(s) => {
                let parsed = match s.trim() {
                    "true" if allows("boolean") => Some(Value::Bool(true)),
                    "false" if allows("boolean") => Some(Value::Bool(false)),
                    t => t
                        .parse::<i64>()
                        .ok()
                        .filter(|_| allows("integer") || allows("number"))
                        .map(Value::from)
                        .or_else(|| {
                            t.parse::<f64>()
                                .ok()
                                .filter(|f| f.is_finite() && allows("number"))
                                .and_then(serde_json::Number::from_f64)
                                .map(Value::Number)
                        }),
                };
                out.extend(parsed.map(Suggestion::Unquote));
            }
            ValueKind::Integer(_)
            | ValueKind::Float(_)
            | ValueKind::Bool(_)
            | ValueKind::DateTime(..)
                if allows("string") =>
            {
                out.push(Suggestion::Quote);
            }
            _ => {}
        }
    }
    if allows("array") && !matches!(item, ItemRef::Array(_)) {
        out.push(Suggestion::WrapInArray);
    }
    out
}

impl<'r> Validator<'r, '_> {
    fn regex(&self, pattern: &str) -> Option<Regex> {
        self.regexes
            .borrow_mut()
            .entry(pattern.to_owned())
            .or_insert_with(|| Regex::new(&format::translate_regex(pattern)).ok())
            .clone()
    }

    /// The keys of a table that the schema sees.
    fn keys<'t>(&self, table: &'t Table, path: &Path) -> Vec<(&'t str, ItemRef<'t>)> {
        table
            .entries()
            .iter()
            .filter(|e| !path.is_empty() || !self.ignored_root_keys.contains(&e.key.as_str()))
            .map(|e| (e.key.as_str(), e.item.as_ref()))
            .collect()
    }

    fn validate(
        &self,
        schema: &SchemaRef<'r>,
        item: ItemRef<'_>,
        path: &mut Path,
        out: &mut Vec<ValidationError>,
        depth: usize,
    ) -> Evaluated {
        let mut eval = Evaluated::default();
        if depth > MAX_DEPTH {
            return eval;
        }
        // Malformed values are syntax errors already.
        if let ItemRef::Value(v) = item
            && matches!(v.kind, ValueKind::Invalid)
        {
            return eval;
        }
        let obj = match schema.value {
            Value::Bool(true) => return eval,
            Value::Bool(false) => {
                out.push(error(path, "not allowed by the schema".to_owned()));
                return eval;
            }
            Value::Object(obj) => obj,
            _ => return eval,
        };

        for key in ["$ref", "$dynamicRef", "$recursiveRef"] {
            if let Some(reference) = obj.get(key).and_then(Value::as_str) {
                // A missing document is reported by `Registry::missing`; its
                // schema is skipped meanwhile.
                if let Some(target) = self.registry.resolve(&schema.base, reference) {
                    eval.merge(self.validate(&target, item, path, out, depth + 1));
                }
            }
        }

        if let Some(ty) = obj.get("type")
            && !matches_type(ty, item)
        {
            out.push(
                error(
                    path,
                    format!("expected {}, found {}", describe_type(ty), describe_item(item)),
                )
                .suggest(type_suggestions(ty, item)),
            );
            // Other keywords would only add noise.
            return eval;
        }

        if let Some(values) = obj.get("enum").and_then(Value::as_array) {
            let json = to_json(item);
            if !values.iter().any(|v| json_eq(v, &json)) {
                let choices = values.iter().take(10).cloned().map(Suggestion::ReplaceValue);
                out.push(
                    error(path, format!("expected one of {}", list_values(values)))
                        .suggest(choices),
                );
            }
        }
        if let Some(expected) = obj.get("const")
            && !json_eq(expected, &to_json(item))
        {
            out.push(
                error(path, format!("expected `{}`", show_value(expected)))
                    .suggest([Suggestion::ReplaceValue(expected.clone())]),
            );
        }

        if obj.get("deprecated").and_then(Value::as_bool) == Some(true) && !path.is_empty() {
            let mut message = format!("`{}` is deprecated", display_path(path));
            // `deprecationMessage` (VS Code), else what the description says
            // about the deprecation; not its first line, which describes the
            // key.
            let explanation = obj
                .get("deprecationMessage")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    obj.get("description").and_then(Value::as_str).and_then(deprecation_sentence)
                });
            let mut suggestions = Vec::new();
            if let Some(e) = &explanation {
                message.push_str(": ");
                message.push_str(e);
                suggestions.extend(replacement_key(e).map(Suggestion::RenameKey));
            }
            suggestions.push(Suggestion::RemoveKey);
            out.push(ValidationError {
                path: path.clone(),
                target: Target::Key,
                message,
                severity: Severity::Warning,
                suggestions,
                deprecated: true,
            });
        }

        match item {
            ItemRef::Value(v) => self.scalar(obj, &v.kind, path, out),
            ItemRef::Array(a) => {
                let items: Vec<ItemRef<'_>> = a.items.iter().map(|i| i.as_ref()).collect();
                self.array(schema, obj, &items, path, out, depth, &mut eval);
            }
            ItemRef::Table(t) => self.object(schema, obj, t, path, out, depth, &mut eval),
        }

        self.applicators(schema, obj, item, path, out, depth, &mut eval);

        // After every other keyword: they decide what is evaluated.
        if let ItemRef::Table(t) = item
            && let Some(unevaluated) = obj.get("unevaluatedProperties")
            && !eval.all_props
        {
            for (key, child) in self.keys(t, path) {
                if eval.props.contains(key) {
                    continue;
                }
                path.push(PathSegment::Key(key.to_owned()));
                self.additional(schema, unevaluated, key, child, path, out, depth);
                path.pop();
            }
            eval.all_props = true;
        }
        if let ItemRef::Array(a) = item
            && let Some(unevaluated) = obj.get("unevaluatedItems")
            && !eval.all_items
        {
            let sub = schema.child(unevaluated);
            for (i, child) in a.items.iter().enumerate().skip(eval.items) {
                path.push(PathSegment::Index(i));
                self.validate(&sub, child.as_ref(), path, out, depth + 1);
                path.pop();
            }
            eval.all_items = true;
        }
        eval
    }

    /// `allOf`, `anyOf`, `oneOf`, `not`, `if`.
    fn applicators(
        &self,
        schema: &SchemaRef<'r>,
        obj: &'r serde_json::Map<String, Value>,
        item: ItemRef<'_>,
        path: &mut Path,
        out: &mut Vec<ValidationError>,
        depth: usize,
        eval: &mut Evaluated,
    ) {
        if let Some(all) = obj.get("allOf").and_then(Value::as_array) {
            for sub in all {
                eval.merge(self.validate(&schema.child(sub), item, path, out, depth + 1));
            }
        }
        if let Some(any) = obj.get("anyOf").and_then(Value::as_array) {
            let results: Vec<_> =
                any.iter().map(|sub| self.trial(&schema.child(sub), item, path, depth)).collect();
            if results.iter().any(|(errors, _)| errors.is_empty()) {
                for (errors, e) in results {
                    if errors.is_empty() {
                        eval.merge(e);
                    }
                }
            } else {
                self.report_alternatives(any, schema, results, item, path, out);
            }
        }
        if let Some(one) = obj.get("oneOf").and_then(Value::as_array) {
            let results: Vec<_> =
                one.iter().map(|sub| self.trial(&schema.child(sub), item, path, depth)).collect();
            let passed = results.iter().filter(|(errors, _)| errors.is_empty()).count();
            match passed {
                0 => self.report_alternatives(one, schema, results, item, path, out),
                1 => {
                    if let Some((_, e)) = results.into_iter().find(|(errors, _)| errors.is_empty())
                    {
                        eval.merge(e);
                    }
                }
                _ => out.push(error(
                    path,
                    "matches more than one of the allowed schemas (`oneOf`)".to_owned(),
                )),
            }
        }
        if let Some(not) = obj.get("not") {
            let (errors, _) = self.trial(&schema.child(not), item, path, depth);
            if errors.is_empty() {
                out.push(error(path, "must not match the schema of `not`".to_owned()));
            }
        }
        if let Some(cond) = obj.get("if") {
            let (errors, e) = self.trial(&schema.child(cond), item, path, depth);
            let branch = if errors.is_empty() {
                eval.merge(e);
                obj.get("then")
            } else {
                obj.get("else")
            };
            if let Some(branch) = branch {
                eval.merge(self.validate(&schema.child(branch), item, path, out, depth + 1));
            }
        }
    }

    /// Validates without reporting.
    fn trial(
        &self,
        schema: &SchemaRef<'r>,
        item: ItemRef<'_>,
        path: &mut Path,
        depth: usize,
    ) -> (Vec<ValidationError>, Evaluated) {
        let mut errors = Vec::new();
        let eval = self.validate(schema, item, path, &mut errors, depth + 1);
        // Warnings do not make an alternative fail.
        errors.retain(|e| e.severity == Severity::Error);
        (errors, eval)
    }

    /// No alternative of `anyOf`/`oneOf` matches: shows the problems of the
    /// alternative that came closest, when one has the right type.
    fn report_alternatives(
        &self,
        alternatives: &'r [Value],
        schema: &SchemaRef<'r>,
        results: Vec<(Vec<ValidationError>, Evaluated)>,
        item: ItemRef<'_>,
        path: &Path,
        out: &mut Vec<ValidationError>,
    ) {
        let candidates = results
            .into_iter()
            .zip(alternatives)
            .filter(|(_, sub)| self.type_fits(&schema.child(sub), item, 0))
            .map(|((errors, _), _)| errors)
            .min_by_key(|errors| errors.len());
        match candidates {
            Some(errors) => out.extend(errors),
            None => {
                let types: Vec<String> = alternatives
                    .iter()
                    .filter_map(|sub| self.declared_type(&schema.child(sub), 0))
                    .collect();
                let message = if types.is_empty() {
                    "does not match any of the allowed schemas".to_owned()
                } else {
                    let mut types = types;
                    types.dedup();
                    format!("expected {}, found {}", types.join(" or "), describe_item(item))
                };
                out.push(error(path, message));
            }
        }
    }

    /// Whether the `type` of `schema`, if any, accepts `item`.
    fn type_fits(&self, schema: &SchemaRef<'r>, item: ItemRef<'_>, depth: usize) -> bool {
        if depth > 8 {
            return true;
        }
        if let Some(ty) = schema.get("type") {
            return matches_type(ty, item);
        }
        if let Some(r) = schema.get("$ref").and_then(Value::as_str)
            && let Some(target) = self.registry.resolve(&schema.base, r)
        {
            return self.type_fits(&target, item, depth + 1);
        }
        if let Some(c) = schema.get("const") {
            return json_eq(c, &to_json(item));
        }
        true
    }

    /// The `type` of `schema`, for messages.
    fn declared_type(&self, schema: &SchemaRef<'r>, depth: usize) -> Option<String> {
        if depth > 8 {
            return None;
        }
        if let Some(ty) = schema.get("type") {
            return Some(describe_type(ty));
        }
        if let Some(c) = schema.get("const") {
            return Some(format!("`{}`", show_value(c)));
        }
        let r = schema.get("$ref").and_then(Value::as_str)?;
        let target = self.registry.resolve(&schema.base, r)?;
        self.declared_type(&target, depth + 1)
    }

    fn scalar(
        &self,
        obj: &serde_json::Map<String, Value>,
        value: &ValueKind,
        path: &Path,
        out: &mut Vec<ValidationError>,
    ) {
        let number = match value {
            ValueKind::Integer(i) => Some(*i as f64),
            ValueKind::Float(f) => Some(*f),
            _ => None,
        };
        if let Some(n) = number {
            self.number(obj, n, path, out);
        }
        let text = match value {
            ValueKind::String(s) => s.as_str(),
            ValueKind::DateTime(_, s) => s.as_str(),
            _ => return,
        };
        let len = text.chars().count() as u64;
        if let Some(min) = obj.get("minLength").and_then(Value::as_u64)
            && len < min
        {
            out.push(error(path, format!("expected at least {min} characters, found {len}")));
        }
        if let Some(max) = obj.get("maxLength").and_then(Value::as_u64)
            && len > max
        {
            out.push(error(path, format!("expected at most {max} characters, found {len}")));
        }
        if let Some(pattern) = obj.get("pattern").and_then(Value::as_str)
            && let Some(re) = self.regex(pattern)
            && !re.is_match(text)
        {
            out.push(error(path, format!("does not match the pattern `{pattern}`")));
        }
        if let Some(name) = obj.get("format").and_then(Value::as_str) {
            let ok = match value {
                ValueKind::DateTime(kind, _) => format::date_time_fits(name, *kind),
                _ => format::check(name, text),
            };
            if !ok {
                out.push(error(path, format!("invalid {}", format::describe(name, value))));
            }
        }
    }

    fn number(
        &self,
        obj: &serde_json::Map<String, Value>,
        n: f64,
        path: &Path,
        out: &mut Vec<ValidationError>,
    ) {
        let num = |k: &str| obj.get(k).and_then(Value::as_f64);
        let flag = |k: &str| obj.get(k).and_then(Value::as_bool).unwrap_or(false);
        if let Some(min) = num("minimum") {
            // Draft 4: `exclusiveMinimum` is a boolean modifier.
            if flag("exclusiveMinimum") {
                if n <= min {
                    out.push(error(path, format!("must be greater than {}", show_number(min))));
                }
            } else if n < min {
                out.push(error(path, format!("must be at least {}", show_number(min))));
            }
        }
        if let Some(max) = num("maximum") {
            if flag("exclusiveMaximum") {
                if n >= max {
                    out.push(error(path, format!("must be less than {}", show_number(max))));
                }
            } else if n > max {
                out.push(error(path, format!("must be at most {}", show_number(max))));
            }
        }
        if let Some(min) = num("exclusiveMinimum")
            && n <= min
        {
            out.push(error(path, format!("must be greater than {}", show_number(min))));
        }
        if let Some(max) = num("exclusiveMaximum")
            && n >= max
        {
            out.push(error(path, format!("must be less than {}", show_number(max))));
        }
        if let Some(m) = num("multipleOf")
            && m > 0.0
        {
            let q = n / m;
            if (q - q.round()).abs() > 1e-9 {
                out.push(error(path, format!("must be a multiple of {}", show_number(m))));
            }
        }
    }

    fn array(
        &self,
        schema: &SchemaRef<'r>,
        obj: &'r serde_json::Map<String, Value>,
        items: &[ItemRef<'_>],
        path: &mut Path,
        out: &mut Vec<ValidationError>,
        depth: usize,
        eval: &mut Evaluated,
    ) {
        let len = items.len() as u64;
        if let Some(min) = obj.get("minItems").and_then(Value::as_u64)
            && len < min
        {
            out.push(error(path, format!("expected at least {min} items, found {len}")));
        }
        if let Some(max) = obj.get("maxItems").and_then(Value::as_u64)
            && len > max
        {
            out.push(error(path, format!("expected at most {max} items, found {len}")));
        }
        if obj.get("uniqueItems").and_then(Value::as_bool) == Some(true) {
            let json: Vec<Value> = items.iter().map(|&i| to_json(i)).collect();
            'outer: for i in 0..json.len() {
                for j in 0..i {
                    if json_eq(&json[i], &json[j]) {
                        path.push(PathSegment::Index(i));
                        out.push(error(
                            path,
                            format!("items must be unique: this item repeats item {j}"),
                        ));
                        path.pop();
                        break 'outer;
                    }
                }
            }
        }

        // The schemas of the leading items.
        let tuple: &[Value] = match (obj.get("prefixItems"), obj.get("items")) {
            (Some(Value::Array(p)), _) => p,
            (_, Some(Value::Array(p))) => p,
            _ => &[],
        };
        for (i, (sub, &child)) in tuple.iter().zip(items).enumerate() {
            path.push(PathSegment::Index(i));
            self.validate(&schema.child(sub), child, path, out, depth + 1);
            path.pop();
        }
        eval.items = eval.items.max(tuple.len().min(items.len()));
        let rest = match (obj.get("prefixItems"), obj.get("items")) {
            (Some(_), Some(rest)) if !rest.is_array() => Some(rest),
            (None, Some(Value::Array(_))) => obj.get("additionalItems"),
            (None, Some(rest)) => Some(rest),
            _ => None,
        };
        if let Some(rest) = rest {
            if rest == &Value::Bool(false) && items.len() > tuple.len() {
                path.push(PathSegment::Index(tuple.len()));
                out.push(error(
                    path,
                    format!("expected at most {} items, found {len}", tuple.len()),
                ));
                path.pop();
            } else {
                let sub = schema.child(rest);
                for (i, &child) in items.iter().enumerate().skip(tuple.len()) {
                    path.push(PathSegment::Index(i));
                    self.validate(&sub, child, path, out, depth + 1);
                    path.pop();
                }
            }
            eval.all_items = true;
        }

        if let Some(contains) = obj.get("contains") {
            let sub = schema.child(contains);
            let mut matching = 0u64;
            for (i, &child) in items.iter().enumerate() {
                path.push(PathSegment::Index(i));
                if self.trial(&sub, child, path, depth).0.is_empty() {
                    matching += 1;
                }
                path.pop();
            }
            let min = obj.get("minContains").and_then(Value::as_u64).unwrap_or(1);
            if matching < min {
                out.push(error(
                    path,
                    format!("expected at least {min} items matching `contains`, found {matching}"),
                ));
            }
            if let Some(max) = obj.get("maxContains").and_then(Value::as_u64)
                && matching > max
            {
                out.push(error(
                    path,
                    format!("expected at most {max} items matching `contains`, found {matching}"),
                ));
            }
        }
    }

    fn object(
        &self,
        schema: &SchemaRef<'r>,
        obj: &'r serde_json::Map<String, Value>,
        table: &Table,
        path: &mut Path,
        out: &mut Vec<ValidationError>,
        depth: usize,
        eval: &mut Evaluated,
    ) {
        let keys = self.keys(table, path);
        let has = |k: &str| keys.iter().any(|(key, _)| *key == k);
        let len = keys.len() as u64;
        if let Some(min) = obj.get("minProperties").and_then(Value::as_u64)
            && len < min
        {
            out.push(error(path, format!("expected at least {min} keys, found {len}")));
        }
        if let Some(max) = obj.get("maxProperties").and_then(Value::as_u64)
            && len > max
        {
            out.push(error(path, format!("expected at most {max} keys, found {len}")));
        }
        if let Some(required) = obj.get("required").and_then(Value::as_array) {
            let missing: Vec<&str> =
                required.iter().filter_map(Value::as_str).filter(|k| !has(k)).collect();
            if !missing.is_empty() {
                let list: Vec<String> = missing.iter().map(|k| format!("`{k}`")).collect();
                let s = if missing.len() > 1 { "s" } else { "" };
                let keys = missing
                    .iter()
                    .map(|k| {
                        let (default, ty) = self.property_hint(schema, k);
                        ((*k).to_owned(), default, ty)
                    })
                    .collect();
                out.push(
                    error(path, format!("missing required key{s} {}", list.join(", ")))
                        .suggest([Suggestion::AddKeys(keys)]),
                );
            }
        }
        if let Some(deps) = obj.get("dependentRequired").and_then(Value::as_object) {
            self.dependent_required(deps, &has, path, out);
        }
        // Draft 7 `dependencies`: required keys or a schema.
        if let Some(deps) = obj.get("dependencies").and_then(Value::as_object) {
            self.dependent_required(deps, &has, path, out);
            for (key, sub) in deps {
                if (sub.is_object() || sub.is_boolean()) && has(key) {
                    eval.merge(self.validate(
                        &schema.child(sub),
                        ItemRef::Table(table),
                        path,
                        out,
                        depth + 1,
                    ));
                }
            }
        }
        if let Some(deps) = obj.get("dependentSchemas").and_then(Value::as_object) {
            for (key, sub) in deps {
                if has(key) {
                    eval.merge(self.validate(
                        &schema.child(sub),
                        ItemRef::Table(table),
                        path,
                        out,
                        depth + 1,
                    ));
                }
            }
        }
        if let Some(names) = obj.get("propertyNames") {
            let sub = schema.child(names);
            for (key, _) in &keys {
                let mut errors = Vec::new();
                let name = toml_dom::Value {
                    kind: ValueKind::String((*key).to_owned()),
                    range: Default::default(),
                };
                self.validate(
                    &sub,
                    ItemRef::Value(&name),
                    &mut Path::new(),
                    &mut errors,
                    depth + 1,
                );
                path.push(PathSegment::Key((*key).to_owned()));
                for e in errors {
                    out.push(ValidationError {
                        path: path.clone(),
                        target: Target::Key,
                        message: format!("invalid key: {}", e.message),
                        severity: e.severity,
                        suggestions: Vec::new(),
                        deprecated: false,
                    });
                }
                path.pop();
            }
        }

        let properties = obj.get("properties").and_then(Value::as_object);
        let patterns: Vec<(Regex, &'r Value)> = obj
            .get("patternProperties")
            .and_then(Value::as_object)
            .map(|p| p.iter().filter_map(|(pat, sub)| Some((self.regex(pat)?, sub))).collect())
            .unwrap_or_default();
        let additional = obj.get("additionalProperties");
        for (key, child) in keys {
            path.push(PathSegment::Key(key.to_owned()));
            let mut matched = false;
            if let Some(sub) = properties.and_then(|p| p.get(key)) {
                matched = true;
                self.validate(&schema.child(sub), child, path, out, depth + 1);
            }
            for (re, sub) in &patterns {
                if re.is_match(key) {
                    matched = true;
                    self.validate(&schema.child(sub), child, path, out, depth + 1);
                }
            }
            if matched {
                eval.props.insert(key.to_owned());
            } else if let Some(additional) = additional {
                self.additional(schema, additional, key, child, path, out, depth);
                eval.props.insert(key.to_owned());
            }
            path.pop();
        }
        if additional.is_some() {
            eval.all_props = true;
        }
    }

    fn dependent_required(
        &self,
        deps: &serde_json::Map<String, Value>,
        has: &dyn Fn(&str) -> bool,
        path: &Path,
        out: &mut Vec<ValidationError>,
    ) {
        for (key, needed) in deps {
            let Some(needed) = needed.as_array() else { continue };
            if !has(key) {
                continue;
            }
            for n in needed.iter().filter_map(Value::as_str) {
                if !has(n) {
                    out.push(error(path, format!("the key `{key}` requires the key `{n}`")));
                }
            }
        }
    }

    /// A key matched by no `properties` nor `patternProperties`.
    fn additional(
        &self,
        schema: &SchemaRef<'r>,
        additional: &'r Value,
        key: &str,
        child: ItemRef<'_>,
        path: &mut Path,
        out: &mut Vec<ValidationError>,
        depth: usize,
    ) {
        if additional == &Value::Bool(false) {
            let mut message = format!("unknown key `{key}`");
            let known: Vec<&str> = schema
                .get("properties")
                .and_then(Value::as_object)
                .map_or(Vec::new(), |p| p.keys().map(String::as_str).collect());
            let mut suggestions = Vec::new();
            if let Some(similar) = closest(key, &known) {
                message.push_str(&format!(", did you mean `{similar}`?"));
                suggestions.push(Suggestion::RenameKey(similar.to_owned()));
            }
            suggestions.push(Suggestion::RemoveKey);
            out.push(ValidationError {
                path: path.clone(),
                target: Target::Key,
                message,
                severity: Severity::Error,
                suggestions,
                deprecated: false,
            });
        } else {
            self.validate(&schema.child(additional), child, path, out, depth + 1);
        }
    }

    /// The `default` and `type` of the property `key` of `schema`, for a
    /// placeholder value.
    fn property_hint(&self, schema: &SchemaRef<'r>, key: &str) -> (Option<Value>, Option<String>) {
        let Some(mut sub) =
            schema.get("properties").and_then(|p| p.get(key)).map(|s| schema.child(s))
        else {
            return (None, None);
        };
        for _ in 0..8 {
            let default = sub.get("default").cloned();
            let ty = match sub.get("type") {
                Some(Value::String(t)) => Some(t.clone()),
                Some(Value::Array(ts)) => ts.iter().find_map(Value::as_str).map(str::to_owned),
                _ => None,
            };
            if default.is_some() || ty.is_some() {
                return (default, ty);
            }
            let Some(target) = sub
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|r| self.registry.resolve(&sub.base, r))
            else {
                break;
            };
            sub = target;
        }
        (None, None)
    }
}

/// The closest name to `key` among `known`, for typos.
fn closest<'a>(key: &str, known: &[&'a str]) -> Option<&'a str> {
    let lower = key.to_lowercase();
    known
        .iter()
        .map(|k| (levenshtein(&lower, &k.to_lowercase()), *k))
        .filter(|(d, k)| *d <= k.len().max(key.len()).div_ceil(3) && *d < k.len())
        .min()
        .map(|(_, k)| k)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

fn matches_type(ty: &Value, item: ItemRef<'_>) -> bool {
    match ty {
        Value::String(t) => matches_one_type(t, item),
        Value::Array(types) => {
            types.iter().filter_map(Value::as_str).any(|t| matches_one_type(t, item))
        }
        _ => true,
    }
}

fn matches_one_type(ty: &str, item: ItemRef<'_>) -> bool {
    match (ty, item) {
        ("object", ItemRef::Table(_)) | ("array", ItemRef::Array(_)) => true,
        (_, ItemRef::Value(v)) => match (ty, &v.kind) {
            ("string", ValueKind::String(_) | ValueKind::DateTime(..)) => true,
            ("integer", ValueKind::Integer(_)) => true,
            ("integer", ValueKind::Float(f)) => f.fract() == 0.0 && f.is_finite(),
            ("number", ValueKind::Integer(_) | ValueKind::Float(_)) => true,
            ("boolean", ValueKind::Bool(_)) => true,
            _ => false,
        },
        _ => false,
    }
}

fn describe_type(ty: &Value) -> String {
    let name = |t: &str| match t {
        "object" => "a table".to_owned(),
        "array" => "an array".to_owned(),
        "integer" => "an integer".to_owned(),
        other => format!("a {other}"),
    };
    match ty {
        Value::String(t) => name(t),
        Value::Array(types) => {
            types.iter().filter_map(Value::as_str).map(name).collect::<Vec<_>>().join(" or ")
        }
        _ => "a value".to_owned(),
    }
}

fn describe_item(item: ItemRef<'_>) -> String {
    match item {
        ItemRef::Table(_) => "a table".to_owned(),
        ItemRef::Array(_) => "an array".to_owned(),
        ItemRef::Value(v) => match &v.kind {
            ValueKind::String(_) => "a string".to_owned(),
            ValueKind::Integer(_) => "an integer".to_owned(),
            ValueKind::Float(_) => "a float".to_owned(),
            ValueKind::Bool(_) => "a boolean".to_owned(),
            ValueKind::DateTime(..) => "a date-time".to_owned(),
            ValueKind::Invalid => "an invalid value".to_owned(),
        },
    }
}

pub(crate) fn to_json(item: ItemRef<'_>) -> Value {
    match item {
        ItemRef::Table(t) => t.to_json(),
        ItemRef::Array(a) => Value::Array(a.items.iter().map(toml_dom::Item::to_json).collect()),
        ItemRef::Value(v) => v.to_json(),
    }
}

/// JSON equality where `1` equals `1.0`.
pub(crate) fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| json_eq(a, b))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| json_eq(v, w)))
        }
        _ => a == b,
    }
}

/// A value as TOML would write it.
pub(crate) fn show_value(v: &Value) -> String {
    match v {
        Value::String(s) => format!("{s:?}"),
        Value::Null => "null".to_owned(),
        other => other.to_string(),
    }
}

fn list_values(values: &[Value]) -> String {
    let mut shown: Vec<String> =
        values.iter().take(8).map(|v| format!("`{}`", show_value(v))).collect();
    if values.len() > 8 {
        shown.push(format!("... ({} more)", values.len() - 8));
    }
    shown.join(", ")
}

fn show_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 { format!("{}", n as i64) } else { n.to_string() }
}
