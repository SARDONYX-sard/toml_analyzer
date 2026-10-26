//! The built-in schema of `clippy.toml`, generated from the documentation of
//! the Clippy configuration options.
//!
//! Clippy publishes no schema, but `book/src/lint_configuration.md` of
//! rust-clippy, generated from its configuration code, lists every option
//! with its description, default value and lints. The schema is generated
//! for the Clippy of the directory of the file (`cargo clippy --version`,
//! which follows `rust-toolchain.toml`): the documentation of its release
//! tag, or of master for nightly toolchains.
//!
//! The schema has the URL `toml-analyzer:clippy?dir=<directory>` and lives
//! in the schema cache like any other: clearing the cache generates it again.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

/// The URL scheme of built-in schemas.
pub(crate) const SCHEME: &str = "toml-analyzer";

/// The file names Clippy reads its configuration from.
pub(crate) fn is_clippy_config(path: &Path) -> bool {
    path.file_name().is_some_and(|n| {
        n.eq_ignore_ascii_case("clippy.toml") || n.eq_ignore_ascii_case(".clippy.toml")
    })
}

/// The URL of the schema of the `clippy.toml` in `dir`.
pub(crate) fn schema_url(dir: &Path) -> String {
    let mut url = url::Url::parse(&format!("{SCHEME}:clippy")).expect("valid url");
    url.query_pairs_mut().append_pair("dir", &dir.to_string_lossy());
    url.to_string()
}

/// The directory of a schema URL made by [`schema_url`].
fn dir_of(url: &url::Url) -> Option<PathBuf> {
    url.query_pairs().find(|(k, _)| k == "dir").map(|(_, v)| PathBuf::from(v.into_owned()))
}

/// The documentation of Clippy at `git_ref` (a tag such as `rust-1.90.0`, or
/// `master`).
fn source_url(git_ref: &str) -> String {
    format!(
        "https://raw.githubusercontent.com/rust-lang/rust-clippy/{git_ref}/book/src/lint_configuration.md"
    )
}

/// The release tag of the Clippy that runs in `dir`, `None` for nightly or
/// when Clippy is not installed.
fn release_tag(dir: &Path) -> Option<String> {
    let mut command = std::process::Command::new("cargo");
    command.args(["clippy", "--version"]).current_dir(dir);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console window.
        command.creation_flags(0x0800_0000);
    }
    let output = command.output().ok()?;
    // `clippy 0.1.99 (58cf8f9c35 2026-10-13)`: Rust 1.99.
    let text = String::from_utf8_lossy(&output.stdout);
    let version = text.split_whitespace().nth(1)?;
    if version.contains('-') {
        return None;
    }
    let minor = version.strip_prefix("0.1.")?.parse::<u32>().ok()?;
    Some(format!("rust-1.{minor}.0"))
}

/// Generates the schema of a [`schema_url`], fetching through `get`.
pub(crate) fn fetch(
    url: &url::Url,
    get: impl Fn(&str) -> Result<String, String>,
) -> Result<Value, String> {
    let dir = dir_of(url).ok_or_else(|| format!("invalid built-in schema `{url}`"))?;
    let tag = release_tag(&dir);
    let (source, markdown) = match tag.as_deref().map(|t| (source_url(t), get(&source_url(t)))) {
        Some((source, Ok(markdown))) => (source, markdown),
        // A release without the documentation: the latest one.
        _ => {
            let source = source_url("master");
            let markdown = get(&source)?;
            (source, markdown)
        }
    };
    let schema = generate(&markdown, &source);
    if schema["properties"].as_object().is_none_or(Map::is_empty) {
        return Err(format!("no Clippy option found in {source}"));
    }
    log::info!("generated the schema of clippy.toml from {source}");
    Ok(schema)
}

/// The schema described by the markdown of the options.
pub(crate) fn generate(markdown: &str, source: &str) -> Value {
    let mut properties = Map::new();
    for section in markdown.split("\n## ").skip(1) {
        let Some(name) = section.strip_prefix('`').and_then(|s| s.split('`').next()) else {
            continue;
        };
        let body = section.split_once('\n').map_or("", |(_, b)| b);
        let lines: Vec<&str> = body.lines().collect();
        let is_meta = |l: &str| l.starts_with("**Default Value:**") || l.starts_with("**Fields:**");
        let description =
            lines.iter().take_while(|l| !is_meta(l)).copied().collect::<Vec<_>>().join("\n");
        let description = description.trim();
        let default = lines
            .iter()
            .find_map(|l| l.strip_prefix("**Default Value:** `")?.strip_suffix('`'))
            .unwrap_or("");
        let fields = fields(&lines);
        let lints: Vec<String> = lines
            .iter()
            .filter_map(|l| {
                let rest = l.strip_prefix("* [`")?;
                let (lint, rest) = rest.split_once("`](")?;
                Some(format!("[`{lint}`]({})", rest.strip_suffix(')')?))
            })
            .collect();

        let mut markdown_description = format!("{description}\n\nDefault: `{default}`");
        if !lints.is_empty() {
            markdown_description.push_str(&format!("\n\nAffected lints: {}", lints.join(", ")));
        }
        markdown_description.push_str(&format!(
            "\n\n[Documentation](https://doc.rust-lang.org/clippy/lint_configuration.html#{name})"
        ));

        let mut schema = type_of(default, &fields);
        let summary = description.split("\n\n").next().unwrap_or("").replace('\n', " ");
        schema.insert("description".to_owned(), Value::String(summary));
        schema.insert("markdownDescription".to_owned(), Value::String(markdown_description));
        if let Ok(value) = serde_json::from_str::<Value>(default) {
            schema.insert("default".to_owned(), value);
        }
        properties.insert(name.to_owned(), Value::Object(schema));
    }
    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "Clippy configuration",
        "description": format!("The configuration of Clippy (clippy.toml), generated from {source}."),
        "type": "object",
        "properties": properties,
        // Clippy rejects unknown keys.
        "additionalProperties": false,
    })
}

/// A field of the tables of a list option, such as `disallowed-methods`.
struct Field {
    name: String,
    required: bool,
    description: String,
}

/// The `**Fields:**` list: `- `name` (required): description`, continued
/// by indented lines.
fn fields(lines: &[&str]) -> Vec<Field> {
    let Some(start) = lines.iter().position(|l| l.starts_with("**Fields:**")) else {
        return Vec::new();
    };
    let mut out: Vec<Field> = Vec::new();
    for line in &lines[start + 1..] {
        if let Some(rest) = line.strip_prefix("- `") {
            let Some((name, rest)) = rest.split_once('`') else { continue };
            let rest = rest.trim_start();
            let (kind, description) = match rest.strip_prefix('(').and_then(|r| r.split_once(')')) {
                Some((kind, d)) => (kind, d.trim_start_matches(':').trim()),
                None => ("", rest),
            };
            out.push(Field {
                name: name.to_owned(),
                required: kind.starts_with("required"),
                description: description.to_owned(),
            });
        } else if line.starts_with("  ") && !out.is_empty() {
            if let Some(last) = out.last_mut() {
                last.description.push(' ');
                last.description.push_str(line.trim());
            }
        } else {
            break;
        }
    }
    out
}

/// The schema of a value, from its default as the documentation writes it.
fn type_of(default: &str, fields: &[Field]) -> Map<String, Value> {
    let v = default.trim();
    let schema = if v == "true" || v == "false" {
        json!({ "type": "boolean" })
    } else if v.parse::<i64>().is_ok() {
        json!({ "type": "integer" })
    } else if v.parse::<f64>().is_ok() {
        json!({ "type": "number" })
    } else if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        json!({ "type": "string" })
    } else if v.starts_with('[') {
        if !fields.is_empty() {
            // A path, or a table with the path and more.
            let properties: Map<String, Value> = fields
                .iter()
                .map(|f| (f.name.clone(), json!({ "description": f.description })))
                .collect();
            let required: Vec<&str> =
                fields.iter().filter(|f| f.required).map(|f| f.name.as_str()).collect();
            json!({
                "type": "array",
                "items": { "anyOf": [
                    { "type": "string" },
                    { "type": "object", "properties": properties, "required": required, "additionalProperties": false },
                ]},
            })
        } else if v == "[]" || v.starts_with("[\"") {
            json!({ "type": "array", "items": { "type": "string" } })
        } else {
            json!({ "type": "array" })
        }
    } else {
        // `current version`, `target_pointer_width`, ...: written as strings.
        json!({ "type": "string" })
    };
    match schema {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options() {
        let markdown = "# Lint Configuration Options\n\n---\n\n## `absolute-paths-max-segments`\nThe maximum number of segments a path can have before being linted, anything above this will\nbe linted.\n\n**Default Value:** `2`\n\n---\n**Affected lints:**\n* [`absolute_paths`](https://clippy.rust-lang.org/main/index.html#absolute_paths)\n\n\n## `disallowed-methods`\nThe list of disallowed methods.\n\n**Fields:**\n- `path` (required): the fully qualified path\n- `allow-invalid` (optional, `false` by default): when set to `true`, it will ignore this entry\n  if the path doesn't exist\n\n**Default Value:** `[]`\n\n---\n**Affected lints:**\n* [`disallowed_methods`](https://clippy.rust-lang.org/main/index.html#disallowed_methods)\n\n\n## `msrv`\nThe minimum rust version.\n\n**Default Value:** `current version`\n";
        let schema = generate(markdown, "test");
        let p = &schema["properties"];
        assert_eq!(p["absolute-paths-max-segments"]["type"], "integer");
        assert_eq!(p["absolute-paths-max-segments"]["default"], 2);
        assert_eq!(
            p["absolute-paths-max-segments"]["description"],
            "The maximum number of segments a path can have before being linted, anything above this will be linted."
        );
        let item = &p["disallowed-methods"]["items"]["anyOf"][1];
        assert_eq!(item["required"], json!(["path"]));
        assert_eq!(
            item["properties"]["allow-invalid"]["description"],
            "when set to `true`, it will ignore this entry if the path doesn't exist"
        );
        assert_eq!(p["msrv"]["type"], "string");
        assert!(p["msrv"].get("default").is_none());

        let url = url::Url::parse(&schema_url(Path::new("/a b/c"))).unwrap();
        assert_eq!(dir_of(&url), Some(PathBuf::from("/a b/c")));
    }
}
