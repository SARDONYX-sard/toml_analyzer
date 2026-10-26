//! Protocol extensions, see `extension/src/protocol.ts`.

use lsp_types::{TextDocumentIdentifier, request::Request};
use serde::{Deserialize, Serialize};

/// `toml/clearSchemaCache`: forgets every schema, in memory and on disk,
/// then fetches the schemas of the open documents again.
pub(crate) enum ClearSchemaCache {}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClearSchemaCacheResult {
    /// The cache files removed.
    pub(crate) removed_files: usize,
}

impl Request for ClearSchemaCache {
    type Params = ();
    type Result = ClearSchemaCacheResult;
    const METHOD: &'static str = "toml/clearSchemaCache";
}

/// `toml/documentSchema`: the schema of a document, for the status bar.
pub(crate) enum DocumentSchema {}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DocumentSchemaResult {
    /// The schema URL.
    pub(crate) url: Option<String>,
    /// `directive`, `key`, `settings`, `extension` or `catalog`.
    pub(crate) source: Option<String>,
    /// `ready`, `loading` or `failed: ...`.
    pub(crate) status: String,
    /// For `settings`: the URL of the `toml.schemas` association, as
    /// written (`none` for no schema), and its pattern matching the file.
    pub(crate) association_url: Option<String>,
    pub(crate) pattern: Option<String>,
}

/// `toml/schemaCatalog`: the schemas of the SchemaStore catalog, to choose
/// one. `loading` while the catalog is fetched: ask again.
pub(crate) enum SchemaCatalog {}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SchemaCatalogResult {
    pub(crate) loading: bool,
    pub(crate) schemas: Vec<CatalogSchema>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CatalogSchema {
    pub(crate) name: String,
    pub(crate) url: String,
    pub(crate) description: Option<String>,
    /// The schema applies to some TOML files.
    pub(crate) toml: bool,
}

impl Request for SchemaCatalog {
    type Params = ();
    type Result = SchemaCatalogResult;
    const METHOD: &'static str = "toml/schemaCatalog";
}

impl Request for DocumentSchema {
    type Params = TextDocumentIdentifier;
    type Result = DocumentSchemaResult;
    const METHOD: &'static str = "toml/documentSchema";
}

/// `toml/status`: what the server knows, for `TOML: Show Status`.
pub(crate) enum Status {}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatusResult {
    pub(crate) version: String,
    pub(crate) cache_directory: Option<String>,
    pub(crate) documents: Vec<String>,
    /// URL and state of every schema document.
    pub(crate) schemas: Vec<(String, String)>,
}

impl Request for Status {
    type Params = ();
    type Result = StatusResult;
    const METHOD: &'static str = "toml/status";
}
