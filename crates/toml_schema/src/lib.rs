//! JSON Schema support for TOML documents.
//!
//! A TOML document maps to JSON naturally: tables are objects, arrays are
//! arrays, and dates and times are strings (with the `date-time`, `date` and
//! `time` formats). This crate validates a [`toml_dom::Dom`] against JSON
//! Schemas (drafts 4, 6, 7, 2019-09 and 2020-12) and finds the schemas that
//! apply at a path, for completion and hover.
//!
//! No I/O happens here: schemas are fetched by the caller and put into a
//! [`Registry`], which also tells which documents referenced by `$ref` are
//! still missing.

mod format;
mod lookup;
mod validate;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::Value;
use url::Url;

pub use crate::{
    lookup::{KeyInfo, SchemaInfo, expand, key_completions, schemas_at},
    validate::{Severity, Suggestion, Target, ValidationError, closest, validate},
};

/// The keywords whose values are data rather than subschemas.
const DATA_KEYWORDS: &[&str] = &["enum", "const", "default", "examples", "x-taplo", "x-tombi"];

/// The schema documents known so far, by URL.
#[derive(Clone, Debug, Default)]
pub struct Registry {
    docs: FxHashMap<String, Arc<Value>>,
    /// Embedded resources (`$id` inside a document): absolute URL to the
    /// document URL and the JSON pointer of the resource.
    ids: FxHashMap<String, (String, String)>,
    /// `$anchor`s and draft 7 `#name` ids: `url#name` to the document URL and
    /// the JSON pointer.
    anchors: FxHashMap<String, (String, String)>,
}

/// A schema and the URL its relative references resolve against.
#[derive(Clone, Debug)]
pub struct SchemaRef<'r> {
    /// The schema: an object or a boolean.
    pub value: &'r Value,
    /// The base URL.
    pub base: Url,
}

/// Turns `url` into a canonical key: no fragment, no trailing `#`.
pub fn normalize_url(url: &str) -> String {
    match Url::parse(url) {
        Ok(mut u) => {
            u.set_fragment(None);
            u.to_string()
        }
        Err(_) => url.split('#').next().unwrap_or(url).to_owned(),
    }
}

impl Registry {
    /// An empty registry.
    pub fn new() -> Registry {
        Registry::default()
    }

    /// Whether the document of `url` is loaded.
    pub fn contains(&self, url: &str) -> bool {
        let url = normalize_url(url);
        self.docs.contains_key(&url) || self.ids.contains_key(&url)
    }

    /// Adds the document fetched from `url`.
    pub fn insert(&mut self, url: &str, doc: Value) {
        // A reloaded document replaces the old one, its ids and anchors too.
        self.remove(url);
        let url = normalize_url(url);
        let doc = Arc::new(doc);
        let base = Url::parse(&url).unwrap_or_else(|_| fallback_base());
        // The `$id` of the root names the document too.
        if let Some(id) = doc.get("$id").or_else(|| doc.get("id")).and_then(Value::as_str)
            && !id.starts_with('#')
            && let Ok(abs) = base.join(id)
        {
            let abs = normalize_url(abs.as_str());
            if abs != url {
                self.ids.insert(abs, (url.clone(), String::new()));
            }
        }
        self.scan(&url, &doc, &base, &mut String::new(), true);
        self.docs.insert(url, doc);
    }

    /// Removes every document, e.g. to fetch them again.
    pub fn clear(&mut self) {
        *self = Registry::default();
    }

    /// Forgets the document of `url`, e.g. because its file changed.
    /// Returns whether it was loaded.
    pub fn remove(&mut self, url: &str) -> bool {
        let url = normalize_url(url);
        let removed = self.docs.remove(&url).is_some();
        self.ids.retain(|_, (doc, _)| *doc != url);
        self.anchors.retain(|_, (doc, _)| *doc != url);
        removed
    }

    /// The URLs of the loaded documents.
    pub fn urls(&self) -> impl Iterator<Item = &str> {
        self.docs.keys().map(String::as_str)
    }

    /// Indexes the `$id`s and anchors below `value`.
    fn scan(
        &mut self,
        doc_url: &str,
        value: &Value,
        base: &Url,
        pointer: &mut String,
        is_root: bool,
    ) {
        match value {
            Value::Object(map) => {
                let mut base = base.clone();
                if let Some(id) = map.get("$id").or_else(|| map.get("id")).and_then(Value::as_str) {
                    if let Some(anchor) = id.strip_prefix('#') {
                        self.add_anchor(&base, anchor, doc_url, pointer);
                    } else if let Ok(abs) = base.join(id) {
                        if !is_root {
                            self.ids.insert(
                                normalize_url(abs.as_str()),
                                (doc_url.to_owned(), pointer.clone()),
                            );
                        }
                        base = abs;
                    }
                }
                for key in ["$anchor", "$dynamicAnchor"] {
                    if let Some(anchor) = map.get(key).and_then(Value::as_str) {
                        self.add_anchor(&base, anchor, doc_url, pointer);
                    }
                }
                for (key, child) in map {
                    if DATA_KEYWORDS.contains(&key.as_str()) {
                        continue;
                    }
                    let len = pointer.len();
                    pointer.push('/');
                    pointer.push_str(&key.replace('~', "~0").replace('/', "~1"));
                    self.scan(doc_url, child, &base, pointer, false);
                    pointer.truncate(len);
                }
            }
            Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    let len = pointer.len();
                    pointer.push('/');
                    pointer.push_str(&i.to_string());
                    self.scan(doc_url, child, base, pointer, false);
                    pointer.truncate(len);
                }
            }
            _ => {}
        }
    }

    fn add_anchor(&mut self, base: &Url, anchor: &str, doc_url: &str, pointer: &str) {
        let mut key = base.clone();
        key.set_fragment(Some(anchor));
        self.anchors.insert(key.to_string(), (doc_url.to_owned(), pointer.to_owned()));
    }

    /// The root schema of the document of `url`.
    pub fn root(&self, url: &str) -> Option<SchemaRef<'_>> {
        let key = normalize_url(url);
        let doc = self.docs.get(&key)?;
        let mut base = Url::parse(&key).unwrap_or_else(|_| fallback_base());
        if let Some(id) = doc.get("$id").and_then(Value::as_str)
            && let Ok(abs) = base.join(id)
        {
            base = abs;
        }
        Some(SchemaRef { value: doc, base })
    }

    /// Resolves `reference` (a `$ref`) against `base`.
    pub fn resolve(&self, base: &Url, reference: &str) -> Option<SchemaRef<'_>> {
        let target = base.join(reference).ok()?;
        let fragment = target.fragment().unwrap_or("").to_owned();
        let mut doc_url = target.clone();
        doc_url.set_fragment(None);
        let doc_key = doc_url.to_string();

        // The document, or the embedded resource, and its base.
        let (root, resource_base) = match self.ids.get(&doc_key) {
            Some((url, pointer)) => (self.docs.get(url)?.pointer(pointer)?, doc_url.clone()),
            None => (&**self.docs.get(&doc_key)?, doc_url.clone()),
        };
        if fragment.is_empty() {
            return Some(SchemaRef { value: root, base: resource_base });
        }
        if fragment.starts_with('/') {
            let pointer = percent_decode(&fragment);
            let value = root.pointer(&pointer)?;
            // Resources between the root and the target change the base.
            let base = base_along(&resource_base, root, &pointer);
            return Some(SchemaRef { value, base });
        }
        let (url, pointer) = self.anchors.get(target.as_str())?;
        let doc = self.docs.get(url)?;
        let value = doc.pointer(pointer)?;
        let base = Url::parse(url).ok().map_or(resource_base, |u| base_along(&u, doc, pointer));
        Some(SchemaRef { value, base })
    }

    /// The documents referenced (transitively) from the document of `url`
    /// that are not loaded yet.
    pub fn missing(&self, url: &str) -> Vec<String> {
        let mut missing = Vec::new();
        let mut seen = FxHashSet::default();
        let mut queue = vec![normalize_url(url)];
        while let Some(url) = queue.pop() {
            if !seen.insert(url.clone()) {
                continue;
            }
            let Some(doc) = self.docs.get(&url) else {
                if !self.ids.contains_key(&url) {
                    missing.push(url);
                }
                continue;
            };
            let base = Url::parse(&url).unwrap_or_else(|_| fallback_base());
            let mut refs = Vec::new();
            collect_refs(doc, &base, &mut refs);
            for r in refs {
                let key = normalize_url(&r);
                match self.ids.get(&key) {
                    Some((doc_url, _)) => queue.push(doc_url.clone()),
                    None => queue.push(key),
                }
            }
        }
        missing.sort();
        missing.dedup();
        missing
    }
}

impl<'r> SchemaRef<'r> {
    /// A subschema of this schema, with its own base when it has an `$id`.
    pub fn child(&self, value: &'r Value) -> SchemaRef<'r> {
        let mut base = self.base.clone();
        if let Some(id) = value.get("$id").and_then(Value::as_str)
            && !id.starts_with('#')
            && let Ok(abs) = base.join(id)
        {
            base = abs;
        }
        SchemaRef { value, base }
    }

    /// A keyword of the schema.
    pub fn get(&self, keyword: &str) -> Option<&'r Value> {
        self.value.get(keyword)
    }

    /// The `description` or `markdownDescription` of the schema.
    pub fn description(&self) -> Option<&'r str> {
        self.get("markdownDescription").or_else(|| self.get("description")).and_then(Value::as_str)
    }

    /// The `title` of the schema.
    pub fn title(&self) -> Option<&'r str> {
        self.get("title").and_then(Value::as_str)
    }
}

/// The base URL for relative `$ref`s of a document that has no URL.
fn fallback_base() -> Url {
    Url::parse("toml-analyzer:///schema.json").expect("valid url")
}

/// The base of the value at `pointer` below `root`, following `$id`s.
fn base_along(base: &Url, root: &Value, pointer: &str) -> Url {
    let mut base = base.clone();
    let mut value = root;
    for part in pointer.split('/').skip(1) {
        let part = part.replace("~1", "/").replace("~0", "~");
        let next = match value {
            Value::Object(m) => m.get(&part),
            Value::Array(a) => part.parse::<usize>().ok().and_then(|i| a.get(i)),
            _ => None,
        };
        let Some(next) = next else { break };
        value = next;
        if let Some(id) = value.get("$id").and_then(Value::as_str)
            && !id.starts_with('#')
            && let Ok(abs) = base.join(id)
        {
            base = abs;
        }
    }
    base
}

/// The absolute targets of every `$ref` below `value`.
fn collect_refs(value: &Value, base: &Url, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            let mut base = base.clone();
            if let Some(id) = map.get("$id").and_then(Value::as_str)
                && !id.starts_with('#')
                && let Ok(abs) = base.join(id)
            {
                base = abs;
            }
            for key in ["$ref", "$dynamicRef", "$recursiveRef"] {
                if let Some(r) = map.get(key).and_then(Value::as_str)
                    && let Ok(abs) = base.join(r)
                {
                    out.push(abs.to_string());
                }
            }
            for (key, child) in map {
                if !DATA_KEYWORDS.contains(&key.as_str()) {
                    collect_refs(child, &base, out);
                }
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_refs(child, base, out);
            }
        }
        _ => {}
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_owned())
}
