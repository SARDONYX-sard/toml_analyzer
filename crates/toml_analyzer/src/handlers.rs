//! Request handlers.

use std::str::FromStr;

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
        let doc = &self.doc(&p.text_document.uri).ok()?.doc;
        let schema = url.as_deref().map(|url| Schema { registry: &self.schemas.registry, url });
        let hover = toml_ide::hover(doc, conv::offset(doc, p.position), schema)?;
        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: hover.markdown,
            }),
            range: Some(conv::range(doc, hover.range)),
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
            toml_ide::cargo_context(doc, conv::offset(doc, p.position))
        } else {
            None
        };
        let answer = match &context {
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
        if let (Some(context), Some(answer)) = (&context, &answer) {
            items.extend(match answer {
                Answer::Search(found) => toml_ide::cargo_completions(context, Some(found), None),
                Answer::Versions(versions) => {
                    toml_ide::cargo_completions(context, None, Some(versions))
                }
            });
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
        let (range, target) = self.schema_target(&params.text_document.uri)?;
        let doc = &self.doc(&params.text_document.uri).ok()?.doc;
        Some(vec![DocumentLink {
            range: conv::range(doc, range),
            target: conv::url_to_uri(&target),
            tooltip: Some("Open the schema".to_owned()),
            data: None,
        }])
    }

    pub(crate) fn goto_definition(
        &mut self,
        params: GotoDefinitionParams,
    ) -> Option<GotoDefinitionResponse> {
        let p = params.text_document_position_params;
        let (range, target) = self.schema_target(&p.text_document.uri)?;
        let doc = &self.doc(&p.text_document.uri).ok()?.doc;
        let offset = conv::offset(doc, p.position);
        if !range.contains_inclusive(offset) || target.scheme() != "file" {
            return None;
        }
        Some(GotoDefinitionResponse::Scalar(lsp_types::Location {
            uri: Uri::from_str(target.as_str()).ok()?,
            range: lsp_types::Range::default(),
        }))
    }
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
fn is_cargo_manifest(doc: &crate::server::Doc) -> bool {
    let name = doc.uri.as_str().rsplit('/').next().unwrap_or("");
    name.eq_ignore_ascii_case("Cargo.toml")
}
