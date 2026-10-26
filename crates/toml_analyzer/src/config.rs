//! The settings of the server, from the editor and the command line.

use std::{path::PathBuf, time::Duration};

use serde_json::Value;

/// Usage of the command line.
pub(crate) const USAGE: &str = "\
toml_analyzer: language server for TOML, with JSON Schema support

usage:
  toml_analyzer [--cache-dir <dir>]   run the language server over stdio
  toml_analyzer fmt [--check] <files> format TOML files
  toml_analyzer check <files>         report syntax, TOML and schema problems
  toml_analyzer --version

options:
  --cache-dir <dir>   where schemas fetched over HTTP are cached
  --no-cache          do not cache schemas on disk

`RUST_LOG` controls the logs, written to stderr (default: info).";

/// Options of the command line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CliOptions {
    pub(crate) cache_dir: Option<PathBuf>,
    pub(crate) no_cache: bool,
}

impl CliOptions {
    pub(crate) fn parse(args: Vec<String>) -> Result<CliOptions, String> {
        let mut out = CliOptions::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--cache-dir" => {
                    out.cache_dir = Some(args.next().ok_or("--cache-dir needs a value")?.into())
                }
                "--no-cache" => out.no_cache = true,
                // Added by some clients.
                "--stdio" => {}
                other => return Err(format!("unknown option `{other}`\n\n{USAGE}")),
            }
        }
        Ok(out)
    }
}

/// A schema and the files it applies to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Association {
    /// A URL, or a path relative to the first workspace folder.
    pub(crate) url: String,
    /// Glob patterns. A pattern without `/` matches file names.
    pub(crate) file_match: Vec<String>,
}

/// The editor settings, the `toml` section.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Config {
    /// `toml.schemas`: user associations, first match wins.
    pub(crate) schemas: Vec<Association>,
    /// `tomlValidation` contributions of extensions, sent by the client.
    pub(crate) contributed: Vec<Association>,
    /// Use the SchemaStore catalog for files without a schema.
    pub(crate) schema_store: bool,
    /// The catalog URL.
    pub(crate) catalog_url: String,
    /// Keep schemas fetched over HTTP on disk.
    pub(crate) disk_cache: bool,
    /// The directory of the disk cache.
    pub(crate) cache_dir: Option<PathBuf>,
    /// Cached schemas older than this are fetched again.
    pub(crate) cache_max_age: Duration,
    /// Validate documents against their schema.
    pub(crate) validate: bool,
    /// Complete the dependencies of `Cargo.toml` from crates.io.
    pub(crate) cargo: bool,
    /// Complete the versions of dependencies too: off by default, other
    /// extensions do it.
    pub(crate) cargo_versions: bool,
    /// Formatting.
    pub(crate) format: toml_fmt::Options,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            schemas: Vec::new(),
            contributed: Vec::new(),
            schema_store: true,
            catalog_url: "https://www.schemastore.org/api/json/catalog.json".to_owned(),
            disk_cache: true,
            cache_dir: None,
            cache_max_age: Duration::from_secs(24 * 60 * 60),
            validate: true,
            cargo: true,
            cargo_versions: false,
            format: toml_fmt::Options::default(),
        }
    }
}

impl Config {
    /// Reads `{ "toml": { ... } }` or the section itself; missing or invalid
    /// values keep their defaults.
    pub(crate) fn from_json(value: Option<Value>) -> Config {
        let mut config = Config::default();
        let Some(mut value) = value else { return config };
        if let Some(section) = value.get_mut("toml") {
            value = section.take();
        }
        let get = |path: &str| value.pointer(path);
        if let Some(schemas) = get("/schemas") {
            config.schemas = associations(schemas);
        }
        if let Some(contributed) = get("/associations") {
            config.contributed = associations(contributed);
        }
        if let Some(b) = get("/schemaStore/enable").and_then(Value::as_bool) {
            config.schema_store = b;
        }
        if let Some(url) = get("/schemaStore/url").and_then(Value::as_str) {
            config.catalog_url = url.to_owned();
        }
        if let Some(b) = get("/schema/cache").and_then(Value::as_bool) {
            config.disk_cache = b;
        }
        if let Some(dir) =
            get("/schema/cacheDirectory").and_then(Value::as_str).filter(|d| !d.is_empty())
        {
            config.cache_dir = Some(dir.into());
        }
        if let Some(secs) = get("/schema/cacheMaxAge").and_then(Value::as_u64) {
            config.cache_max_age = Duration::from_secs(secs);
        }
        if let Some(b) = get("/validation/enable").and_then(Value::as_bool) {
            config.validate = b;
        }
        if let Some(b) = get("/cargo/enable").and_then(Value::as_bool) {
            config.cargo = b;
        }
        if let Some(b) = get("/cargo/versionCompletion").and_then(Value::as_bool) {
            config.cargo_versions = b;
        }
        let f = &mut config.format;
        if let Some(n) = get("/formatter/indentWidth").and_then(Value::as_u64) {
            f.indent = " ".repeat(n.min(16) as usize);
        }
        if let Some(true) = get("/formatter/useTabs").and_then(Value::as_bool) {
            f.indent = "\t".to_owned();
        }
        if let Some(n) = get("/formatter/maxBlankLines").and_then(Value::as_u64) {
            f.max_blank_lines = n as usize;
        }
        if let Some(b) = get("/formatter/indentTables").and_then(Value::as_bool) {
            f.indent_tables = b;
        }
        if let Some(b) = get("/formatter/alignEntries").and_then(Value::as_bool) {
            f.align_entries = b;
        }
        if let Some(b) = get("/formatter/alignComments").and_then(Value::as_bool) {
            f.align_comments = b;
        }
        if let Some(b) = get("/formatter/crlf").and_then(Value::as_bool) {
            f.crlf = b;
        }
        config
    }

    /// The command line wins over the editor.
    pub(crate) fn with_cli(mut self, cli: &CliOptions) -> Config {
        if let Some(dir) = &cli.cache_dir {
            self.cache_dir = Some(dir.clone());
        }
        if cli.no_cache {
            self.disk_cache = false;
        }
        self
    }
}

/// `{ "url": ["glob", ...] }` or `[{ "url": "...", "fileMatch": [...] }]`.
fn associations(value: &Value) -> Vec<Association> {
    let globs = |v: &Value| -> Vec<String> {
        match v {
            Value::String(s) => vec![s.clone()],
            Value::Array(a) => a.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
            _ => Vec::new(),
        }
    };
    match value {
        Value::Object(map) => map
            .iter()
            .map(|(url, m)| Association { url: url.clone(), file_match: globs(m) })
            .collect(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| {
                let url = i.get("url")?.as_str()?.to_owned();
                Some(Association {
                    url,
                    file_match: i.get("fileMatch").map(globs).unwrap_or_default(),
                })
            })
            .collect(),
        _ => Vec::new(),
    }
}
