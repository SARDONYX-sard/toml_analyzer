use expect_test::{Expect, expect};
use serde_json::json;
use toml_parser::TextSize;

use crate::{
    Document, Registry, Schema, SchemaState, completions, diagnostics, hover, position_at,
    resolve_location, schema_location,
};

const URL: &str = "file:///p/schema.json";

fn registry() -> Registry {
    let mut r = Registry::new();
    r.insert(
        URL,
        json!({
            "type": "object",
            "properties": {
                "package": {
                    "type": "object",
                    "title": "Package",
                    "description": "The package.",
                    "required": ["name"],
                    "properties": {
                        "name": {"type": "string", "description": "The name."},
                        "edition": {"enum": ["2021", "2024"], "default": "2024"},
                        "publish": {"type": "boolean"},
                    },
                    "additionalProperties": false,
                },
                "bin": {"type": "array", "items": {"type": "object", "properties": {"path": {"type": "string"}}}},
                "version": {"type": "string"},
            },
        }),
    );
    r
}

/// Splits `$0` out of `src`.
fn cursor(src: &str) -> (Document, TextSize) {
    let offset = src.find("$0").expect("a cursor");
    let text = src.replace("$0", "");
    (Document::new(text), TextSize::new(offset as u32))
}

fn check_completions(src: &str, expect: Expect) {
    let (doc, offset) = cursor(src);
    let r = registry();
    let schema = Schema { registry: &r, url: URL };
    let mut items = completions(&doc, offset, schema, true);
    items.sort_by(|a, b| a.sort.cmp(&b.sort));
    let out: Vec<String> =
        items.iter().map(|i| format!("{:?} {} => {}", i.kind, i.label, i.insert)).collect();
    expect.assert_eq(&out.join("\n"));
}

#[test]
fn key_completions() {
    check_completions(
        "[package]\nname = \"x\"\n$0\n",
        expect![[r#"
            Key edition => edition = ${1|"2021","2024"|}
            Key publish => publish = ${1|true,false|}"#]],
    );
    check_completions(
        "[$0]\n",
        expect![[r#"
            Table bin => bin
            Table package => package"#]],
    );
    check_completions(
        "package.na$0\n",
        expect![[r#"
            Key name => name = "$1"
            Key edition => edition = ${1|"2021","2024"|}
            Key publish => publish = ${1|true,false|}"#]],
    );
}

#[test]
fn value_completions() {
    check_completions(
        "[package]\nedition = $0\n",
        expect![[r#"
            Value "2021" => "2021"
            Value "2024" => "2024""#]],
    );
    check_completions(
        "package = { publish = t$0 }\n",
        expect![[r#"
            Value true => true
            Value false => false"#]],
    );
}

#[test]
fn positions() {
    let (doc, offset) = cursor("[[bin]]\n[[bin]]\npath = [\"a\", $0]\n");
    let p = position_at(&doc, offset);
    assert_eq!(format!("{:?}", p.path), r#"[Key("bin"), Index(1), Key("path"), Index(1)]"#);
}

#[test]
fn schema_diagnostics_and_hover() {
    let r = registry();
    let schema = Schema { registry: &r, url: URL };
    let doc = Document::new("#:schema ./schema.json\nversion = 1\n[package]\nnmae = \"x\"\n");
    let found: Vec<String> = diagnostics(&doc, SchemaState::Ready(schema))
        .into_iter()
        .map(|d| format!("{:?} {}: {}", d.range, d.code, d.message))
        .collect();
    expect![[r#"
        [
            "33..34 schema: expected a string, found an integer",
            "36..43 schema: missing required key `name`",
            "45..49 schema: unknown key `nmae`, did you mean `name`?",
        ]
    "#]]
    .assert_debug_eq(&found);

    let (doc, offset) = cursor("[pack$0age]\n");
    let h = hover(&doc, offset, Some(schema)).unwrap();
    assert_eq!(h.markdown, "**Package**: `object`\n\nThe package.");
}

#[test]
fn schema_locations() {
    let doc = Document::new("# comment\n#:schema  ../s.json \na = 1\n");
    let loc = schema_location(&doc).unwrap();
    assert_eq!((loc.text.as_str(), &doc.text[loc.range]), ("../s.json", "../s.json"));
    let doc = Document::new("\"$schema\" = 'https://example.com/s.json'\n");
    assert_eq!(schema_location(&doc).unwrap().text, "https://example.com/s.json");

    let base = url::Url::parse("file:///p/dir/Cargo.toml").unwrap();
    let resolve = |s| resolve_location(s, Some(&base)).map(|u| u.to_string());
    assert_eq!(resolve("../s.json").unwrap(), "file:///p/s.json");
    assert_eq!(resolve("schemas\\a b.json").unwrap(), "file:///p/dir/schemas/a%20b.json");
    assert_eq!(resolve("https://x.org/s.json").unwrap(), "https://x.org/s.json");
    assert!(resolve_location("s.json", None).is_err());
}

fn cargo(src: &str) -> Option<crate::CargoContext> {
    let (doc, offset) = cursor(src);
    crate::cargo_context(&doc, offset)
}

#[test]
fn cargo_contexts() {
    use crate::{CargoQuery, CargoTarget};
    let c = cargo("[dependencies]\nser$0\n").unwrap();
    assert_eq!(c.query, CargoQuery::Search("ser".to_owned()));
    assert_eq!(c.target, CargoTarget::Name { header: false, has_value: false });
    let c = cargo("[workspace.dependencies]\nser$0 = \"1\"\n").unwrap();
    assert_eq!(c.target, CargoTarget::Name { header: false, has_value: true });
    let c = cargo("[dependencies.ser$0]\n").unwrap();
    assert_eq!(c.target, CargoTarget::Name { header: true, has_value: false });

    let c = cargo("[dev-dependencies]\nserde = \"1.$0\"\n").unwrap();
    assert_eq!(
        (c.query, c.target),
        (
            CargoQuery::Versions("serde".to_owned()),
            CargoTarget::Version { name: "serde".to_owned() }
        )
    );
    let c = cargo(
        "[target.'cfg(windows)'.dependencies]\nwin = { package = \"windows\", version = $0 }\n",
    )
    .unwrap();
    assert_eq!(c.query, CargoQuery::Versions("windows".to_owned()));
    let c =
        cargo("[dependencies.serde]\nversion = \"1.0\"\nfeatures = [\"derive\", $0]\n").unwrap();
    assert_eq!(
        c.target,
        CargoTarget::Feature {
            name: "serde".to_owned(),
            requirement: Some("1.0".to_owned()),
            existing: vec!["derive".to_owned()],
            prefix: String::new(),
        }
    );
    assert!(cargo("[package]\nname = $0\n").is_none());
}

#[test]
fn cargo_items() {
    use crate::{CrateVersion, cargo_completions};
    let v = |version: &str, yanked, features: &[&str]| CrateVersion {
        version: version.to_owned(),
        yanked,
        features: features.iter().map(|f| (*f).to_owned()).collect(),
    };
    let versions = [
        v("0.9.0", false, &["old"]),
        v("1.0.0", false, &["std", "derive"]),
        v("1.1.0", true, &[]),
        v("2.0.0-rc.1", false, &[]),
    ];
    let c = cargo("[dependencies]\nserde = $0\n").unwrap();
    let labels: Vec<String> =
        cargo_completions(&c, None, Some(&versions), crate::DependencyStyle::Table, false)
            .into_iter()
            .map(|i| i.insert)
            .collect();
    assert_eq!(labels, ["\"2.0.0-rc.1\"", "\"1.0.0\"", "\"0.9.0\""]);
    let c = cargo("[dependencies]\nserde = { version = \"1\", features = [\"std\", \"$0\"] }\n")
        .unwrap();
    let labels: Vec<String> =
        cargo_completions(&c, None, Some(&versions), crate::DependencyStyle::Table, false)
            .into_iter()
            .map(|i| i.label)
            .collect();
    assert_eq!(labels, ["derive"]);
    assert!(
        crate::cargo::matches_requirement("^0.4", "0.4.9")
            && !crate::cargo::matches_requirement("0.4", "0.5.0")
    );
    assert!(
        crate::cargo::matches_requirement("~1.2.3", "1.2.9")
            && !crate::cargo::matches_requirement("~1.2.3", "1.3.0")
    );
}

/// Applies the fixes of every diagnostic, one at a time.
fn check_fixes(src: &str, expect: Expect) {
    let r = registry();
    let schema = Schema { registry: &r, url: URL };
    let doc = Document::new(src);
    let mut out = String::new();
    for d in diagnostics(&doc, SchemaState::Ready(schema)) {
        for f in &d.fixes {
            let mut text = src.to_owned();
            for (range, new) in f.edits.iter().rev() {
                text.replace_range(usize::from(range.start())..usize::from(range.end()), new);
            }
            let star = if f.preferred { "*" } else { " " };
            out.push_str(&format!("{star}{} => {text:?}\n", f.title));
        }
    }
    expect.assert_eq(&out);
}

#[test]
fn quick_fixes() {
    check_fixes(
        "version = 1\n[package]\nnmae = \"x\" # c\nedition = \"2018\"\n",
        expect![[r#"
            *Convert to the string "1" => "version = \"1\"\n[package]\nnmae = \"x\" # c\nedition = \"2018\"\n"
            *Add `name` => "version = 1\n[package]\nnmae = \"x\" # c\nedition = \"2018\"\nname = \"\"\n"
            *Rename to `name` => "version = 1\n[package]\nname = \"x\" # c\nedition = \"2018\"\n"
             Remove `nmae` => "version = 1\n[package]\nedition = \"2018\"\n"
             Replace with `"2021"` => "version = 1\n[package]\nnmae = \"x\" # c\nedition = \"2021\"\n"
             Replace with `"2024"` => "version = 1\n[package]\nnmae = \"x\" # c\nedition = \"2024\"\n"
        "#]],
    );
    check_fixes(
        "a = word\na = 2\npackage = { publish = \"true\" }\n",
        expect![[r#"
        *Quote as "word" => "a = \"word\"\na = 2\npackage = { publish = \"true\" }\n"
         Remove the duplicate key => "a = word\npackage = { publish = \"true\" }\n"
        *Add `name` => "a = word\na = 2\npackage = { publish = \"true\", name = \"\" }\n"
        *Convert to `true` => "a = word\na = 2\npackage = { publish = true }\n"
    "#]],
    );
}

#[test]
fn cargo_hover_and_links() {
    let (doc, offset) = cursor(
        "[dependencies]\nser$0de = { version = \"1.0\", features = [\"derive\"] }\nlocal = { path = \"../local\" }\nws.workspace = true\n",
    );
    let h = crate::cargo_hover_context(&doc, offset).unwrap();
    assert_eq!(
        crate::cargo_hover_markdown(&h),
        "**serde** `1.0`\n\n[docs.rs](https://docs.rs/serde/1.0) · [crates.io](https://crates.io/crates/serde)"
    );
    let links: Vec<String> = crate::cargo_docs_links(&doc)
        .into_iter()
        .map(|(r, u)| format!("{} {u}", &doc.text[r]))
        .collect();
    assert_eq!(links, ["serde https://docs.rs/serde/1.0"]);
    let (doc, offset) = cursor("[dependencies]\nlo$0cal = { path = \"../local\" }\n");
    assert!(crate::cargo_hover_context(&doc, offset).is_none());
    assert_eq!(crate::cargo_path_dependency(&doc, offset).as_deref(), Some("../local"));
    let paths: Vec<String> = crate::path_strings(&doc).into_iter().map(|p| p.path).collect();
    assert_eq!(paths, ["../local"]);
}

#[test]
fn sort_keys() {
    let r = registry();
    let schema = Schema { registry: &r, url: URL };
    let src = "[package]\npublish = true\n# the name\nname = \"x\"\nother = 1\n\nedition = \"2024\"\n[bin]\n";
    let (doc, offset) = (Document::new(src), TextSize::new(12));
    let fix = crate::sort_keys_fix(&doc, offset, schema).unwrap();
    let mut text = src.to_owned();
    for (range, new) in fix.edits.iter().rev() {
        text.replace_range(usize::from(range.start())..usize::from(range.end()), new);
    }
    assert_eq!(
        text,
        "[package]\n# the name\nname = \"x\"\npublish = true\nother = 1\n\nedition = \"2024\"\n[bin]\n"
    );
    assert!(crate::sort_keys_fix(&Document::new(text.clone()), offset, schema).is_none());
}

#[test]
fn cargo_sort() {
    let src = "[package]\nname = \"x\"\n\n[dependencies]\nserde = { version = \"1\" }\n# local\nhi = { path = \"../hi\" }\n\n# logging\nanyhow = \"1\"\nZed = { path = \"z\" }\n\n[dev-dependencies]\n";
    let doc = Document::new(src);
    let offset = TextSize::new(src.find("serde").unwrap() as u32);
    let all = || {
        crate::cargo_sort_fixes(&doc, offset, &[])
            .into_iter()
            .chain(crate::sort_alphabetically_fix(&doc, offset))
    };
    for fix in all() {
        let mut text = src.to_owned();
        for (range, new) in fix.edits.iter().rev() {
            text.replace_range(usize::from(range.start())..usize::from(range.end()), new);
        }
        let section =
            &text[text.find("[dependencies]\n").unwrap() + 15..text.find("\n[dev").unwrap()];
        match fix.title.as_str() {
            "Sort keys alphabetically" => assert_eq!(
                section,
                "# local\nhi = { path = \"../hi\" }\nserde = { version = \"1\" }\n\n# logging\nanyhow = \"1\"\nZed = { path = \"z\" }\n"
            ),
            "Sort dependencies, local first" => assert_eq!(
                section,
                "# local\nhi = { path = \"../hi\" }\nZed = { path = \"z\" }\n\n# logging\nanyhow = \"1\"\nserde = { version = \"1\" }\n"
            ),
            "Sort dependencies, local last" => assert_eq!(
                section,
                "# logging\nanyhow = \"1\"\nserde = { version = \"1\" }\n\n# local\nhi = { path = \"../hi\" }\nZed = { path = \"z\" }\n"
            ),
            other => panic!("{other}"),
        }
    }
    assert_eq!(all().count(), 3);
}

#[test]
fn cargo_local() {
    let child = Document::new(
        "[package]\nname = \"hi\"\nversion.workspace = true\n[features]\ndefault = [\"std\"]\nstd = []\nserde = [\"dep:serde\"]\n[dependencies]\nserde = { version = \"1\", optional = true }\nlog = { version = \"0.4\", optional = true }\n",
    );
    let ws = Document::new(
        "[workspace]\nmembers = [\"*\"]\n[workspace.package]\nversion = \"0.3.1\"\n[workspace.dependencies]\nhi = { path = \"hi\" }\nserde = \"1\"\n",
    );
    let v = crate::cargo_local_crate(&child, Some(&ws));
    assert_eq!(
        (v.version.as_str(), v.features),
        (
            "0.3.1",
            vec!["default".to_owned(), "log".to_owned(), "serde".to_owned(), "std".to_owned()]
        )
    );
    assert_eq!(crate::cargo_workspace_locals(&ws), Some(vec!["hi".to_owned()]));

    // `workspace = true` users of local crates are local; groups keep their headings.
    let src = "[dependencies]\n# local crates\nzed.workspace = true\nhi.workspace = true\n\n# others\nserde.workspace = true\nanyhow = \"1\"\n";
    let doc = Document::new(src);
    let apply = |title: &str, text: &str| {
        let doc = Document::new(text);
        let fix =
            crate::cargo_sort_fixes(&doc, TextSize::new(16), &["hi".to_owned(), "zed".to_owned()])
                .into_iter()
                .chain(crate::sort_alphabetically_fix(&doc, TextSize::new(16)))
                .find(|f| f.title == title)?;
        let mut out = text.to_owned();
        for (range, new) in fix.edits.iter().rev() {
            out.replace_range(usize::from(range.start())..usize::from(range.end()), new);
        }
        Some(out)
    };
    let alpha = apply("Sort keys alphabetically", src).unwrap();
    assert_eq!(
        alpha,
        "[dependencies]\n# local crates\nhi.workspace = true\nzed.workspace = true\n\n# others\nanyhow = \"1\"\nserde.workspace = true\n"
    );
    // Already grouped: local first changes nothing but the order inside.
    assert_eq!(apply("Sort dependencies, local first", &alpha), None);
    let last = apply("Sort dependencies, local last", &alpha).unwrap();
    assert_eq!(
        last,
        "[dependencies]\n# others\nanyhow = \"1\"\nserde.workspace = true\n\n# local crates\nhi.workspace = true\nzed.workspace = true\n"
    );
    let first = apply("Sort dependencies, local first", &last).unwrap();
    assert_eq!(first, alpha, "no duplicated heading");
    let _ = doc;
}

#[test]
fn cargo_sort_adds_no_heading() {
    let src = "[dependencies]\nserde = \"1\"\nhi = { path = \"hi\" }\n";
    let doc = Document::new(src);
    let fix = crate::cargo_sort_fixes(&doc, TextSize::new(16), &[])
        .into_iter()
        .find(|f| f.title == "Sort dependencies, local first")
        .unwrap();
    assert_eq!(fix.edits[0].1, "hi = { path = \"hi\" }\n\nserde = \"1\"\n");
}

#[test]
fn sort_alphabetically() {
    let src = "[features]\ndefault = []\n# sample\nlog = []\naa = []\n\nzz = []\nbb = []\n";
    let doc = Document::new(src);
    let fix = crate::sort_alphabetically_fix(&doc, TextSize::new(14)).unwrap();
    assert_eq!(fix.edits[0].1, "aa = []\ndefault = []\n# sample\nlog = []\n\nbb = []\nzz = []\n");
    // Before the first header too, its leading comments staying on top.
    let doc = Document::new("#:schema ./s.json\nb = 1\na = 2\n[t]\n");
    let fix = crate::sort_alphabetically_fix(&doc, TextSize::new(20)).unwrap();
    assert_eq!(fix.edits[0].1, "#:schema ./s.json\na = 2\nb = 1\n");
    assert!(
        crate::sort_alphabetically_fix(&Document::new("[t]\na = 1\nb = 2\n"), TextSize::new(5))
            .is_none()
    );
}

#[test]
fn cargo_dependency_styles() {
    use crate::{CrateSummary, DependencyStyle, cargo_completions};
    let found = [CrateSummary {
        name: "serde".to_owned(),
        version: "1.0.219".to_owned(),
        description: None,
    }];
    let c = cargo("[dependencies]\nser$0\n").unwrap();
    let insert = |style, snippets| {
        cargo_completions(&c, Some(&found), None, style, snippets)[0].insert.clone()
    };
    assert_eq!(insert(DependencyStyle::Table, true), "serde = { version = \"1.0.219\"$0 }");
    assert_eq!(insert(DependencyStyle::Table, false), "serde = { version = \"1.0.219\" }");
    assert_eq!(insert(DependencyStyle::String, true), "serde = \"1.0.219\"");

    let convert = |src: &str| {
        let (doc, offset) = cursor(src);
        crate::cargo_dependency_fixes(&doc, toml_parser::TextRange::empty(offset))
            .into_iter()
            .map(|f| {
                let mut text = doc.text.to_string();
                for (range, new) in f.edits.iter().rev() {
                    text.replace_range(usize::from(range.start())..usize::from(range.end()), new);
                }
                format!("{} => {text:?}", f.title)
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        convert("[dependencies]\nse$0rde = \"1.0\"\n"),
        [
            "Convert to `{ version = \"1.0\" }` => \"[dependencies]\\nserde = { version = \\\"1.0\\\" }\\n\""
        ]
    );
    assert_eq!(
        convert("[dependencies]\nserde = { version = \"1$0.0\" }\n"),
        ["Convert to `\"1.0\"` => \"[dependencies]\\nserde = \\\"1.0\\\"\\n\""]
    );
    // Not with other keys than `version`.
    assert!(convert("[dependencies]\nse$0rde = { version = \"1\", features = [] }\n").is_empty());
}

#[test]
fn cargo_dependency_range_fixes() {
    let src = "[dependencies]\na = \"1\"\nb = { version = \"2\" }\nc = \"3\"\nd = { path = \"d\" }\n\n[dev-dependencies]\ne = \"5\"\n";
    let doc = Document::new(src);
    let whole = toml_parser::TextRange::new(TextSize::new(15), TextSize::new(src.len() as u32));
    let fixes = crate::cargo_dependency_fixes(&doc, whole);
    let titles: Vec<&str> = fixes.iter().map(|f| f.title.as_str()).collect();
    assert_eq!(titles, ["Convert 3 dependencies to `{ version = ... }`", "Convert to `\"2\"`"]);
    let mut text = src.to_owned();
    for (range, new) in fixes[0].edits.iter().rev() {
        text.replace_range(usize::from(range.start())..usize::from(range.end()), new);
    }
    assert_eq!(
        text,
        "[dependencies]\na = { version = \"1\" }\nb = { version = \"2\" }\nc = { version = \"3\" }\nd = { path = \"d\" }\n\n[dev-dependencies]\ne = { version = \"5\" }\n"
    );
    // Only the lines selected.
    let first_two = toml_parser::TextRange::new(TextSize::new(15), TextSize::new(30));
    let titles: Vec<String> =
        crate::cargo_dependency_fixes(&doc, first_two).into_iter().map(|f| f.title).collect();
    assert_eq!(titles, ["Convert to `{ version = \"1\" }`", "Convert to `\"2\"`"]);
}

#[test]
fn cargo_workspace_inheritance() {
    let root = Document::new(
        "[workspace.package]\nauthors = [\"SARDONYX\"]\n\n[workspace.dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\n\n[workspace.lints.rust]\nunsafe_code = \"deny\"\n",
    );
    let hover = |src: &str| {
        let (doc, offset) = cursor(src);
        crate::cargo_workspace_ref(&doc, offset).map(|r| crate::cargo_workspace_hover(&r, &root))
    };
    assert_eq!(
        hover("[package]\nauth$0ors.workspace = true\n").unwrap(),
        "From `[workspace.package]` of the workspace:\n\n```toml\nauthors = [\"SARDONYX\"]\n```"
    );
    // On `workspace`, on `true`, and the table form.
    assert!(hover("[package]\nauthors.work$0space = true\n").is_some());
    assert!(hover("[package]\nauthors.workspace = tr$0ue\n").is_some());
    assert!(hover("[package]\nauthors = { workspace = t$0rue }\n").is_some());
    assert_eq!(
        hover("[dev-dependencies]\nse$0rde.workspace = true\n").unwrap(),
        "From `[workspace.dependencies]` of the workspace:\n\n```toml\nserde = { version = \"1\", features = [\"derive\"] }\n```"
    );
    assert_eq!(
        hover("[lints]\nwork$0space = true\n").unwrap(),
        "From `[workspace.lints]` of the workspace:\n\n```toml\n[workspace.lints.rust]\nunsafe_code = \"deny\"\n```"
    );
    assert_eq!(
        hover("[package]\nlic$0ense.workspace = true\n").unwrap(),
        "`license` is not defined in `[workspace.package]` of the workspace"
    );
    assert!(hover("[package]\nna$0me = \"x\"\n").is_none());
    // Go to definition: the key in the root.
    let (doc, offset) = cursor("[dependencies]\nse$0rde.workspace = true\n");
    let r = crate::cargo_workspace_ref(&doc, offset).unwrap();
    assert_eq!(&root.text[root.locate(&r.target, true)], "serde");
}

#[test]
fn cargo_feature_checks() {
    let src = r#"[dependencies]
serde = "1"
tracing = { version = "0.1" }
log.workspace = true
opt = { version = "1", optional = true }

[dependencies.rand]
version = "0.8"

[dev-dependencies]
tokio = "1"

[features]
default = ["std", "stf", "opt", "dep:tracing", "dep:tracng", "dep:log", "dep:rand"]
std = ["serde/derive", "serd/derive", "serde?/std", "tokio/full", "opt?/x"]
"#;
    let doc = Document::new(src);
    let mut out = String::new();
    for d in crate::cargo_feature_diagnostics(&doc) {
        out.push_str(&format!("{:?} {}: {}\n", d.severity, &src[d.range], d.message));
        for f in &d.fixes {
            let mut text = src.to_owned();
            for (range, new) in f.edits.iter().rev() {
                text.replace_range(usize::from(range.start())..usize::from(range.end()), new);
            }
            let changed: Vec<&str> =
                text.lines().filter(|l| !src.lines().any(|s| s == *l)).collect();
            let star = if f.preferred { "*" } else { " " };
            out.push_str(&format!("  {star}{} => {changed:?}\n", f.title));
        }
    }
    expect![[r#"
        Error "stf": `stf` is neither a feature nor an optional dependency, did you mean `std`?
          *Replace with `"std"` => ["default = [\"std\", \"std\", \"opt\", \"dep:tracing\", \"dep:tracng\", \"dep:log\", \"dep:rand\"]"]
        Error "dep:tracing": `dep:tracing` needs `tracing` to be an optional dependency
          *Make `tracing` optional => ["tracing = { version = \"0.1\", optional = true }"]
        Error "dep:tracng": `tracng` is not a dependency, did you mean `tracing`?
          *Replace with `"dep:tracing"` => ["default = [\"std\", \"stf\", \"opt\", \"dep:tracing\", \"dep:tracing\", \"dep:log\", \"dep:rand\"]"]
        Error "dep:log": `dep:log` needs `log` to be an optional dependency
          *Make `log` optional => ["log.optional = true"]
        Error "dep:rand": `dep:rand` needs `rand` to be an optional dependency
          *Make `rand` optional => ["optional = true"]
        Error "serd/derive": `serd` is not a dependency, did you mean `serde`?
          *Replace with `"serde/derive"` => ["std = [\"serde/derive\", \"serde/derive\", \"serde?/std\", \"tokio/full\", \"opt?/x\"]"]
        Warning "serde?/std": `serde` is not optional: `serde?/` is the same as `serde/`
          *Make `serde` optional => ["serde = { version = \"1\", optional = true }"]
        Error "tokio/full": `tokio` is not a dependency
    "#]].assert_eq(&out);
}

#[test]
fn cargo_feature_items() {
    let src = "[dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\nopt = { version = \"1\", optional = true }\nhid = { version = \"1\", optional = true }\n\n[features]\ndefault = [\"std\", \"$0\"]\nstd = [\"dep:hid\"]\nextra = []\n";
    let (doc, offset) = cursor(src);
    let labels: Vec<String> =
        crate::cargo_feature_completions(&doc, offset).into_iter().map(|i| i.insert).collect();
    assert_eq!(labels, ["\"extra\"", "\"opt\"", "\"dep:opt\"", "\"dep:hid\""]);

    let src = "[dependencies]\nserde = { version = \"1\" }\n\n[features]\nstd = [\"serde/std\", \"serde/$0\"]\n";
    let (doc, offset) = cursor(src);
    assert!(crate::cargo_feature_completions(&doc, offset).is_empty());
    let c = crate::cargo_context(&doc, offset).unwrap();
    assert_eq!(c.query, crate::CargoQuery::Versions("serde".to_owned()));
    let versions = [crate::CrateVersion {
        version: "1.0.0".to_owned(),
        yanked: false,
        features: vec!["std".to_owned(), "derive".to_owned()],
    }];
    let items =
        crate::cargo_completions(&c, None, Some(&versions), crate::DependencyStyle::Table, false);
    let inserts: Vec<String> = items.into_iter().map(|i| i.insert).collect();
    assert_eq!(inserts, ["\"serde/derive\""]);

    let (doc, offset) = cursor("[dependencies]\nserde = \"1\"\n[features]\na = [\"serde?/$0\"]\n");
    let c = crate::cargo_context(&doc, offset).unwrap();
    assert!(
        matches!(c.target, crate::CargoTarget::Feature { ref prefix, .. } if prefix == "serde?/")
    );
}
