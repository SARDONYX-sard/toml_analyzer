use expect_test::{Expect, expect};
use serde_json::json;
use toml_dom::{Dom, display_path};

use crate::{Registry, SchemaInfo, key_completions, schemas_at, validate};

const URL: &str = "file:///project/schema.json";

fn errors(registry: &Registry, src: &str) -> String {
    let parse = toml_parser::parse(src);
    let dom = Dom::build(&parse);
    let root = registry.root(URL).expect("schema loaded");
    let mut out = String::new();
    for e in validate(registry, &root, &dom.root, &["$schema"]) {
        out.push_str(&format!(
            "{} {:?} {:?}: {}\n",
            display_path(&e.path),
            e.target,
            e.severity,
            e.message
        ));
    }
    out
}

fn check(schema: serde_json::Value, src: &str, expect: Expect) {
    let mut registry = Registry::new();
    registry.insert(URL, schema);
    expect.assert_eq(&errors(&registry, src));
}

#[test]
fn objects() {
    check(
        json!({
            "type": "object",
            "required": ["name", "version"],
            "properties": {
                "name": {"type": "string", "minLength": 2},
                "edition": {"enum": ["2021", "2024"]},
                "old": {"deprecated": true, "description": "Use `new`."},
            },
            "additionalProperties": false,
        }),
        "\"$schema\" = \"./schema.json\"\nname = \"x\"\nedition = \"2018\"\nnmae = 1\nold = 1\n",
        expect![[r#"
             Value Error: missing required key `version`
            name Value Error: expected at least 2 characters, found 1
            edition Value Error: expected one of `"2021"`, `"2024"`
            nmae Key Error: unknown key `nmae`, did you mean `name`?
            old Key Warning: `old` is deprecated: Use `new`.
        "#]],
    );
}

#[test]
fn refs_and_alternatives() {
    check(
        json!({
            "$defs": {
                "dep": {
                    "anyOf": [
                        {"type": "string", "pattern": "^\\d+\\.\\d+$"},
                        {"type": "object", "properties": {"version": {"$ref": "#/$defs/dep"}}, "additionalProperties": false},
                    ],
                },
            },
            "properties": {
                "deps": {"additionalProperties": {"$ref": "#/$defs/dep"}},
                "when": {"type": "string", "format": "date-time"},
                "ports": {"type": "array", "items": {"type": "integer", "maximum": 65535}, "uniqueItems": true},
            },
        }),
        "deps.a = \"1.0\"\ndeps.b = \"x\"\ndeps.c = {version = \"1.0\", path = \"p\"}\ndeps.d = 1\nwhen = 1979-05-27\nports = [80, 80, 70000]\n",
        expect![[r#"
            deps.b Value Error: does not match the pattern `^\d+\.\d+$`
            deps.c.path Key Error: unknown key `path`
            deps.d Value Error: expected a string or a table, found an integer
            when Value Error: invalid date-time: expected an offset date-time
            ports[1] Value Error: items must be unique: this item repeats item 0
            ports[2] Value Error: must be at most 65535
        "#]],
    );
}

#[test]
fn remote_refs_and_reload() {
    let mut registry = Registry::new();
    registry.insert(URL, json!({"properties": {"a": {"$ref": "./defs.json#/$defs/a"}}}));
    assert_eq!(registry.missing(URL), ["file:///project/defs.json"]);
    // Missing documents are skipped meanwhile.
    assert_eq!(errors(&registry, "a = 1\n"), "");

    registry.insert("file:///project/defs.json", json!({"$defs": {"a": {"type": "string"}}}));
    assert!(registry.missing(URL).is_empty());
    assert_eq!(
        errors(&registry, "a = 1\n"),
        "a Value Error: expected a string, found an integer\n"
    );

    // The file changed: the old document must not be used anymore.
    registry.insert("file:///project/defs.json", json!({"$defs": {"a": {"type": "integer"}}}));
    assert_eq!(errors(&registry, "a = 1\n"), "");
    assert!(registry.remove("file:///project/defs.json"));
    assert_eq!(registry.missing(URL), ["file:///project/defs.json"]);
}

#[test]
fn lookup() {
    let mut registry = Registry::new();
    registry.insert(
        URL,
        json!({
            "properties": {
                "package": {"$ref": "#/$defs/package"},
                "bin": {"type": "array", "items": {"properties": {"name": {"description": "The binary."}}}},
            },
            "$defs": {"package": {"required": ["name"], "properties": {"name": {"type": "string", "title": "Name"}, "edition": {"enum": ["2021", "2024"]}}}},
        }),
    );
    let root = registry.root(URL).unwrap();
    let path = |p: &[&str]| -> Vec<toml_dom::PathSegment> {
        p.iter()
            .map(|s| match s.parse() {
                Ok(i) => toml_dom::PathSegment::Index(i),
                Err(_) => toml_dom::PathSegment::Key((*s).to_owned()),
            })
            .collect()
    };
    let keys: Vec<(String, bool)> =
        key_completions(&schemas_at(&registry, &root, &path(&["package"])))
            .into_iter()
            .map(|k| (k.key, k.required))
            .collect();
    assert_eq!(keys, [("name".to_owned(), true), ("edition".to_owned(), false)]);
    let info = SchemaInfo::of(&schemas_at(&registry, &root, &path(&["bin", "0", "name"])));
    assert_eq!(info.description.as_deref(), Some("The binary."));
    let info = SchemaInfo::of(&schemas_at(&registry, &root, &path(&["package", "edition"])));
    assert_eq!(info.values, [json!("2021"), json!("2024")]);
}

#[test]
fn deprecation() {
    let schema = json!({"properties": {
        "authors": {"deprecated": true, "description": "The authors.\n\nShown on crates.io."},
        "old": {"deprecated": true, "description": "An old key. Deprecated: renamed to `new-key`."},
        "legacy": {"deprecated": true, "deprecationMessage": "Use 'modern' instead."},
    }});
    let mut registry = Registry::new();
    registry.insert(URL, schema);
    let dom = Dom::build(&toml_parser::parse("authors = []\nold = 1\nlegacy = 1\n"));
    let root = registry.root(URL).unwrap();
    let found: Vec<String> = validate(&registry, &root, &dom.root, &[])
        .into_iter()
        .map(|e| format!("{}: {:?}", e.message, e.suggestions))
        .collect();
    expect![[r#"
        [
            "`authors` is deprecated: [RemoveKey]",
            "`old` is deprecated: renamed to `new-key`.: [RenameKey(\"new-key\"), RemoveKey]",
            "`legacy` is deprecated: Use 'modern' instead.: [RenameKey(\"modern\"), RemoveKey]",
        ]
    "#]]
    .assert_debug_eq(&found);
}
