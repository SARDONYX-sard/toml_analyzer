//! The `fmt` and `check` subcommands.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use toml_ide::{Document, Schema, SchemaState};

use crate::schemas::{FetchSettings, Schemas, Status, default_cache_dir};

/// `toml_analyzer fmt [--check] <files>`
#[allow(clippy::print_stdout, clippy::print_stderr)]
pub(crate) fn fmt(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: toml_analyzer fmt [--check] <files>");
        return 0;
    }
    let check = args.iter().any(|a| a == "--check");
    let files: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let mut failed = false;
    for file in files {
        let Ok(text) = std::fs::read_to_string(file) else {
            eprintln!("cannot read {file}");
            failed = true;
            continue;
        };
        let Some(formatted) = toml_fmt::format(&text, &toml_fmt::Options::default()) else {
            eprintln!("{file}: syntax errors, not formatted");
            failed = true;
            continue;
        };
        if formatted == text {
            continue;
        }
        if check {
            println!("{file}: not formatted");
            failed = true;
        } else if let Err(e) = std::fs::write(file, formatted) {
            eprintln!("cannot write {file}: {e}");
            failed = true;
        }
    }
    i32::from(failed)
}

/// `toml_analyzer check <files>`: the problems of each file, with the
/// schema named in the file.
#[allow(clippy::print_stdout, clippy::print_stderr)]
pub(crate) fn check(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "usage: toml_analyzer check <files>\n\nThe schema is taken from `#:schema` or `$schema`."
        );
        return 0;
    }
    let mut schemas = Schemas::new(FetchSettings {
        cache_dir: default_cache_dir(),
        max_age: Duration::from_secs(24 * 60 * 60),
        catalog_url: String::new(),
    });
    let mut errors = 0;
    for file in args.iter().filter(|a| !a.starts_with("--")) {
        let path = PathBuf::from(file);
        let Ok(text) = std::fs::read_to_string(&path) else {
            eprintln!("cannot read {file}");
            errors += 1;
            continue;
        };
        let doc = Document::new(text);
        let schema_url = schema_url(&doc, &path);
        let status = match &schema_url {
            Some(Ok(url)) => Some(wait(&mut schemas, url)),
            Some(Err(e)) => Some(Status::Failed(e.clone())),
            None => None,
        };
        let failure;
        let state = match (&status, &schema_url) {
            (Some(Status::Ready), Some(Ok(url))) => {
                SchemaState::Ready(Schema { registry: &schemas.registry, url })
            }
            (Some(Status::Failed(e)), _) => {
                failure = format!("cannot load the schema: {e}");
                SchemaState::Failed(&failure)
            }
            _ => SchemaState::None,
        };
        for d in toml_ide::diagnostics(&doc, state) {
            let lc = doc.line_index.line_col(&doc.text, d.range.start());
            let severity = match d.severity {
                toml_ide::Severity::Error => {
                    errors += 1;
                    "error"
                }
                toml_ide::Severity::Warning => "warning",
                toml_ide::Severity::Information => "info",
                toml_ide::Severity::Hint => continue,
            };
            println!(
                "{file}:{}:{}: {severity}: {} [{}]",
                lc.line + 1,
                lc.col + 1,
                d.message,
                d.code
            );
        }
    }
    i32::from(errors > 0)
}

fn schema_url(doc: &Document, path: &Path) -> Option<Result<String, String>> {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    let Some(location) = toml_ide::schema_location(doc) else {
        // `clippy.toml` has a built-in schema.
        let dir = abs.parent().filter(|_| crate::clippy::is_clippy_config(&abs))?;
        return Some(Ok(crate::clippy::schema_url(dir)));
    };
    let base = url::Url::from_file_path(&abs).ok();
    Some(toml_ide::resolve_location(&location.text, base.as_ref()).map(|u| u.to_string()))
}

/// Loads the schema of `url` and what it references.
fn wait(schemas: &mut Schemas, url: &str) -> Status {
    loop {
        match schemas.ensure(url) {
            Status::Loading => match schemas.rx.recv_timeout(Duration::from_secs(60)) {
                Ok(fetched) => {
                    schemas.on_fetched(fetched);
                }
                Err(_) => return Status::Failed(format!("timed out loading {url}")),
            },
            status => return status,
        }
    }
}
