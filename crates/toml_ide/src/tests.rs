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
            existing: vec!["derive".to_owned()]
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
        cargo_completions(&c, None, Some(&versions)).into_iter().map(|i| i.insert).collect();
    assert_eq!(labels, ["\"2.0.0-rc.1\"", "\"1.0.0\"", "\"0.9.0\""]);
    let c = cargo("[dependencies]\nserde = { version = \"1\", features = [\"std\", \"$0\"] }\n")
        .unwrap();
    let labels: Vec<String> =
        cargo_completions(&c, None, Some(&versions)).into_iter().map(|i| i.label).collect();
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
