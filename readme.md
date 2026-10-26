# TOML Analyzer

A language server for TOML with JSON Schema support, and its VS Code extension.

## Features

- Syntax errors with resilient parsing (TOML 1.1, which accepts every 1.0 document)
- TOML semantic errors: duplicate keys, tables defined twice, dotted keys extending
  tables of headers, appending to static arrays, ...
- JSON Schema validation (drafts 4, 6, 7, 2019-09 and 2020-12), completion of keys,
  table names and values, and hover documentation from the schema
- In `Cargo.toml`: completion of dependency names, versions and features from
  crates.io (`toml.cargo.enable`)
- Formatting, document symbols, folding, semantic highlighting

## Schemas

The schema of a document is, in this order:

1. a `#:schema <url or path>` comment before the first table;
2. a root `"$schema" = "<url or path>"` key;
3. the `toml.schemas` setting: `{ "<url or path>": ["<glob>", ...] }`;
4. the `tomlValidation` contributions of installed extensions;
5. the [SchemaStore](https://www.schemastore.org) catalog (`toml.schemaStore.enable`).

Paths may be absolute or relative to the TOML file (`./schema.json`,
`../schemas/app.json`); URLs may be `file://`, `http://` or `https://`.
`$ref`s to other documents are followed.

### Caching

- Local schema files are never cached on disk. The server remembers when it
  read each one and reads it again as soon as it changes (on a file watcher
  event, or on the next edit of a TOML document), so an edited schema is used
  at once, including the files it references.
- Schemas fetched over HTTP are cached on disk for `toml.schema.cacheMaxAge`
  seconds (a stale copy is used offline).
- `TOML: Clear Schema Cache` (or the status bar item) forgets every schema,
  in memory and on disk, and loads them again.

## Command line

```text
toml_analyzer fmt [--check] <files>
toml_analyzer check <files>
```

## Development

```text
cargo test
npm install
npm run debug      # then F5 in VS Code
npm run package    # build/toml-analyzer.vsix
```
