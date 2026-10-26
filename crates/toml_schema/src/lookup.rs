//! Finding the schemas that apply at a path, for completion and hover.
//!
//! Unlike validation, lookup does not know which alternative of `anyOf` or
//! `oneOf` applies: all of them do, and `if` contributes both `then` and
//! `else`.

use rustc_hash::FxHashSet;
use serde_json::Value;
use toml_dom::PathSegment;

use crate::{Registry, SchemaRef};

/// At most this many schemas are collected, against huge or looping schemas.
const LIMIT: usize = 256;

/// The schemas that apply to `schemas` themselves: the targets of their
/// `$ref`s and their applicators, recursively. Boolean schemas are dropped.
pub fn expand<'r>(registry: &'r Registry, schemas: Vec<SchemaRef<'r>>) -> Vec<SchemaRef<'r>> {
    let mut out = Vec::new();
    let mut seen = FxHashSet::default();
    let mut queue: std::collections::VecDeque<SchemaRef<'r>> = schemas.into();
    while let Some(schema) = queue.pop_front() {
        if out.len() >= LIMIT {
            break;
        }
        let Some(obj) = schema.value.as_object() else { continue };
        if !seen.insert(schema.value as *const Value) {
            continue;
        }
        for key in ["$ref", "$dynamicRef", "$recursiveRef"] {
            if let Some(r) = obj.get(key).and_then(Value::as_str)
                && let Some(target) = registry.resolve(&schema.base, r)
            {
                queue.push_back(target);
            }
        }
        for key in ["allOf", "anyOf", "oneOf"] {
            for sub in obj.get(key).and_then(Value::as_array).into_iter().flatten() {
                queue.push_back(schema.child(sub));
            }
        }
        for key in ["then", "else"] {
            if let Some(sub) = obj.get(key) {
                queue.push_back(schema.child(sub));
            }
        }
        out.push(schema);
    }
    out
}

/// The schemas of the item at `path` below a document validated by `root`.
pub fn schemas_at<'r>(
    registry: &'r Registry,
    root: &SchemaRef<'r>,
    path: &[PathSegment],
) -> Vec<SchemaRef<'r>> {
    let mut current = expand(registry, vec![root.clone()]);
    for seg in path {
        let mut next = Vec::new();
        for schema in &current {
            match seg {
                PathSegment::Key(key) => property(schema, key, &mut next),
                PathSegment::Index(i) => item(schema, *i, &mut next),
            }
        }
        current = expand(registry, next);
        if current.is_empty() {
            break;
        }
    }
    current
}

fn property<'r>(schema: &SchemaRef<'r>, key: &str, out: &mut Vec<SchemaRef<'r>>) {
    let mut matched = false;
    if let Some(sub) = schema.get("properties").and_then(|p| p.get(key)) {
        out.push(schema.child(sub));
        matched = true;
    }
    if let Some(patterns) = schema.get("patternProperties").and_then(Value::as_object) {
        for (pattern, sub) in patterns {
            let re = regex::Regex::new(&crate::format::translate_regex(pattern));
            if re.is_ok_and(|re| re.is_match(key)) {
                out.push(schema.child(sub));
                matched = true;
            }
        }
    }
    if !matched {
        for k in ["additionalProperties", "unevaluatedProperties"] {
            if let Some(sub) = schema.get(k).filter(|s| s.is_object()) {
                out.push(schema.child(sub));
            }
        }
    }
}

fn item<'r>(schema: &SchemaRef<'r>, index: usize, out: &mut Vec<SchemaRef<'r>>) {
    let tuple = schema.get("prefixItems").or_else(|| schema.get("items").filter(|i| i.is_array()));
    if let Some(Value::Array(tuple)) = tuple {
        match tuple.get(index) {
            Some(sub) => out.push(schema.child(sub)),
            None => {
                let rest = if schema.get("prefixItems").is_some() {
                    schema.get("items")
                } else {
                    schema.get("additionalItems")
                };
                if let Some(rest) = rest.filter(|r| r.is_object()) {
                    out.push(schema.child(rest));
                }
            }
        }
    } else if let Some(sub) = schema.get("items").filter(|i| i.is_object()) {
        out.push(schema.child(sub));
    }
}

/// A key that a table may have.
#[derive(Clone, Debug)]
pub struct KeyInfo<'r> {
    /// The key.
    pub key: String,
    /// Its schema, not expanded.
    pub schema: SchemaRef<'r>,
    /// Listed in `required`.
    pub required: bool,
}

/// The keys declared by `schemas` (as [`schemas_at`] returns them), in
/// declaration order.
pub fn key_completions<'r>(schemas: &[SchemaRef<'r>]) -> Vec<KeyInfo<'r>> {
    let mut out: Vec<KeyInfo<'r>> = Vec::new();
    let mut required: FxHashSet<&str> = FxHashSet::default();
    for schema in schemas {
        for r in schema.get("required").and_then(Value::as_array).into_iter().flatten() {
            if let Some(r) = r.as_str() {
                required.insert(r);
            }
        }
        let Some(props) = schema.get("properties").and_then(Value::as_object) else { continue };
        for (key, sub) in props {
            if out.iter().any(|k| &k.key == key) {
                continue;
            }
            out.push(KeyInfo { key: key.clone(), schema: schema.child(sub), required: false });
        }
    }
    for k in &mut out {
        k.required = required.contains(k.key.as_str());
    }
    out
}

/// What the schemas of an item say about it, merged.
#[derive(Clone, Debug, Default)]
pub struct SchemaInfo {
    /// The first `title`.
    pub title: Option<String>,
    /// The first `markdownDescription` or `description`.
    pub description: Option<String>,
    /// The allowed types.
    pub types: Vec<String>,
    /// The allowed values, from `enum` and `const`.
    pub values: Vec<Value>,
    /// The first `default`.
    pub default: Option<Value>,
    /// The `examples`.
    pub examples: Vec<Value>,
    /// Marked `deprecated`.
    pub deprecated: bool,
    /// The keys of an object, or of the items of an array, are free.
    pub free_keys: bool,
}

impl SchemaInfo {
    /// Merges what `schemas` (expanded) say.
    pub fn of(schemas: &[SchemaRef<'_>]) -> SchemaInfo {
        let mut info = SchemaInfo::default();
        for schema in schemas {
            if info.title.is_none() {
                info.title = schema.title().map(str::to_owned);
            }
            if info.description.is_none() {
                info.description = schema.description().map(str::to_owned);
            }
            match schema.get("type") {
                Some(Value::String(t)) => push_unique(&mut info.types, t.clone()),
                Some(Value::Array(ts)) => {
                    for t in ts.iter().filter_map(Value::as_str) {
                        push_unique(&mut info.types, t.to_owned());
                    }
                }
                _ => {}
            }
            if let Some(values) = schema.get("enum").and_then(Value::as_array) {
                for v in values {
                    if !info.values.contains(v) {
                        info.values.push(v.clone());
                    }
                }
            }
            if let Some(c) = schema.get("const")
                && !info.values.contains(c)
            {
                info.values.push(c.clone());
            }
            if info.default.is_none() {
                info.default = schema.get("default").cloned();
            }
            if let Some(examples) = schema.get("examples").and_then(Value::as_array) {
                info.examples.extend(examples.iter().cloned());
            }
            info.deprecated |= schema.get("deprecated").and_then(Value::as_bool) == Some(true);
            info.free_keys |= schema.get("additionalProperties").is_some_and(Value::is_object)
                || schema.get("patternProperties").is_some();
        }
        info
    }
}

fn push_unique(v: &mut Vec<String>, s: String) {
    if !v.contains(&s) {
        v.push(s);
    }
}
