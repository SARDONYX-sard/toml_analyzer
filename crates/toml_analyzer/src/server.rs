//! The server state and main loop.
//!
//! Requests are answered on the main thread: parsing and validating a TOML
//! document takes microseconds to a few milliseconds. Only schema fetching
//! happens in the background (see [`crate::schemas`]); when a schema
//! arrives, the diagnostics of every open document are published again.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::Arc,
};

use crossbeam_channel::{Sender, select};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::{
    ClientCapabilities, InitializeParams, PublishDiagnosticsParams, Uri, notification, request,
};
use rustc_hash::FxHashMap;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use toml_ide::{Document, Schema, SchemaSource, SchemaState};

use crate::{
    config::{CliOptions, Config},
    conv,
    crates::Crates,
    schemas::{CatalogMatch, FetchSettings, Schemas, Status, default_cache_dir, glob_set},
};

/// An open document.
pub(crate) struct Doc {
    pub(crate) uri: Uri,
    pub(crate) path: Option<PathBuf>,
    pub(crate) doc: Document,
    pub(crate) version: i32,
}

/// The schema chosen for a document.
pub(crate) struct Selection {
    /// The schema URL, `None` without a schema or on error.
    pub(crate) url: Option<String>,
    pub(crate) source: &'static str,
    /// Why the location of the document cannot be resolved.
    pub(crate) error: Option<String>,
    /// The association of `toml.schemas` that chose the schema: its URL as
    /// written and the pattern matching the document.
    pub(crate) matched: Option<(String, String)>,
}

/// A schema association, resolved.
struct ResolvedAssociation {
    /// `None` for `"none"`: the files have no schema.
    url: Option<String>,
    /// The URL as written in the settings.
    written: String,
    globs: globset::GlobSet,
    /// The patterns of `globs`, as written.
    patterns: Vec<String>,
    source: &'static str,
}

/// The language server.
pub(crate) struct Server {
    sender: Sender<Message>,
    pub(crate) docs: FxHashMap<String, Doc>,
    pub(crate) config: Config,
    cli: CliOptions,
    roots: Vec<PathBuf>,
    pub(crate) caps: ClientCapabilities,
    pub(crate) schemas: Schemas,
    /// The associations of the settings, then of the extensions, resolved.
    associations: Vec<ResolvedAssociation>,
    /// The crates.io registry, for `Cargo.toml`.
    pub(crate) crates: Crates,
    /// Completions waiting for the registry.
    pending: Vec<(RequestId, lsp_types::CompletionParams)>,
}

type HandlerResult = Result<Value, (ErrorCode, String)>;

impl Server {
    pub(crate) fn new(sender: Sender<Message>, init: &InitializeParams, cli: CliOptions) -> Server {
        let mut roots: Vec<PathBuf> = init
            .workspace_folders
            .iter()
            .flatten()
            .filter_map(|f| conv::uri_to_path(&f.uri))
            .collect();
        #[allow(deprecated)]
        if roots.is_empty() {
            roots.extend(init.root_uri.as_ref().and_then(conv::uri_to_path));
        }
        let config = Config::from_json(init.initialization_options.clone()).with_cli(&cli);
        let schemas = Schemas::new(fetch_settings(&config));
        let mut server = Server {
            sender,
            docs: FxHashMap::default(),
            config,
            cli,
            roots,
            caps: init.capabilities.clone(),
            schemas,
            associations: Vec::new(),
            crates: Crates::new(),
            pending: Vec::new(),
        };
        server.resolve_associations();
        server
    }

    /// Runs until the client asks for shutdown.
    pub(crate) fn run(mut self, connection: &Connection) -> anyhow::Result<()> {
        loop {
            select! {
                recv(connection.receiver) -> msg => {
                    let Ok(msg) = msg else { return Ok(()) };
                    match msg {
                        Message::Request(req) => {
                            if connection.handle_shutdown(&req)? {
                                return Ok(());
                            }
                            self.on_request(req);
                        }
                        Message::Notification(not) => self.on_notification(not),
                        Message::Response(_) => {}
                    }
                }
                recv(self.schemas.rx) -> fetched => {
                    if let Ok(fetched) = fetched
                        && self.schemas.on_fetched(fetched)
                    {
                        self.publish_all();
                    }
                }
                recv(self.crates.rx) -> fetched => {
                    if let Ok(fetched) = fetched {
                        self.crates.on_fetched(fetched);
                        self.retry_pending();
                    }
                }
            }
        }
    }

    // === Deferred completions ===

    fn completion_request(&mut self, id: RequestId, params: Value) {
        let params: lsp_types::CompletionParams = match serde_json::from_value(params) {
            Ok(p) => p,
            Err(e) => {
                self.respond(id, Err((ErrorCode::InvalidParams, format!("invalid params: {e}"))));
                return;
            }
        };
        match self.try_completion(&params) {
            Some(response) => self.respond_value(id, &response),
            None => self.pending.push((id, params)),
        }
    }

    /// Answers the completions whose registry answer arrived.
    fn retry_pending(&mut self) {
        for (id, params) in std::mem::take(&mut self.pending) {
            match self.try_completion(&params) {
                Some(response) => self.respond_value(id, &response),
                None => self.pending.push((id, params)),
            }
        }
    }

    fn respond_value(&self, id: RequestId, value: &impl Serialize) {
        let result =
            serde_json::to_value(value).map_err(|e| (ErrorCode::InternalError, e.to_string()));
        self.respond(id, result);
    }

    // === Messaging ===

    pub(crate) fn send(&self, msg: impl Into<Message>) {
        if self.sender.send(msg.into()).is_err() {
            log::error!("client connection closed");
        }
    }

    pub(crate) fn notify<N: notification::Notification>(&self, params: N::Params) {
        self.send(Notification::new(N::METHOD.to_owned(), params));
    }

    // === Requests ===

    fn on_request(&mut self, req: Request) {
        let Request { id, method, params } = req;
        if method == <request::Completion as request::Request>::METHOD {
            self.completion_request(id, params);
            return;
        }
        let result = catch_unwind(AssertUnwindSafe(|| self.dispatch(&method, params)))
            .unwrap_or_else(|panic| {
                let msg = panic
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| panic.downcast_ref::<&str>().copied())
                    .unwrap_or("unknown panic");
                log::error!("request `{method}` panicked: {msg}");
                Err((ErrorCode::InternalError, format!("internal error: {msg}")))
            });
        self.respond(id, result);
    }

    fn respond(&self, id: RequestId, result: HandlerResult) {
        let response = match result {
            Ok(value) => Response::new_ok(id, value),
            Err((code, message)) => Response::new_err(id, code as i32, message),
        };
        self.send(response);
    }

    fn dispatch(&mut self, method: &str, params: Value) -> HandlerResult {
        use lsp_types::request::*;
        match method {
            HoverRequest::METHOD => handle::<HoverRequest>(self, params, Server::hover),
            Formatting::METHOD => handle::<Formatting>(self, params, Server::formatting),
            DocumentSymbolRequest::METHOD => {
                handle::<DocumentSymbolRequest>(self, params, Server::document_symbols)
            }
            FoldingRangeRequest::METHOD => {
                handle::<FoldingRangeRequest>(self, params, Server::folding_ranges)
            }
            SemanticTokensFullRequest::METHOD => {
                handle::<SemanticTokensFullRequest>(self, params, Server::semantic_tokens_full)
            }
            SemanticTokensRangeRequest::METHOD => {
                handle::<SemanticTokensRangeRequest>(self, params, Server::semantic_tokens_range)
            }
            CodeActionRequest::METHOD => {
                handle::<CodeActionRequest>(self, params, Server::code_actions)
            }
            DocumentLinkRequest::METHOD => {
                handle::<DocumentLinkRequest>(self, params, Server::document_links)
            }
            GotoDefinition::METHOD => {
                handle::<GotoDefinition>(self, params, Server::goto_definition)
            }
            crate::ext::ClearSchemaCache::METHOD => {
                handle::<crate::ext::ClearSchemaCache>(self, params, Server::clear_schema_cache)
            }
            crate::ext::DocumentSchema::METHOD => {
                handle::<crate::ext::DocumentSchema>(self, params, Server::document_schema)
            }
            crate::ext::SchemaCatalog::METHOD => {
                handle::<crate::ext::SchemaCatalog>(self, params, Server::schema_catalog)
            }
            crate::ext::Status::METHOD => {
                handle::<crate::ext::Status>(self, params, Server::status)
            }
            _ => Err((ErrorCode::MethodNotFound, format!("unknown request `{method}`"))),
        }
    }

    // === Notifications ===

    fn on_notification(&mut self, not: Notification) {
        use lsp_types::notification::*;
        let method = not.method.clone();
        let result = catch_unwind(AssertUnwindSafe(|| match not.method.as_str() {
            DidOpenTextDocument::METHOD => notify::<DidOpenTextDocument>(not, |p| self.did_open(p)),
            Cancel::METHOD => notify::<Cancel>(not, |p| {
                let id: RequestId = match p.id {
                    lsp_types::NumberOrString::Number(n) => n.into(),
                    lsp_types::NumberOrString::String(s) => s.into(),
                };
                if let Some(i) = self.pending.iter().position(|(p, _)| *p == id) {
                    self.pending.remove(i);
                    self.respond(id, Err((ErrorCode::RequestCanceled, "canceled".to_owned())));
                }
            }),
            DidChangeTextDocument::METHOD => {
                notify::<DidChangeTextDocument>(not, |p| self.did_change(p))
            }
            DidCloseTextDocument::METHOD => {
                notify::<DidCloseTextDocument>(not, |p| self.did_close(p))
            }
            DidSaveTextDocument::METHOD => {
                notify::<DidSaveTextDocument>(not, |_| self.refresh_schema_files())
            }
            DidChangeWatchedFiles::METHOD => {
                notify::<DidChangeWatchedFiles>(not, |p| self.did_change_watched_files(p));
            }
            DidChangeConfiguration::METHOD => notify::<DidChangeConfiguration>(not, |p| {
                let config = Config::from_json(Some(p.settings)).with_cli(&self.cli);
                self.set_config(config);
            }),
            _ => {}
        }));
        if result.is_err() {
            log::error!("notification `{method}` panicked");
        }
    }

    fn did_open(&mut self, params: lsp_types::DidOpenTextDocumentParams) {
        let item = params.text_document;
        let key = item.uri.as_str().to_owned();
        let path = conv::uri_to_path(&item.uri);
        let doc = Doc { uri: item.uri, path, doc: Document::new(item.text), version: item.version };
        self.docs.insert(key.clone(), doc);
        if self.schemas.refresh_files() {
            self.publish_all();
        } else {
            self.publish(&key);
        }
    }

    fn did_change(&mut self, params: lsp_types::DidChangeTextDocumentParams) {
        let key = params.text_document.uri.as_str().to_owned();
        let Some(doc) = self.docs.get_mut(&key) else { return };
        let mut text = doc.doc.text.to_string();
        apply_changes(&mut text, params.content_changes);
        doc.doc = Document::new(text);
        doc.version = params.text_document.version;
        // The schema file may have been edited in the meantime.
        if self.schemas.refresh_files() {
            self.publish_all();
        } else {
            self.publish(&key);
        }
    }

    fn did_close(&mut self, params: lsp_types::DidCloseTextDocumentParams) {
        self.docs.remove(params.text_document.uri.as_str());
        self.notify::<notification::PublishDiagnostics>(PublishDiagnosticsParams {
            uri: params.text_document.uri,
            diagnostics: Vec::new(),
            version: None,
        });
    }

    fn did_change_watched_files(&mut self, params: lsp_types::DidChangeWatchedFilesParams) {
        let mut changed = false;
        for change in params.changes {
            if let Some(path) = conv::uri_to_path(&change.uri) {
                changed |= self.schemas.invalidate_path(&path);
            }
        }
        changed |= self.schemas.refresh_files();
        if changed {
            self.publish_all();
        }
    }

    fn refresh_schema_files(&mut self) {
        if self.schemas.refresh_files() {
            self.publish_all();
        }
    }

    fn set_config(&mut self, config: Config) {
        if config == self.config {
            return;
        }
        self.config = config;
        self.schemas.set_settings(fetch_settings(&self.config));
        self.resolve_associations();
        self.publish_all();
    }

    /// Resolves the URLs of the associations: relative paths of the
    /// settings are relative to the first workspace folder.
    fn resolve_associations(&mut self) {
        let root_dir = self.roots.first().cloned();
        let root = root_dir.as_ref().and_then(|r| url::Url::from_directory_path(r).ok());
        let mut out = Vec::new();
        for (list, source) in
            [(&self.config.schemas, "settings"), (&self.config.contributed, "extension")]
        {
            for a in list {
                let Some((globs, patterns)) = glob_set(&a.file_match, root_dir.as_deref()) else {
                    continue;
                };
                // `"none"`: no schema for these files.
                let url = if a.url.trim().eq_ignore_ascii_case("none") {
                    None
                } else {
                    match toml_ide::resolve_location(&a.url, root.as_ref()) {
                        Ok(url) => Some(url.to_string()),
                        Err(e) => {
                            log::warn!("schema association: {e}");
                            continue;
                        }
                    }
                };
                out.push(ResolvedAssociation {
                    url,
                    written: a.url.clone(),
                    globs,
                    patterns,
                    source,
                });
            }
        }
        self.associations = out;
    }

    // === Schemas ===

    /// The schema of the document `key`.
    pub(crate) fn select(&mut self, key: &str) -> Selection {
        let Some(doc) = self.docs.get(key) else {
            return Selection { url: None, source: "", error: None, matched: None };
        };
        if let Some(location) = toml_ide::schema_location(&doc.doc) {
            let source = match location.source {
                SchemaSource::Directive => "directive",
                SchemaSource::Key => "key",
            };
            let base = url::Url::parse(doc.uri.as_str()).ok().filter(|u| !u.cannot_be_a_base());
            return match toml_ide::resolve_location(&location.text, base.as_ref()) {
                Ok(url) => {
                    Selection { url: Some(url.to_string()), source, error: None, matched: None }
                }
                Err(e) => Selection { url: None, source, error: Some(e), matched: None },
            };
        }
        let Some(path) = doc.path.clone() else {
            return Selection { url: None, source: "", error: None, matched: None };
        };
        let slashed = crate::schemas::slash_path(&path);
        for a in &self.associations {
            if let Some(&i) = a.globs.matches(&slashed).first() {
                return Selection {
                    url: a.url.clone(),
                    source: a.source,
                    error: None,
                    matched: Some((a.written.clone(), a.patterns[i].clone())),
                };
            }
        }
        if crate::clippy::is_clippy_config(&path)
            && let Some(dir) = path.parent()
        {
            return Selection {
                url: Some(crate::clippy::schema_url(dir)),
                source: "builtin",
                error: None,
                matched: None,
            };
        }
        if self.config.schema_store
            && let CatalogMatch::Found(url) = self.schemas.catalog_match(&path)
        {
            return Selection { url: Some(url), source: "catalog", error: None, matched: None };
        }
        Selection { url: None, source: "", error: None, matched: None }
    }

    /// The schema of `key` with its status.
    pub(crate) fn schema_status(&mut self, key: &str) -> (Selection, Option<Status>) {
        let selection = self.select(key);
        let status = match (&selection.url, &selection.error) {
            (_, Some(e)) => Some(Status::Failed(e.clone())),
            (Some(url), None) => Some(self.schemas.ensure(url)),
            (None, None) => None,
        };
        (selection, status)
    }

    // === Diagnostics ===

    pub(crate) fn publish_all(&mut self) {
        let keys: Vec<String> = self.docs.keys().cloned().collect();
        for key in keys {
            self.publish(&key);
        }
    }

    /// The problems of the document `key`, with its schema.
    pub(crate) fn ide_diagnostics(&mut self, key: &str) -> Vec<toml_ide::Diagnostic> {
        let (selection, status) =
            if self.config.validate { self.schema_status(key) } else { (self.select(key), None) };
        let Some(doc) = self.docs.get(key) else { return Vec::new() };
        let failure;
        let state = match (&status, &selection.url) {
            (Some(Status::Ready), Some(url)) => {
                SchemaState::Ready(Schema { registry: &self.schemas.registry, url })
            }
            (Some(Status::Loading), _) => SchemaState::Loading,
            (Some(Status::Failed(e)), _) => {
                failure = format!("cannot load the schema: {e}");
                SchemaState::Failed(&failure)
            }
            _ => SchemaState::None,
        };
        toml_ide::diagnostics(&doc.doc, state)
    }

    fn publish(&mut self, key: &str) {
        let found = self.ide_diagnostics(key);
        let Some(doc) = self.docs.get(key) else { return };
        let diagnostics = found.into_iter().map(|d| to_lsp_diagnostic(doc, d)).collect();
        self.notify::<notification::PublishDiagnostics>(PublishDiagnosticsParams {
            uri: doc.uri.clone(),
            diagnostics,
            version: Some(doc.version),
        });
    }

    // === Extensions ===

    fn clear_schema_cache(&mut self, _: ()) -> crate::ext::ClearSchemaCacheResult {
        let removed_files = self.schemas.clear();
        self.crates.clear();
        // Fetches the schemas of the open documents again.
        self.publish_all();
        crate::ext::ClearSchemaCacheResult { removed_files }
    }

    fn document_schema(
        &mut self,
        params: lsp_types::TextDocumentIdentifier,
    ) -> crate::ext::DocumentSchemaResult {
        let (selection, status) = self.schema_status(params.uri.as_str());
        let (association_url, pattern) = selection.matched.clone().unzip();
        crate::ext::DocumentSchemaResult {
            url: selection.url,
            source: (!selection.source.is_empty()).then(|| selection.source.to_owned()),
            association_url,
            pattern,
            status: match status {
                None => "none".to_owned(),
                Some(Status::Ready) => "ready".to_owned(),
                Some(Status::Loading) => "loading".to_owned(),
                Some(Status::Failed(e)) => format!("failed: {e}"),
            },
        }
    }

    fn schema_catalog(&mut self, _: ()) -> crate::ext::SchemaCatalogResult {
        let Some(list) = self.schemas.catalog_list() else {
            return crate::ext::SchemaCatalogResult { loading: true, schemas: Vec::new() };
        };
        let schemas = list
            .into_iter()
            .map(|(e, toml)| crate::ext::CatalogSchema {
                name: e.name.clone(),
                url: e.url.clone(),
                description: e.description.clone(),
                toml,
            })
            .collect();
        crate::ext::SchemaCatalogResult { loading: false, schemas }
    }

    fn status(&mut self, _: ()) -> crate::ext::StatusResult {
        crate::ext::StatusResult {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            cache_directory: fetch_settings(&self.config)
                .cache_dir
                .map(|d| d.display().to_string()),
            documents: self.docs.keys().cloned().collect(),
            schemas: self.schemas.report(),
        }
    }

    /// The open document of `uri`.
    pub(crate) fn doc(&self, uri: &Uri) -> Result<&Doc, (ErrorCode, String)> {
        self.docs.get(uri.as_str()).ok_or_else(|| {
            (ErrorCode::InvalidParams, format!("unknown document: {}", uri.as_str()))
        })
    }
}

/// How schemas are fetched with `config`.
fn fetch_settings(config: &Config) -> FetchSettings {
    FetchSettings {
        cache_dir: config
            .disk_cache
            .then(|| config.cache_dir.clone().or_else(default_cache_dir))
            .flatten(),
        max_age: config.cache_max_age,
        catalog_url: config.catalog_url.clone(),
    }
}

fn handle<R>(
    server: &mut Server,
    params: Value,
    f: impl FnOnce(&mut Server, R::Params) -> R::Result,
) -> HandlerResult
where
    R: request::Request,
    R::Params: DeserializeOwned,
    R::Result: Serialize,
{
    let params: R::Params = serde_json::from_value(params)
        .map_err(|e| (ErrorCode::InvalidParams, format!("invalid params: {e}")))?;
    serde_json::to_value(f(server, params)).map_err(|e| (ErrorCode::InternalError, e.to_string()))
}

fn notify<N>(not: Notification, f: impl FnOnce(N::Params))
where
    N: notification::Notification,
    N::Params: DeserializeOwned,
{
    match serde_json::from_value(not.params) {
        Ok(params) => f(params),
        Err(err) => log::warn!("invalid `{}` params: {err}", N::METHOD),
    }
}

/// Applies editor changes to `text`.
fn apply_changes(text: &mut String, changes: Vec<lsp_types::TextDocumentContentChangeEvent>) {
    for change in changes {
        match change.range {
            Some(range) => {
                // Positions refer to the text after the previous change.
                let index = Arc::new(toml_ide::LineIndex::new(text));
                let start = index.offset(
                    text,
                    toml_ide::LineCol { line: range.start.line, col: range.start.character },
                );
                let end = index.offset(
                    text,
                    toml_ide::LineCol { line: range.end.line, col: range.end.character },
                );
                text.replace_range(usize::from(start)..usize::from(end.max(start)), &change.text);
            }
            None => *text = change.text,
        }
    }
}

pub(crate) fn to_lsp_diagnostic(doc: &Doc, d: toml_ide::Diagnostic) -> lsp_types::Diagnostic {
    lsp_types::Diagnostic {
        range: conv::range(&doc.doc, d.range),
        severity: Some(match d.severity {
            toml_ide::Severity::Error => lsp_types::DiagnosticSeverity::ERROR,
            toml_ide::Severity::Warning => lsp_types::DiagnosticSeverity::WARNING,
            toml_ide::Severity::Information => lsp_types::DiagnosticSeverity::INFORMATION,
            toml_ide::Severity::Hint => lsp_types::DiagnosticSeverity::HINT,
        }),
        code: Some(lsp_types::NumberOrString::String(d.code.to_owned())),
        source: Some("toml".to_owned()),
        tags: d.deprecated.then(|| vec![lsp_types::DiagnosticTag::DEPRECATED]),
        message: d.message,
        related_information: (!d.related.is_empty()).then(|| {
            d.related
                .iter()
                .map(|(range, message)| lsp_types::DiagnosticRelatedInformation {
                    location: lsp_types::Location {
                        uri: doc.uri.clone(),
                        range: conv::range(&doc.doc, *range),
                    },
                    message: message.clone(),
                })
                .collect()
        }),
        ..Default::default()
    }
}
