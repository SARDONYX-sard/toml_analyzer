//! Language server for TOML, with JSON Schema support.
//!
//! Speaks the Language Server Protocol over stdio. Logs go to stderr and are
//! controlled with the `RUST_LOG` environment variable (default: `info`).

mod cli;
mod clippy;
mod config;
mod conv;
mod crates;
mod ext;
mod handlers;
mod schemas;
mod server;

use lsp_types::{
    CompletionOptions, FoldingRangeProviderCapability, HoverProviderCapability, InitializeParams,
    InitializeResult, OneOf, SaveOptions, SemanticTokensFullOptions, SemanticTokensLegend,
    SemanticTokensOptions, SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions,
    TextDocumentSyncSaveOptions, WorkDoneProgressOptions,
};
use toml_ide::TokenType;

#[allow(clippy::print_stdout)]
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command: Option<fn(&[String]) -> i32> = match args.first().map(String::as_str) {
        Some("fmt") => Some(cli::fmt),
        Some("check") => Some(cli::check),
        _ => None,
    };
    if let Some(command) = command {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
            .target(env_logger::Target::Stderr)
            .init();
        std::process::exit(command(&args[1..]));
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("toml_analyzer {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{}", config::USAGE);
        return Ok(());
    }
    let cli = config::CliOptions::parse(args).map_err(anyhow::Error::msg)?;

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Stderr)
        .init();
    log::info!("toml_analyzer {} starting", env!("CARGO_PKG_VERSION"));

    let (connection, io_threads) = lsp_server::Connection::stdio();
    let (id, params) = connection.initialize_start()?;
    let init: InitializeParams = serde_json::from_value(params)?;
    let result = InitializeResult {
        capabilities: capabilities(),
        server_info: Some(ServerInfo {
            name: "toml_analyzer".to_owned(),
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        }),
    };
    connection.initialize_finish(id, serde_json::to_value(result)?)?;

    server::Server::new(connection.sender.clone(), &init, cli).run(&connection)?;
    drop(connection);
    io_threads.join()?;
    log::info!("toml_analyzer stopped");
    Ok(())
}

fn capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Options(TextDocumentSyncOptions {
            open_close: Some(true),
            change: Some(TextDocumentSyncKind::INCREMENTAL),
            will_save: None,
            will_save_wait_until: None,
            save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                include_text: Some(false),
            })),
        })),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(lsp_types::CodeActionProviderCapability::Options(
            lsp_types::CodeActionOptions {
                code_action_kinds: Some(vec![lsp_types::CodeActionKind::QUICKFIX]),
                ..Default::default()
            },
        )),
        folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
        document_formatting_provider: Some(OneOf::Left(true)),
        document_link_provider: Some(lsp_types::DocumentLinkOptions {
            resolve_provider: Some(false),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        }),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(
                ["=", ".", "[", "\"", " ", "/", ":"].map(str::to_owned).to_vec(),
            ),
            ..Default::default()
        }),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                legend: SemanticTokensLegend {
                    token_types: TokenType::ALL.iter().map(|(_, n)| (*n).into()).collect(),
                    token_modifiers: Vec::new(),
                },
                full: Some(SemanticTokensFullOptions::Bool(true)),
                range: Some(true),
                work_done_progress_options: WorkDoneProgressOptions::default(),
            },
        )),
        ..Default::default()
    }
}
