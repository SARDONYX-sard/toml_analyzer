//! Problems of a document: syntax, TOML semantics and schema.

use toml_parser::{TextRange, TextSize};

use crate::{
    Document, Schema, SchemaLocation,
    fixes::{self, Fix},
    schema_location,
};

/// How bad a problem is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// An error.
    Error,
    /// A warning.
    Warning,
    /// Information.
    Information,
    /// Barely shown: a strike-through, no entry in the problem list.
    Hint,
}

/// A problem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// Where.
    pub range: TextRange,
    /// How bad.
    pub severity: Severity,
    /// `syntax`, `toml` or `schema`.
    pub code: &'static str,
    /// What.
    pub message: String,
    /// Related places, e.g. the first definition of a duplicate.
    pub related: Vec<(TextRange, String)>,
    /// Quick fixes.
    pub fixes: Vec<Fix>,
    /// About something deprecated: editors strike it through.
    pub deprecated: bool,
}

/// The schema of the document, as far as the caller got.
#[derive(Clone, Copy)]
pub enum SchemaState<'r> {
    /// The document has no schema.
    None,
    /// The schema (or a document it references) is being fetched.
    Loading,
    /// The schema could not be loaded.
    Failed(&'r str),
    /// The schema is ready.
    Ready(Schema<'r>),
}

/// The problems of `doc`.
pub fn diagnostics(doc: &Document, schema: SchemaState<'_>) -> Vec<Diagnostic> {
    let mut out: Vec<Diagnostic> = doc
        .errors
        .iter()
        .map(|e| Diagnostic {
            range: e.range(),
            severity: Severity::Error,
            code: "syntax",
            message: e.message().to_owned(),
            related: Vec::new(),
            fixes: fixes::syntax_fixes(doc, e),
            deprecated: false,
        })
        .collect();
    out.extend(doc.dom.errors.iter().map(|e| Diagnostic {
        range: e.range,
        severity: Severity::Error,
        code: "toml",
        message: e.message.clone(),
        related: e.related.iter().cloned().collect(),
        fixes: if e.removable { fixes::remove_duplicate(doc, e.range) } else { Vec::new() },
        deprecated: false,
    }));

    let location = schema_location(doc);
    let anchor = location.as_ref().map_or_else(|| first_line(doc), |l| l.range);
    match schema {
        SchemaState::None | SchemaState::Loading => {}
        SchemaState::Failed(reason) => out.push(Diagnostic {
            range: anchor,
            severity: if location.is_some() { Severity::Error } else { Severity::Warning },
            code: "schema",
            message: reason.to_owned(),
            related: Vec::new(),
            fixes: Vec::new(),
            deprecated: false,
        }),
        SchemaState::Ready(schema) => {
            out.extend(schema_diagnostics(doc, schema, location.as_ref(), anchor))
        }
    }
    out.sort_by_key(|d| (d.range.start(), d.range.end()));
    out
}

fn schema_diagnostics(
    doc: &Document,
    schema: Schema<'_>,
    location: Option<&SchemaLocation>,
    anchor: TextRange,
) -> Vec<Diagnostic> {
    let Some(root) = schema.root() else { return Vec::new() };
    // Only a `$schema` key that names the schema is hidden from it.
    let ignored: &[&str] = match location {
        Some(l) if l.source == crate::SchemaSource::Key => &["$schema"],
        _ => &[],
    };
    toml_schema::validate(schema.registry, &root, &doc.dom.root, ignored)
        .into_iter()
        .map(|e| {
            let range = if e.path.is_empty() {
                anchor
            } else {
                doc.locate(&e.path, e.target == toml_schema::Target::Key)
            };
            Diagnostic {
                range,
                severity: match e.severity {
                    toml_schema::Severity::Error => Severity::Error,
                    // Deprecated without a replacement, e.g. `authors` of
                    // Cargo: still valid, nothing to do but strike it through.
                    toml_schema::Severity::Warning
                        if e.deprecated
                            && !e
                                .suggestions
                                .iter()
                                .any(|s| matches!(s, toml_schema::Suggestion::RenameKey(_))) =>
                    {
                        Severity::Hint
                    }
                    toml_schema::Severity::Warning => Severity::Warning,
                },
                code: "schema",
                fixes: fixes::schema_fixes(doc, &e),
                deprecated: e.deprecated,
                message: e.message,
                related: Vec::new(),
            }
        })
        .collect()
}

/// The first line, where problems of the whole document go.
fn first_line(doc: &Document) -> TextRange {
    let end = doc.text.find(['\r', '\n']).unwrap_or(doc.text.len());
    TextRange::new(TextSize::new(0), TextSize::new(end as u32))
}
