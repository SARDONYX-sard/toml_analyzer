//! Fetching, caching and refreshing schemas.
//!
//! Schemas are fetched on background threads, which report through
//! [`Schemas::rx`]; documents referenced by `$ref` are fetched in turn until
//! the schema is complete.
//!
//! # Freshness
//!
//! A schema must never outlive its source:
//!
//! - Local files (`file://` URLs, which relative paths of `$schema` and
//!   `#:schema` resolve to) are never cached on disk. Each loaded file is
//!   remembered with its modification time and size, and
//!   [`Schemas::refresh_files`], called whenever a document changes or is
//!   validated again, drops the files that changed or disappeared: they are
//!   read again on the next use. Watched file events
//!   ([`Schemas::invalidate_path`]) drop them at once.
//! - Documents fetched over HTTP are cached on disk for `cacheMaxAge`, and a
//!   stale copy is used when the network fails.
//! - [`Schemas::clear`] (the `toml/clearSchemaCache` request) forgets every
//!   schema, in memory and on disk, and fetches them again.

use std::{
    io::Read as _,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use crossbeam_channel::{Receiver, Sender};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use rustc_hash::FxHashMap;
use serde_json::Value;
use toml_schema::{Registry, normalize_url};
use url::Url;

/// Where a schema stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// The schema and every document it references are loaded.
    Ready,
    /// Something is being fetched.
    Loading,
    /// The schema cannot be loaded.
    Failed(String),
}

/// The identity of a loaded local file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileStamp {
    path: PathBuf,
    modified: Option<SystemTime>,
    len: u64,
}

impl FileStamp {
    fn of(path: &Path) -> Option<FileStamp> {
        let meta = std::fs::metadata(path).ok()?;
        Some(FileStamp { path: path.to_owned(), modified: meta.modified().ok(), len: meta.len() })
    }
}

enum State {
    Loading,
    /// The reason, and for local files what the file was then (it may be
    /// fixed or created later).
    Failed(String, Option<FileStamp>),
    Loaded {
        file: Option<FileStamp>,
    },
}

/// What the fetching threads send.
pub(crate) enum Fetched {
    Schema { generation: u64, url: String, result: Result<(Value, Option<FileStamp>), String> },
    Catalog { generation: u64, result: Result<Vec<CatalogEntry>, String> },
}

/// A schema of the SchemaStore catalog.
pub(crate) struct CatalogEntry {
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) url: String,
    /// The TOML files it applies to, if any.
    globs: Option<GlobSet>,
}

enum Catalog {
    NotLoaded,
    Loading,
    Loaded(Vec<CatalogEntry>),
    Failed,
}

/// The catalog's answer for a file.
pub(crate) enum CatalogMatch {
    /// The schema of the file.
    Found(String),
    /// The catalog is being fetched.
    Loading,
    /// No schema.
    None,
}

/// How documents are fetched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FetchSettings {
    /// The disk cache of HTTP documents, `None` when disabled.
    pub(crate) cache_dir: Option<PathBuf>,
    pub(crate) max_age: Duration,
    pub(crate) catalog_url: String,
}

/// The schemas known to the server.
pub(crate) struct Schemas {
    pub(crate) registry: Registry,
    states: FxHashMap<String, State>,
    /// Incremented by [`Schemas::clear`]: results of older fetches are
    /// dropped.
    generation: u64,
    tx: Sender<Fetched>,
    pub(crate) rx: Receiver<Fetched>,
    catalog: Catalog,
    settings: FetchSettings,
}

impl Schemas {
    pub(crate) fn new(settings: FetchSettings) -> Schemas {
        let (tx, rx) = crossbeam_channel::unbounded();
        Schemas {
            registry: Registry::new(),
            states: FxHashMap::default(),
            generation: 0,
            tx,
            rx,
            catalog: Catalog::NotLoaded,
            settings,
        }
    }

    pub(crate) fn set_settings(&mut self, settings: FetchSettings) {
        if settings.catalog_url != self.settings.catalog_url {
            self.catalog = Catalog::NotLoaded;
        }
        self.settings = settings;
    }

    /// The status of the schema of `url`, fetching what is missing.
    pub(crate) fn ensure(&mut self, url: &str) -> Status {
        let key = normalize_url(url);
        match self.states.get(&key) {
            None => {
                self.request(&key);
                return Status::Loading;
            }
            Some(State::Loading) => return Status::Loading,
            Some(State::Failed(e, _)) => return Status::Failed(e.clone()),
            Some(State::Loaded { .. }) => {}
        }
        let mut loading = false;
        for missing in self.registry.missing(&key) {
            match self.states.get(&missing) {
                None => {
                    self.request(&missing);
                    loading = true;
                }
                Some(State::Loading) => loading = true,
                // A referenced document that cannot be loaded is skipped by
                // validation; the failure is in the log.
                Some(State::Failed(..) | State::Loaded { .. }) => {}
            }
        }
        if loading { Status::Loading } else { Status::Ready }
    }

    fn request(&mut self, url: &str) {
        log::info!("loading schema {url}");
        self.states.insert(url.to_owned(), State::Loading);
        let (url, tx, settings, generation) =
            (url.to_owned(), self.tx.clone(), self.settings.clone(), self.generation);
        let spawned =
            std::thread::Builder::new().name("toml-schema-fetch".to_owned()).spawn(move || {
                let result = fetch_schema(&url, &settings);
                let _ = tx.send(Fetched::Schema { generation, url, result });
            });
        if let Err(e) = spawned {
            log::error!("cannot spawn a fetching thread: {e}");
        }
    }

    /// Handles a fetch result; `true` when diagnostics should be refreshed.
    pub(crate) fn on_fetched(&mut self, fetched: Fetched) -> bool {
        match fetched {
            Fetched::Schema { generation, url, result } => {
                if generation != self.generation {
                    return false;
                }
                match result {
                    Ok((doc, file)) => {
                        self.registry.insert(&url, doc);
                        self.states.insert(url, State::Loaded { file });
                    }
                    Err(e) => {
                        log::warn!("{e}");
                        let stamp = file_path(&url).map(|p| FileStamp::of(&p));
                        self.states.insert(url, State::Failed(e, stamp.flatten()));
                    }
                }
                true
            }
            Fetched::Catalog { generation, result } => {
                if generation != self.generation {
                    return false;
                }
                self.catalog = match result {
                    Ok(entries) => {
                        log::info!(
                            "SchemaStore catalog: {} schemas, {} for TOML files",
                            entries.len(),
                            entries.iter().filter(|e| e.globs.is_some()).count()
                        );
                        Catalog::Loaded(entries)
                    }
                    Err(e) => {
                        log::warn!("cannot load the SchemaStore catalog: {e}");
                        Catalog::Failed
                    }
                };
                true
            }
        }
    }

    /// Drops the local schema files that changed since they were read;
    /// `true` when one was dropped.
    pub(crate) fn refresh_files(&mut self) -> bool {
        let stale: Vec<String> = self
            .states
            .iter()
            .filter_map(|(url, state)| match state {
                State::Loaded { file: Some(stamp) }
                    if FileStamp::of(&stamp.path).as_ref() != Some(stamp) =>
                {
                    Some(url.clone())
                }
                // A file that could not be read or parsed may be fixed now.
                State::Failed(_, stamp) => {
                    let path = file_path(url)?;
                    (FileStamp::of(&path) != *stamp).then(|| url.clone())
                }
                _ => None,
            })
            .collect();
        for url in &stale {
            log::info!("schema changed on disk: {url}");
            self.forget(url);
        }
        !stale.is_empty()
    }

    /// Drops the schema read from `path`; `true` when there was one.
    pub(crate) fn invalidate_path(&mut self, path: &Path) -> bool {
        let Ok(url) = Url::from_file_path(path) else { return false };
        let key = normalize_url(url.as_str());
        let known = self.states.contains_key(&key);
        self.forget(&key);
        known
    }

    fn forget(&mut self, url: &str) {
        self.registry.remove(url);
        self.states.remove(url);
    }

    /// Forgets every schema and the catalog, in memory and on disk. Returns
    /// the number of removed cache files.
    pub(crate) fn clear(&mut self) -> usize {
        self.registry.clear();
        self.states.clear();
        self.catalog = Catalog::NotLoaded;
        self.generation += 1;
        let mut removed = 0;
        if let Some(dir) = &self.settings.cache_dir
            && let Ok(entries) = std::fs::read_dir(dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "json")
                    && std::fs::remove_file(&path).is_ok()
                {
                    removed += 1;
                }
            }
        }
        log::info!("schema cache cleared ({removed} files removed)");
        removed
    }

    /// The loaded schemas and their state, for `toml/status`.
    pub(crate) fn report(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .states
            .iter()
            .map(|(url, s)| {
                let state = match s {
                    State::Loading => "loading".to_owned(),
                    State::Failed(e, _) => format!("failed: {e}"),
                    State::Loaded { file: Some(_) } => "loaded from disk (watched)".to_owned(),
                    State::Loaded { file: None } => "loaded".to_owned(),
                };
                (url.clone(), state)
            })
            .collect();
        out.sort();
        out
    }

    /// The schemas of the catalog, for choosing one: those of TOML files
    /// first. `None` while the catalog is fetched.
    pub(crate) fn catalog_list(&mut self) -> Option<Vec<(&CatalogEntry, bool)>> {
        if matches!(self.catalog, Catalog::NotLoaded) {
            // Starts fetching it.
            let _ = self.catalog_match(Path::new(""));
        }
        match &self.catalog {
            Catalog::Loaded(entries) => {
                let mut list: Vec<(&CatalogEntry, bool)> =
                    entries.iter().map(|e| (e, e.globs.is_some())).collect();
                list.sort_by_key(|(_, toml)| !toml);
                Some(list)
            }
            Catalog::Failed => Some(Vec::new()),
            Catalog::NotLoaded | Catalog::Loading => None,
        }
    }

    /// The schema of the SchemaStore catalog for `path`.
    pub(crate) fn catalog_match(&mut self, path: &Path) -> CatalogMatch {
        match &self.catalog {
            Catalog::NotLoaded => {
                self.catalog = Catalog::Loading;
                let (tx, settings, generation) =
                    (self.tx.clone(), self.settings.clone(), self.generation);
                let spawned = std::thread::Builder::new()
                    .name("toml-catalog-fetch".to_owned())
                    .spawn(move || {
                        let result = fetch_catalog(&settings);
                        let _ = tx.send(Fetched::Catalog { generation, result });
                    });
                if let Err(e) = spawned {
                    log::error!("cannot spawn a fetching thread: {e}");
                    self.catalog = Catalog::Failed;
                }
                CatalogMatch::Loading
            }
            Catalog::Loading => CatalogMatch::Loading,
            Catalog::Failed => CatalogMatch::None,
            Catalog::Loaded(entries) => {
                let path = slash_path(path);
                entries
                    .iter()
                    .find(|e| e.globs.as_ref().is_some_and(|g| g.is_match(&path)))
                    .map_or(CatalogMatch::None, |e| CatalogMatch::Found(e.url.clone()))
            }
        }
    }
}

/// The path of a `file:` URL.
fn file_path(url: &str) -> Option<PathBuf> {
    Url::parse(url).ok().filter(|u| u.scheme() == "file")?.to_file_path().ok()
}

/// The path with `/` separators, for glob matching.
pub(crate) fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Builds the patterns of an association. A pattern without `/` matches
/// file names in any directory; a relative one with `/` is relative to
/// `root` (the workspace folder). Returns the set and the patterns it holds,
/// as written, in order.
pub(crate) fn glob_set(patterns: &[String], root: Option<&Path>) -> Option<(GlobSet, Vec<String>)> {
    let mut builder = GlobSetBuilder::new();
    let mut kept = Vec::new();
    for written in patterns {
        let p = written.replace('\\', "/");
        let absolute =
            p.starts_with('/') || p.starts_with("**") || p.as_bytes().get(1) == Some(&b':');
        let p = match root {
            _ if !p.contains('/') => format!("**/{p}"),
            Some(root) if !absolute => {
                format!("{}/{}", slash_path(root).trim_end_matches('/'), p.trim_start_matches("./"))
            }
            _ => p,
        };
        match GlobBuilder::new(&p).case_insensitive(cfg!(windows)).literal_separator(false).build()
        {
            Ok(glob) => {
                builder.add(glob);
                kept.push(written.clone());
            }
            Err(e) => log::warn!("invalid file pattern `{p}`: {e}"),
        }
    }
    if kept.is_empty() {
        return None;
    }
    builder.build().ok().map(|set| (set, kept))
}
fn fetch_schema(url: &str, settings: &FetchSettings) -> Result<(Value, Option<FileStamp>), String> {
    let parsed = Url::parse(url).map_err(|e| format!("invalid schema URL `{url}`: {e}"))?;
    if parsed.scheme() == crate::clippy::SCHEME {
        let schema = crate::clippy::fetch(&parsed, |u| http_get_cached(u, settings))?;
        return Ok((schema, None));
    }
    let (text, stamp) = match parsed.scheme() {
        "file" => {
            let path =
                parsed.to_file_path().map_err(|()| format!("invalid schema path `{url}`"))?;
            // The stamp first: a change while reading is seen next time.
            let stamp = FileStamp::of(&path);
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read the schema {}: {e}", path.display()))?;
            (text, stamp)
        }
        "http" | "https" => (http_get_cached(url, settings)?, None),
        other => return Err(format!("unsupported schema URL scheme `{other}` in `{url}`")),
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let value = serde_json::from_str(text)
        .map_err(|e| format!("the schema {url} is not valid JSON: {e}"))?;
    Ok((value, stamp))
}

fn fetch_catalog(settings: &FetchSettings) -> Result<Vec<CatalogEntry>, String> {
    let text = http_get_cached(&settings.catalog_url, settings)?;
    let catalog: Value =
        serde_json::from_str(&text).map_err(|e| format!("invalid catalog: {e}"))?;
    let schemas =
        catalog.get("schemas").and_then(Value::as_array).ok_or("invalid catalog: no `schemas`")?;
    Ok(schemas
        .iter()
        .filter_map(|s| {
            let url = s.get("url")?.as_str()?.to_owned();
            let name = s.get("name").and_then(Value::as_str).unwrap_or(&url).to_owned();
            let description = s.get("description").and_then(Value::as_str).map(str::to_owned);
            let all: Vec<&str> = s
                .get("fileMatch")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let is_toml = |p: &&str| p.to_ascii_lowercase().ends_with(".toml");
            // A schema of TOML files also takes their names without an
            // extension: `.cargo/config` beside `.cargo/config.toml`.
            let patterns: Vec<String> = if all.iter().any(is_toml) {
                all.iter()
                    .filter(|p| is_toml(p) || !p.rsplit('/').next().unwrap_or(p).contains('.'))
                    .map(|p| (*p).to_owned())
                    .collect()
            } else {
                Vec::new()
            };
            let globs = glob_set(&patterns, None).map(|(set, _)| set);
            Some(CatalogEntry { name, description, url, globs })
        })
        .collect())
}

/// GETs `url`, through the disk cache when enabled.
fn http_get_cached(url: &str, settings: &FetchSettings) -> Result<String, String> {
    let cached = settings.cache_dir.as_ref().map(|d| d.join(format!("{:016x}.json", fnv1a(url))));
    if let Some(path) = &cached
        && let Ok(meta) = std::fs::metadata(path)
        && meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age < settings.max_age)
        && let Ok(text) = std::fs::read_to_string(path)
    {
        log::debug!("{url} from the cache {}", path.display());
        return Ok(text);
    }
    match http_get(url) {
        Ok(text) => {
            if let Some(path) = &cached {
                let written = path
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| std::fs::write(path, &text));
                if let Err(e) = written {
                    log::warn!("cannot cache {url} in {}: {e}", path.display());
                }
            }
            Ok(text)
        }
        Err(e) => match cached.as_ref().and_then(|p| std::fs::read_to_string(p).ok()) {
            Some(text) => {
                log::warn!("{e}; using the cached copy");
                Ok(text)
            }
            None => Err(e),
        },
    }
}

/// Identifies the server, as crates.io asks.
pub(crate) const USER_AGENT: &str = concat!(
    "toml_analyzer/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/SARDONYX-sard/toml_analyzer)"
);

fn http_get(url: &str) -> Result<String, String> {
    let agent: ureq::Agent =
        ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(20))).build().into();
    let mut response = agent
        .get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| format!("cannot fetch {url}: {e}"))?;
    let mut text = String::new();
    response
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .reader()
        .read_to_string(&mut text)
        .map_err(|e| format!("cannot fetch {url}: {e}"))?;
    Ok(text)
}

/// A stable hash of the URL, for cache file names.
fn fnv1a(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// The default cache directory of the platform.
pub(crate) fn default_cache_dir() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if cfg!(windows) {
        env("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Caches"))
    } else {
        env("XDG_CACHE_HOME").or_else(|| env("HOME").map(|h| h.join(".cache")))
    };
    base.map(|b| b.join("toml_analyzer").join("schemas"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait(schemas: &mut Schemas, url: &str) -> Status {
        loop {
            match schemas.ensure(url) {
                Status::Loading => {
                    let fetched =
                        schemas.rx.recv_timeout(Duration::from_secs(10)).expect("fetched");
                    schemas.on_fetched(fetched);
                }
                status => return status,
            }
        }
    }

    fn type_of(schemas: &Schemas, url: &str) -> Value {
        let defs = url.replace("main.json", "defs.json");
        schemas.registry.resolve(&Url::parse(&defs).unwrap(), "#/a").unwrap().value["type"].clone()
    }

    /// Edited local schemas are read again: they must never stay stale.
    #[test]
    fn local_files_are_refreshed() {
        let dir = std::env::temp_dir().join(format!("toml_analyzer_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.json"), r#"{"properties": {"a": {"$ref": "defs.json#/a"}}}"#)
            .unwrap();
        std::fs::write(dir.join("defs.json"), r#"{"a": {"type": "string"}}"#).unwrap();
        let url = Url::from_file_path(dir.join("main.json")).unwrap().to_string();
        let settings =
            FetchSettings { cache_dir: None, max_age: Duration::ZERO, catalog_url: String::new() };
        let mut schemas = Schemas::new(settings);

        assert_eq!(wait(&mut schemas, &url), Status::Ready);
        assert_eq!(type_of(&schemas, &url), "string");
        assert!(!schemas.refresh_files());

        // A referenced file changes (another size: the time may not move).
        std::fs::write(dir.join("defs.json"), r#"{"a": {"type": "integer" }}"#).unwrap();
        assert!(schemas.refresh_files());
        assert_eq!(wait(&mut schemas, &url), Status::Ready);
        assert_eq!(type_of(&schemas, &url), "integer");

        // Broken, then fixed.
        std::fs::write(dir.join("main.json"), "{").unwrap();
        assert!(schemas.invalidate_path(&dir.join("main.json")));
        assert!(matches!(wait(&mut schemas, &url), Status::Failed(_)));
        std::fs::write(dir.join("main.json"), r#"{"properties": {"a": {"$ref": "defs.json#/a"}}}"#)
            .unwrap();
        assert!(schemas.refresh_files());
        assert_eq!(wait(&mut schemas, &url), Status::Ready);

        // Clearing forgets everything.
        schemas.clear();
        assert!(!schemas.registry.contains(&url));
        assert_eq!(wait(&mut schemas, &url), Status::Ready);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
