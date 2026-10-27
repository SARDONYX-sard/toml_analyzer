//! Request handlers.

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionItemTag, CompletionParams, CompletionResponse,
    DocumentLink, DocumentLinkParams, DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse,
    FoldingRange, FoldingRangeKind, FoldingRangeParams, GotoDefinitionParams,
    GotoDefinitionResponse, Hover, HoverContents, HoverParams, InsertTextFormat, MarkupContent,
    MarkupKind, SemanticToken, SemanticTokens, SemanticTokensParams, SemanticTokensRangeParams,
    SemanticTokensRangeResult, SemanticTokensResult, TextEdit, Uri,
};
use toml_ide::{Document, LineCol, Schema, TextRange};

use crate::{conv, crates::Answer, schemas::Status, server::Server};

impl Server {
    /// The schema of `uri` when it is ready.
    fn ready_schema(&mut self, uri: &Uri) -> Option<String> {
        if !self.config.validate {
            return None;
        }
        match self.schema_status(uri.as_str()) {
            (selection, Some(Status::Ready)) => selection.url,
            _ => None,
        }
    }

    pub(crate) fn hover(&mut self, params: HoverParams) -> Option<Hover> {
        let p = params.text_document_position_params;
        let url = self.ready_schema(&p.text_document.uri);
        let cargo =
            self.config.cargo && self.doc(&p.text_document.uri).is_ok_and(is_cargo_manifest);
        let doc = &self.doc(&p.text_document.uri).ok()?.doc;
        let offset = conv::offset(doc, p.position);
        let schema = url.as_deref().map(|url| Schema { registry: &self.schemas.registry, url });
        let from_schema = toml_ide::hover(doc, offset, schema);
        let mut dependency = cargo.then(|| toml_ide::cargo_hover_context(doc, offset)).flatten();
        let inherited = self.workspace_ref(&p.text_document.uri, offset);
        // `serde.workspace = true`: the version of the workspace.
        if let (Some(dep), Some((r, _, root))) = (&mut dependency, &inherited)
            && dep.requirement.is_none()
        {
            let version = toml_ide::PathSegment::Key("version".to_owned());
            let string = |path: &[toml_ide::PathSegment]| match root.dom.get(path) {
                Some(toml_dom_item) => toml_dom_item.to_json().as_str().map(str::to_owned),
                None => None,
            };
            let mut with_version = r.target.clone();
            with_version.push(version);
            dep.requirement = string(&r.target).or_else(|| string(&with_version));
        }
        let doc = &self.doc(&p.text_document.uri).ok()?.doc;
        let (markdown, range) = match (dependency, from_schema) {
            (Some(dep), Some(h)) => (
                format!("{}\n\n---\n\n{}", toml_ide::cargo_hover_markdown(&dep), h.markdown),
                dep.range,
            ),
            (Some(dep), None) => (toml_ide::cargo_hover_markdown(&dep), dep.range),
            (None, Some(h)) => (h.markdown, h.range),
            (None, None) => match &inherited {
                Some((r, _, _)) => (String::new(), r.range),
                None => return None,
            },
        };
        let (markdown, range) = match &inherited {
            Some((r, _, root)) => {
                let from = toml_ide::cargo_workspace_hover(r, root);
                let markdown =
                    if markdown.is_empty() { from } else { format!("{from}\n\n---\n\n{markdown}") };
                (markdown, r.range)
            }
            None => (markdown, range),
        };
        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: markdown,
            }),
            range: Some(conv::range(doc, range)),
        })
    }

    /// The completions of `params`, `None` while the crate registry is
    /// being asked: the request is answered when it replies.
    pub(crate) fn try_completion(
        &mut self,
        params: &CompletionParams,
    ) -> Option<CompletionResponse> {
        let p = &params.text_document_position;
        let uri = &p.text_document.uri;
        let cargo = self.config.cargo && self.doc(uri).is_ok_and(is_cargo_manifest);
        let context = if cargo {
            let doc = &self.doc(uri).ok()?.doc;
            toml_ide::cargo_context(doc, conv::offset(doc, p.position)).filter(|c| {
                // Registry versions: other extensions (crates, Dependi) complete
                // them too. Local versions come from the local manifest.
                self.config.cargo_versions
                    || c.local_path.is_some()
                    || !matches!(c.target, toml_ide::CargoTarget::Version { .. })
            })
        } else {
            None
        };
        let answer = match &context {
            Some(c) if c.local_path.is_some() => Some(self.local_crate_answer(uri, c)),
            Some(c) => Some(self.crates.get(&c.query)?),
            None => None,
        };

        let url = self.ready_schema(uri);
        let snippets = self
            .caps
            .text_document
            .as_ref()
            .and_then(|t| t.completion.as_ref())
            .and_then(|c| c.completion_item.as_ref())
            .and_then(|i| i.snippet_support)
            .unwrap_or(false);
        let Ok(doc) = self.doc(uri) else { return Some(CompletionResponse::Array(Vec::new())) };
        let doc = &doc.doc;
        let offset = conv::offset(doc, p.position);
        let mut items = Vec::new();
        let style = self.config.cargo_dependency_style;
        if let (Some(context), Some(answer)) = (&context, &answer) {
            items.extend(match answer {
                Answer::Search(found) => {
                    toml_ide::cargo_completions(context, Some(found), None, style, snippets)
                }
                Answer::Versions(versions) => {
                    toml_ide::cargo_completions(context, None, Some(versions), style, snippets)
                }
            });
        }
        if cargo {
            items.extend(toml_ide::cargo_feature_completions(doc, offset));
        }
        if let Some(url) = &url {
            let schema = Schema { registry: &self.schemas.registry, url };
            items.extend(toml_ide::completions(doc, offset, schema, snippets));
        }
        let items: Vec<CompletionItem> =
            items.into_iter().map(|i| completion_item(doc, i)).collect();
        // Crate names: the registry is searched again as the name grows.
        let is_incomplete =
            matches!(context, Some(c) if matches!(c.target, toml_ide::CargoTarget::Name { .. }));
        Some(CompletionResponse::List(lsp_types::CompletionList { is_incomplete, items }))
    }

    pub(crate) fn formatting(
        &mut self,
        params: lsp_types::DocumentFormattingParams,
    ) -> Option<Vec<TextEdit>> {
        let doc = &self.doc(&params.text_document.uri).ok()?.doc;
        let mut options = self.config.format.clone();
        options.indent = if params.options.insert_spaces {
            " ".repeat(params.options.tab_size.min(16) as usize)
        } else {
            "\t".to_owned()
        };
        let formatted = toml_fmt::format(&doc.text, &options)?;
        if formatted == *doc.text {
            return Some(Vec::new());
        }
        let all = TextRange::up_to(toml_ide::TextSize::of(&*doc.text));
        Some(vec![TextEdit { range: conv::range(doc, all), new_text: formatted }])
    }

    pub(crate) fn document_symbols(
        &mut self,
        params: DocumentSymbolParams,
    ) -> Option<DocumentSymbolResponse> {
        let doc = &self.doc(&params.text_document.uri).ok()?.doc;
        fn convert(doc: &Document, s: toml_ide::Symbol) -> DocumentSymbol {
            use lsp_types::SymbolKind as K;
            #[allow(deprecated)]
            DocumentSymbol {
                name: if s.name.is_empty() { "\"\"".to_owned() } else { s.name },
                detail: None,
                kind: match s.kind {
                    toml_ide::SymbolKind::Table => K::OBJECT,
                    toml_ide::SymbolKind::Array => K::ARRAY,
                    toml_ide::SymbolKind::String => K::STRING,
                    toml_ide::SymbolKind::Number => K::NUMBER,
                    toml_ide::SymbolKind::Boolean => K::BOOLEAN,
                    toml_ide::SymbolKind::DateTime => K::CONSTANT,
                    toml_ide::SymbolKind::Invalid => K::NULL,
                },
                tags: None,
                deprecated: None,
                range: conv::range(doc, s.range),
                selection_range: conv::range(doc, s.selection_range),
                children: (!s.children.is_empty())
                    .then(|| s.children.into_iter().map(|c| convert(doc, c)).collect()),
            }
        }
        let symbols =
            toml_ide::document_symbols(doc).into_iter().map(|s| convert(doc, s)).collect();
        Some(DocumentSymbolResponse::Nested(symbols))
    }

    pub(crate) fn folding_ranges(
        &mut self,
        params: FoldingRangeParams,
    ) -> Option<Vec<FoldingRange>> {
        let doc = &self.doc(&params.text_document.uri).ok()?.doc;
        let folds = toml_ide::folding_ranges(doc)
            .into_iter()
            .filter_map(|f| {
                let start = doc.line_index.line_col(&doc.text, f.range.start()).line;
                let end = doc.line_index.line_col(&doc.text, f.range.end()).line;
                (end > start).then_some(FoldingRange {
                    start_line: start,
                    end_line: end,
                    kind: Some(match f.kind {
                        toml_ide::FoldKind::Region => FoldingRangeKind::Region,
                        toml_ide::FoldKind::Comment => FoldingRangeKind::Comment,
                    }),
                    ..Default::default()
                })
            })
            .collect();
        Some(folds)
    }

    pub(crate) fn semantic_tokens_full(
        &mut self,
        params: SemanticTokensParams,
    ) -> Option<SemanticTokensResult> {
        let doc = &self.doc(&params.text_document.uri).ok()?.doc;
        Some(SemanticTokensResult::Tokens(encode_tokens(doc, None)))
    }

    pub(crate) fn semantic_tokens_range(
        &mut self,
        params: SemanticTokensRangeParams,
    ) -> Option<SemanticTokensRangeResult> {
        let doc = &self.doc(&params.text_document.uri).ok()?.doc;
        let range = conv::text_range(doc, params.range);
        Some(SemanticTokensRangeResult::Tokens(encode_tokens(doc, Some(range))))
    }

    /// The schema of `uri` as a link target.
    fn schema_target(&mut self, uri: &Uri) -> Option<(TextRange, url::Url)> {
        let doc = &self.doc(uri).ok()?.doc;
        let location = toml_ide::schema_location(doc)?;
        let base = url::Url::parse(uri.as_str()).ok().filter(|u| !u.cannot_be_a_base());
        let target = toml_ide::resolve_location(&location.text, base.as_ref()).ok()?;
        Some((location.range, target))
    }

    pub(crate) fn document_links(
        &mut self,
        params: DocumentLinkParams,
    ) -> Option<Vec<DocumentLink>> {
        let uri = &params.text_document.uri;
        let mut links = Vec::new();
        if let Some((range, target)) = self.schema_target(uri) {
            let doc = &self.doc(uri).ok()?.doc;
            links.push(DocumentLink {
                range: conv::range(doc, range),
                target: conv::url_to_uri(&target),
                tooltip: Some("Open the schema".to_owned()),
                data: None,
            });
        }
        let doc = self.doc(uri).ok()?;
        for s in toml_ide::path_strings(&doc.doc) {
            // The schema link covers its own string.
            if links.iter().any(|l| l.range == conv::range(&doc.doc, s.range)) {
                continue;
            }
            if let Some(target) = open_target(doc, &s.path) {
                links.push(DocumentLink {
                    range: conv::range(&doc.doc, s.range),
                    target: conv::path_to_uri(&target),
                    tooltip: Some(format!("Open {}", target.display())),
                    data: None,
                });
            }
        }
        Some(links)
    }

    pub(crate) fn goto_definition(
        &mut self,
        params: GotoDefinitionParams,
    ) -> Option<GotoDefinitionResponse> {
        let p = params.text_document_position_params;
        let uri = &p.text_document.uri;
        let location = |target: &std::path::Path| {
            Some(GotoDefinitionResponse::Scalar(lsp_types::Location {
                uri: conv::path_to_uri(target)?,
                range: lsp_types::Range::default(),
            }))
        };
        let offset = {
            let doc = &self.doc(uri).ok()?.doc;
            conv::offset(doc, p.position)
        };
        if let Some((range, target)) = self.schema_target(uri)
            && range.contains_inclusive(offset)
        {
            return match target.to_file_path() {
                Ok(path) => location(&path),
                Err(()) => None,
            };
        }
        // `authors.workspace = true`: the definition in the root manifest.
        if let Some((reference, path, root)) = self.workspace_ref(uri, offset) {
            let range = root.locate(&reference.target, true);
            return Some(GotoDefinitionResponse::Scalar(lsp_types::Location {
                uri: conv::path_to_uri(&path)?,
                range: conv::range(&root, range),
            }));
        }
        let doc = self.doc(uri).ok()?;
        // The key of a `path` dependency: its manifest.
        if self.config.cargo
            && is_cargo_manifest(doc)
            && let Some(dir) = toml_ide::cargo_path_dependency(&doc.doc, offset).as_deref()
            && let Some(target) = open_target(doc, dir)
        {
            return location(&target);
        }
        let s = toml_ide::path_strings(&doc.doc)
            .into_iter()
            .find(|s| s.range.contains_inclusive(offset))?;
        location(&open_target(doc, &s.path)?)
    }
}

/// The file a path string of `doc` opens: the file, or the `Cargo.toml`
/// of a directory. Relative paths are relative to the document.
fn open_target(doc: &crate::server::Doc, path: &str) -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(path);
    let full =
        if path.is_absolute() { path.to_owned() } else { doc.path.as_ref()?.parent()?.join(path) };
    if full.is_file() {
        return Some(full);
    }
    let manifest = full.join("Cargo.toml");
    manifest.is_file().then_some(manifest)
}

/// Encodes the semantic tokens, split at line breaks.
fn encode_tokens(doc: &Document, range: Option<TextRange>) -> SemanticTokens {
    let mut data = Vec::new();
    let (mut prev_line, mut prev_col) = (0u32, 0u32);
    let index = &doc.line_index;
    for token in toml_ide::semantic_tokens(doc, range) {
        let start = index.line_col(&doc.text, token.range.start());
        let end = index.line_col(&doc.text, token.range.end());
        for line in start.line..=end.line {
            let col = if line == start.line { start.col } else { 0 };
            let end_col = if line == end.line {
                end.col
            } else {
                // Up to the end of the line, line break excluded.
                let next = index.offset(&doc.text, LineCol { line: line + 1, col: 0 });
                let mut eol = usize::from(next);
                let bytes = doc.text.as_bytes();
                while eol > 0 && matches!(bytes[eol - 1], b'\n' | b'\r') {
                    eol -= 1;
                }
                index.line_col(&doc.text, toml_ide::TextSize::new(eol as u32)).col
            };
            if end_col <= col {
                continue;
            }
            let delta_line = line - prev_line;
            let delta_start = if delta_line == 0 { col - prev_col } else { col };
            data.push(SemanticToken {
                delta_line,
                delta_start,
                length: end_col - col,
                token_type: token.ty as u32,
                token_modifiers_bitset: 0,
            });
            prev_line = line;
            prev_col = col;
        }
    }
    SemanticTokens { result_id: None, data }
}

fn completion_item(doc: &Document, i: toml_ide::CompletionItem) -> CompletionItem {
    CompletionItem {
        label: i.label.clone(),
        kind: Some(match i.kind {
            toml_ide::CompletionKind::Key => CompletionItemKind::PROPERTY,
            toml_ide::CompletionKind::Table => CompletionItemKind::MODULE,
            toml_ide::CompletionKind::Value => CompletionItemKind::VALUE,
        }),
        detail: i.detail,
        documentation: i.documentation.map(|d| {
            lsp_types::Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: d,
            })
        }),
        sort_text: Some(i.sort),
        filter_text: Some(i.filter.unwrap_or(i.label)),
        insert_text_format: Some(if i.snippet {
            InsertTextFormat::SNIPPET
        } else {
            InsertTextFormat::PLAIN_TEXT
        }),
        text_edit: Some(lsp_types::CompletionTextEdit::Edit(TextEdit {
            range: conv::range(doc, i.range),
            new_text: i.insert,
        })),
        tags: i.deprecated.then(|| vec![CompletionItemTag::DEPRECATED]),
        ..Default::default()
    }
}

/// Whether the document is a `Cargo.toml`.
pub(crate) fn is_cargo_manifest(doc: &crate::server::Doc) -> bool {
    let name = doc.uri.as_str().rsplit('/').next().unwrap_or("");
    name.eq_ignore_ascii_case("Cargo.toml")
}

impl Server {
    pub(crate) fn code_actions(
        &mut self,
        params: lsp_types::CodeActionParams,
    ) -> Option<lsp_types::CodeActionResponse> {
        let uri = params.text_document.uri;
        let found = self.ide_diagnostics(uri.as_str());
        let doc = self.doc(&uri).ok()?;
        let range = conv::text_range(&doc.doc, params.range);
        let mut actions = Vec::new();
        for d in found {
            // Touching ranges count: the cursor at the end of a key.
            if d.fixes.is_empty() || d.range.start() > range.end() || d.range.end() < range.start()
            {
                continue;
            }
            let fixes = d.fixes.clone();
            let diagnostic = crate::server::to_lsp_diagnostic(doc, d);
            for fix in fixes {
                actions.push(fix_action(doc, fix, Some(diagnostic.clone())));
            }
        }
        // Last: no problem to fix, conveniences.
        let url = self.ready_schema(&uri);
        let cargo = self.config.cargo;
        let doc = self.doc(&uri).ok()?;
        let mut extra = Vec::new();
        if let Some(url) = &url {
            let schema = Schema { registry: &self.schemas.registry, url };
            extra.extend(toml_ide::sort_keys_fix(&doc.doc, range.start(), schema));
        }
        extra.extend(toml_ide::sort_alphabetically_fix(&doc.doc, range.start()));
        if cargo && is_cargo_manifest(doc) {
            let locals = doc
                .path
                .as_deref()
                .and_then(|p| self.workspace_root(p.parent()?))
                .and_then(|w| toml_ide::cargo_workspace_locals(&w))
                .unwrap_or_default();
            extra.extend(toml_ide::cargo_dependency_fixes(&doc.doc, range));
            extra.extend(toml_ide::cargo_sort_fixes(&doc.doc, range.start(), &locals));
        }
        for fix in extra {
            actions.push(fix_action(doc, fix, None));
        }
        Some(actions)
    }
}

/// A quick fix of `doc`, solving `diagnostic` if any.
fn fix_action(
    doc: &crate::server::Doc,
    fix: toml_ide::Fix,
    diagnostic: Option<lsp_types::Diagnostic>,
) -> lsp_types::CodeActionOrCommand {
    let edits = fix
        .edits
        .iter()
        .map(|(r, text)| TextEdit { range: conv::range(&doc.doc, *r), new_text: text.clone() })
        .collect();
    #[allow(clippy::mutable_key_type)]
    let changes = std::iter::once((doc.uri.clone(), edits)).collect();
    lsp_types::CodeActionOrCommand::CodeAction(lsp_types::CodeAction {
        title: fix.title,
        kind: Some(lsp_types::CodeActionKind::QUICKFIX),
        diagnostics: diagnostic.map(|d| vec![d]),
        edit: Some(lsp_types::WorkspaceEdit { changes: Some(changes), ..Default::default() }),
        is_preferred: Some(fix.preferred),
        ..Default::default()
    })
}

impl Server {
    /// The manifest at `path`: the editor text when it is open.
    pub(crate) fn read_manifest(&self, path: &std::path::Path) -> Option<Document> {
        let same = |p: &std::path::Path| {
            if cfg!(windows) {
                p.to_string_lossy().eq_ignore_ascii_case(&path.to_string_lossy())
            } else {
                p == path
            }
        };
        if let Some(open) = self.docs.values().find(|d| d.path.as_deref().is_some_and(same)) {
            return Some(open.doc.clone());
        }
        std::fs::read_to_string(path).ok().map(Document::new)
    }

    /// The version and features of the local dependency of `context`, from
    /// its `Cargo.toml`.
    fn local_crate_answer(&self, uri: &Uri, context: &toml_ide::CargoContext) -> Answer {
        let found = (|| {
            let doc = self.doc(uri).ok()?;
            let dir = doc.path.as_deref()?.parent()?.join(context.local_path.as_deref()?);
            let child = self.read_manifest(&dir.join("Cargo.toml"))?;
            let workspace = self.workspace_root(&dir);
            Some(toml_ide::cargo_local_crate(&child, workspace.as_ref()))
        })();
        Answer::Versions(
            found.filter(|v| !v.version.is_empty() || !v.features.is_empty()).into_iter().collect(),
        )
    }

    /// The `Cargo.toml` with a `[workspace]` in `dir` or above, and its path.
    pub(crate) fn workspace_root_at(
        &self,
        dir: &std::path::Path,
    ) -> Option<(std::path::PathBuf, Document)> {
        dir.ancestors().find_map(|d| {
            let path = d.join("Cargo.toml");
            let manifest = self.read_manifest(&path)?;
            manifest.dom.root.get("workspace").is_some().then_some((path, manifest))
        })
    }

    /// The `Cargo.toml` with a `[workspace]` in `dir` or above.
    pub(crate) fn workspace_root(&self, dir: &std::path::Path) -> Option<Document> {
        self.workspace_root_at(dir).map(|(_, d)| d)
    }

    /// The inheritance from the workspace at `offset` of `uri`, with the root
    /// manifest.
    fn workspace_ref(
        &self,
        uri: &Uri,
        offset: toml_ide::TextSize,
    ) -> Option<(toml_ide::WorkspaceRef, std::path::PathBuf, Document)> {
        let doc = self.doc(uri).ok()?;
        if !self.config.cargo || !is_cargo_manifest(doc) {
            return None;
        }
        let reference = toml_ide::cargo_workspace_ref(&doc.doc, offset)?;
        let (path, root) = self.workspace_root_at(doc.path.as_deref()?.parent()?)?;
        Some((reference, path, root))
    }
}
