// The protocol extensions of the server, see crates/toml_analyzer/src/ext.rs.
import { RequestType, RequestType0, type TextDocumentIdentifier } from "vscode-languageclient/node";

export interface ClearSchemaCacheResult {
  /** The cache files removed. */
  removedFiles: number;
}

/** `toml/clearSchemaCache`: forgets every schema, in memory and on disk. */
export const ClearSchemaCacheRequest = new RequestType0<ClearSchemaCacheResult, void>("toml/clearSchemaCache");

export interface DocumentSchemaResult {
  url?: string;
  /** `directive`, `key`, `settings`, `extension` or `catalog`. */
  source?: string;
  /** `none`, `ready`, `loading` or `failed: ...`. */
  status: string;
}

/** `toml/documentSchema`: the schema of a document. */
export const DocumentSchemaRequest = new RequestType<TextDocumentIdentifier, DocumentSchemaResult, void>(
  "toml/documentSchema",
);

export interface StatusResult {
  version: string;
  cacheDirectory?: string;
  documents: string[];
  /** URL and state of every schema document. */
  schemas: [string, string][];
}

/** `toml/status`: what the server knows. */
export const StatusRequest = new RequestType0<StatusResult, void>("toml/status");
