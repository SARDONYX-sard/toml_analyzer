# TOML Analyzer

TOML language server + VS Code extension + JSON Schema

- For: Rust users (`Cargo.toml`, `clippy.toml`, `rustfmt.toml`, `rust-toolchain.toml`, ...)
- Why: [Tombi](https://github.com/tombi-toml/tombi) few quick fixes

## Features

| Feature                       | Example                                                           |
| ----------------------------- | ----------------------------------------------------------------- |
| Syntax check (TOML 1.1 / 1.0) | `a =` → error: `expected a value`                                 |
| TOML rules                    | `a = 1` `a = 2` → error: `the key a is defined twice`             |
| Schema check                  | `edition = "2030"` → error: `expected one of "2015", ..., "2024"` |
| Completion                    | key / `[table]` / value (`enum`, `default`, `true` / `false`)     |
| Hover                         | schema `title` + `description` + `default`                        |
| Format                        | `a   =1` → `a = 1`                                                |
| Format all                    | `TOML: Format All TOML Files` (skip: `*.lock`, `.gitignore`)      |
| Outline / folding / highlight | yes                                                               |
| Path link                     | `"./src/main.rs"` → Ctrl+click / F12                              |

### Quick fixes

| Before                         | After                                 |
| ------------------------------ | ------------------------------------- |
| `nmae = "x"`                   | `name = "x"` (did you mean)           |
| `unknown = 1`                  | (remove)                              |
| `authors = [...]` (deprecated) | (remove) / new key                    |
| (missing `name`)               | `name = ""`                           |
| `edition = "2018"`             | `edition = "2021"` / `"2024"`         |
| `port = "80"`                  | `port = 80`                           |
| `name = 1`                     | `name = "1"`                          |
| `a = word`                     | `a = "word"`                          |
| `b = 1` / `a = 1`              | `a = 1` / `b = 1` (A→Z, schema order) |

### `Cargo.toml`

|                                             |                                               |
| ------------------------------------------- | --------------------------------------------- |
| `ser` + completion                          | `serde = { version = "1.0.219" }` (crates.io) |
| `features = ["` + completion                | `"derive"`, `"std"`, ...                      |
| `serde = "1"` ⇄ `serde = { version = "1" }` | quick fix (selection → all)                   |
| hover `serde`                               | link: docs.rs / crates.io                     |
| F12 `local = { path = "../local" }`         | → `../local/Cargo.toml`                       |
| `path` deps: `version` / `features`         | ← `../local/Cargo.toml`                       |
| `[dependencies]`                            | A→Z / local first / local last                |

## Schema

Priority: 1 > 2 > ... > 6

| #   | Source                                     | Example                                                             |
| --- | ------------------------------------------ | ------------------------------------------------------------------- |
| 1   | comment (before the first `[table]`)       | `#:schema ./schema.json`                                            |
| 2   | root key                                   | `"$schema" = "https://example.com/schema.json"`                     |
| 3   | `toml.schemas` (settings)                  | `{ "./schemas/app.json": ["config/*.toml"], "none": ["./x.toml"] }` |
| 4   | `tomlValidation` (other extensions)        |                                                                     |
| 5   | built-in                                   | `clippy.toml`, `.clippy.toml` ← `cargo clippy --version`            |
| 6   | [SchemaStore](https://www.schemastore.org) | `Cargo.toml`, `pyproject.toml`, ...                                 |

- Path: `./a.json`, `../b.json`, `C:\c.json`, `/d.json`
- URL: `file://`, `http://`, `https://`
- `$ref` → other files
- Status bar → `Use another schema…` / `Use no schema…` / `Reset to automatic` → `toml.schemas`

### Cache

| Schema       | Disk cache                             | Update                                  |
| ------------ | -------------------------------------- | --------------------------------------- |
| local file   | no                                     | file changed → reload                   |
| `http(s)://` | yes, `toml.schema.cacheMaxAge` (1 day) | offline → old copy                      |
| all          |                                        | `TOML: Clear Schema Cache` → remove all |

## Settings

| Key                                                      | Default                 |
| -------------------------------------------------------- | ----------------------- |
| `toml.schemas`                                           | `{}`                    |
| `toml.schemaStore.enable`                                | `true`                  |
| `toml.schema.cache` / `cacheMaxAge`                      | `true` / `86400`        |
| `toml.validation.enable`                                 | `true`                  |
| `toml.cargo.enable`                                      | `true`                  |
| `toml.cargo.dependencyStyle`                             | `"table"` (`"string"`)  |
| `toml.cargo.versionCompletion`                           | `false`                 |
| `toml.formatter.alignEntries` / `alignComments`          | `false` / `false`       |
| `toml.formatter.indentTables` / `maxBlankLines` / `crlf` | `false` / `2` / `false` |

```toml
# alignEntries + alignComments
a        = 1   # one
long_key = "x" # two
```

## CLI

```sh
toml_analyzer fmt [--check] <files>
toml_analyzer check <files>   # syntax + TOML + schema
```

## Development

```sh
cargo test
npm install
npm run debug           # → F5
npm run package         # → build/toml-analyzer.vsix
npm run release:patch   # 0.1.0 → 0.1.1 + signed commit + tag (minor / major)
node tasks.cjs icon     # resources/icon.svg → icon.png
```
