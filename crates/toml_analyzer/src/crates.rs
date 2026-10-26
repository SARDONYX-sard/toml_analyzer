//! The crates.io registry, for the dependencies of `Cargo.toml`.
//!
//! Versions and features come from the sparse index
//! (`https://index.crates.io`), which is made for this; crate names from the
//! search API of crates.io, at most one request per second as its policy
//! asks. Answers are kept in memory for a few minutes; fetching happens on
//! background threads that report through [`Crates::rx`].

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, Sender};
use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::Value;
use toml_ide::{CargoQuery, CrateSummary, CrateVersion};

/// Answers are fetched again after this.
const TTL: Duration = Duration::from_secs(10 * 60);
/// Failed requests are tried again after this.
const RETRY: Duration = Duration::from_secs(30);

/// An answer of the registry.
#[derive(Clone)]
pub(crate) enum Answer {
    Search(Arc<[CrateSummary]>),
    Versions(Arc<[CrateVersion]>),
}

enum Entry {
    Loading,
    Ready(Answer, Instant),
    Failed(Instant),
}

pub(crate) struct Fetched {
    query: CargoQuery,
    result: Result<Answer, String>,
}

pub(crate) struct Crates {
    entries: FxHashMap<CargoQuery, Entry>,
    tx: Sender<Fetched>,
    pub(crate) rx: Receiver<Fetched>,
    /// When the last search was sent, shared by the fetching threads.
    last_search: Arc<Mutex<Instant>>,
}

impl Crates {
    pub(crate) fn new() -> Crates {
        let (tx, rx) = crossbeam_channel::unbounded();
        let past = Instant::now().checked_sub(Duration::from_secs(1)).unwrap_or_else(Instant::now);
        Crates { entries: FxHashMap::default(), tx, rx, last_search: Arc::new(Mutex::new(past)) }
    }

    /// The answer to `query`; `None` while it is fetched. Failures answer
    /// nothing.
    pub(crate) fn get(&mut self, query: &CargoQuery) -> Option<Answer> {
        let empty = || match query {
            CargoQuery::Search(_) => Answer::Search(Arc::from(Vec::new())),
            CargoQuery::Versions(_) => Answer::Versions(Arc::from(Vec::new())),
        };
        match self.entries.get(query) {
            Some(Entry::Ready(answer, at)) if at.elapsed() < TTL => return Some(answer.clone()),
            Some(Entry::Loading) => return None,
            Some(Entry::Failed(at)) if at.elapsed() < RETRY => return Some(empty()),
            _ => {}
        }
        self.entries.insert(query.clone(), Entry::Loading);
        let (owned, tx, last_search) = (query.clone(), self.tx.clone(), self.last_search.clone());
        let spawned =
            std::thread::Builder::new().name("toml-crates-fetch".to_owned()).spawn(move || {
                let query = owned;
                let result = match &query {
                    CargoQuery::Search(q) => {
                        search(q, &last_search).map(|r| Answer::Search(Arc::from(r)))
                    }
                    CargoQuery::Versions(name) => {
                        versions(name).map(|r| Answer::Versions(Arc::from(r)))
                    }
                };
                let _ = tx.send(Fetched { query, result });
            });
        if let Err(e) = spawned {
            log::error!("cannot spawn a fetching thread: {e}");
            self.entries.insert(query.clone(), Entry::Failed(Instant::now()));
            return Some(empty());
        }
        None
    }

    pub(crate) fn on_fetched(&mut self, fetched: Fetched) {
        let entry = match fetched.result {
            Ok(answer) => Entry::Ready(answer, Instant::now()),
            Err(e) => {
                log::warn!("{e}");
                Entry::Failed(Instant::now())
            }
        };
        self.entries.insert(fetched.query, entry);
    }

    /// Forgets every answer.
    pub(crate) fn clear(&mut self) {
        self.entries.retain(|_, e| matches!(e, Entry::Loading));
    }
}

fn get(url: &str) -> Result<Option<String>, String> {
    let agent: ureq::Agent =
        ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(15))).build().into();
    let response = agent.get(url).header("User-Agent", crate::schemas::USER_AGENT).call();
    match response {
        Ok(mut r) => r
            .body_mut()
            .with_config()
            .limit(32 * 1024 * 1024)
            .read_to_string()
            .map(Some)
            .map_err(|e| format!("cannot fetch {url}: {e}")),
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(e) => Err(format!("cannot fetch {url}: {e}")),
    }
}

/// Searches crates.io.
fn search(query: &str, last_search: &Mutex<Instant>) -> Result<Vec<CrateSummary>, String> {
    {
        // One request per second at most.
        let mut last = last_search.lock().unwrap_or_else(|e| e.into_inner());
        let wait = Duration::from_secs(1).saturating_sub(last.elapsed());
        std::thread::sleep(wait);
        *last = Instant::now();
    }
    let url = url::Url::parse_with_params(
        "https://crates.io/api/v1/crates",
        &[("q", query), ("per_page", "25")],
    )
    .map_err(|e| e.to_string())?;
    let text = get(url.as_str())?.unwrap_or_default();
    let json: Value =
        serde_json::from_str(&text).map_err(|e| format!("invalid answer of crates.io: {e}"))?;
    let crates = json.get("crates").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut found: Vec<(CrateSummary, u64)> = crates
        .iter()
        .filter_map(|c| {
            let name = c.get("name")?.as_str()?.to_owned();
            let version = c
                .get("max_stable_version")
                .and_then(Value::as_str)
                .or_else(|| c.get("max_version").and_then(Value::as_str))?
                .to_owned();
            let description =
                c.get("description").and_then(Value::as_str).map(|d| d.trim().to_owned());
            let downloads = c.get("recent_downloads").and_then(Value::as_u64).unwrap_or(0);
            Some((CrateSummary { name, version, description }, downloads))
        })
        .collect();
    // The exact name first, then those starting with the query, the most
    // downloaded first.
    let lower = query.to_ascii_lowercase().replace('_', "-");
    found.sort_by_key(|(c, downloads)| {
        let n = c.name.to_ascii_lowercase().replace('_', "-");
        let group = if n == lower {
            0
        } else if n.starts_with(&lower) {
            1
        } else {
            2
        };
        (group, std::cmp::Reverse(*downloads))
    });
    let out = found.into_iter().map(|(c, _)| c).collect();
    Ok(out)
}

/// The path of a crate in the sparse index.
fn index_path(name: &str) -> String {
    let name = name.to_ascii_lowercase();
    match name.len() {
        1 => format!("1/{name}"),
        2 => format!("2/{name}"),
        3 => format!("3/{}/{name}", &name[..1]),
        _ => format!("{}/{}/{name}", &name[..2], &name[2..4]),
    }
}

/// The versions of a crate, from the sparse index. An unknown crate has
/// none.
fn versions(name: &str) -> Result<Vec<CrateVersion>, String> {
    if name.is_empty() || !name.is_ascii() {
        return Ok(Vec::new());
    }
    let url = format!("https://index.crates.io/{}", index_path(name));
    let Some(text) = get(&url)? else { return Ok(Vec::new()) };
    Ok(text.lines().filter_map(parse_index_line).collect())
}

fn parse_index_line(line: &str) -> Option<CrateVersion> {
    let v: Value = serde_json::from_str(line).ok()?;
    let version = v.get("vers")?.as_str()?.to_owned();
    let yanked = v.get("yanked").and_then(Value::as_bool).unwrap_or(false);
    let mut features: Vec<String> = Vec::new();
    let mut referenced = FxHashSet::default();
    for key in ["features", "features2"] {
        for (name, enables) in v.get(key).and_then(Value::as_object).into_iter().flatten() {
            features.push(name.clone());
            for e in enables.as_array().into_iter().flatten().filter_map(Value::as_str) {
                if let Some(dep) = e.strip_prefix("dep:") {
                    referenced.insert(dep.to_owned());
                }
            }
        }
    }
    // Optional dependencies are features, unless a `dep:` hides them.
    for dep in v.get("deps").and_then(Value::as_array).into_iter().flatten() {
        if dep.get("optional").and_then(Value::as_bool) == Some(true)
            && let Some(name) = dep.get("name").and_then(Value::as_str)
            && !referenced.contains(name)
            && !features.iter().any(|f| f == name)
        {
            features.push(name.to_owned());
        }
    }
    features.sort_by(|a, b| (a != "default", a).cmp(&(b != "default", b)));
    Some(CrateVersion { version, yanked, features })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index() {
        assert_eq!(index_path("a"), "1/a");
        assert_eq!(index_path("cc"), "2/cc");
        assert_eq!(index_path("syn"), "3/s/syn");
        assert_eq!(index_path("Serde"), "se/rd/serde");
        let line = r#"{"name":"x","vers":"1.0.0","deps":[{"name":"a","optional":true},{"name":"b","optional":true},{"name":"c","optional":false}],"features":{"std":[],"default":["std"]},"features2":{"b":["dep:b"]},"yanked":false}"#;
        let v = parse_index_line(line).unwrap();
        assert_eq!(v.features, ["default", "a", "b", "std"]);
    }
}
