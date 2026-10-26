import * as path from "node:path";
import * as vscode from "vscode";

/** A schema and the files it applies to. */
interface Association {
  url: string;
  fileMatch: string[];
}

/**
 * The `tomlValidation` contributions of the installed extensions, as Even
 * Better TOML reads them: `{ "fileMatch": "...", "url": "./schema.json" }`,
 * with URLs relative to the extension.
 */
function contributedAssociations(): Association[] {
  const out: Association[] = [];
  for (const ext of vscode.extensions.all) {
    const contributions: unknown = ext.packageJSON?.contributes?.tomlValidation;
    if (!Array.isArray(contributions)) {
      continue;
    }
    for (const c of contributions as { fileMatch?: string | string[]; url?: string }[]) {
      if (typeof c.url !== "string" || c.fileMatch === undefined) {
        continue;
      }
      const url = /^[a-z][a-z0-9+.-]+:/i.test(c.url)
        ? c.url
        : vscode.Uri.file(path.join(ext.extensionPath, c.url)).toString();
      out.push({ url, fileMatch: Array.isArray(c.fileMatch) ? c.fileMatch : [c.fileMatch] });
    }
  }
  return out;
}

/** The `toml` settings section, sent to the server. */
export function settings(context: vscode.ExtensionContext): Record<string, unknown> {
  const config = vscode.workspace.getConfiguration("toml");
  const cacheDirectory =
    expandPath(config.get<string>("schema.cacheDirectory", "")) ||
    path.join(context.globalStorageUri.fsPath, "schemas");
  return {
    schemas: config.get<Record<string, string[]>>("schemas", {}),
    associations: contributedAssociations(),
    schemaStore: {
      enable: config.get<boolean>("schemaStore.enable", true),
      url: config.get<string>("schemaStore.url"),
    },
    schema: {
      cache: config.get<boolean>("schema.cache", true),
      cacheDirectory,
      cacheMaxAge: config.get<number>("schema.cacheMaxAge", 86400),
    },
    validation: { enable: config.get<boolean>("validation.enable", true) },
    cargo: { enable: config.get<boolean>("cargo.enable", true) },
    formatter: {
      maxBlankLines: config.get<number>("formatter.maxBlankLines", 2),
      indentTables: config.get<boolean>("formatter.indentTables", false),
      crlf: config.get<boolean>("formatter.crlf", false),
    },
  };
}

/** Expands `~` and `${workspaceFolder}` in a configured path. */
export function expandPath(p: string): string {
  const folder = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? "";
  const home = process.env.HOME ?? process.env.USERPROFILE ?? "";
  return p.replace("${workspaceFolder}", folder).replace(/^~(?=$|[\\/])/, home);
}

/** The server executable: `toml.server.path`, or the bundled one. */
export function serverPath(context: vscode.ExtensionContext): string {
  const configured = vscode.workspace.getConfiguration("toml").get<string>("server.path");
  if (configured) {
    return expandPath(configured);
  }
  const exe = process.platform === "win32" ? "toml_analyzer.exe" : "toml_analyzer";
  return context.asAbsolutePath(path.join("build", exe));
}

/** Extra command line arguments of the server. */
export function serverArgs(): string[] {
  return vscode.workspace.getConfiguration("toml").get<string[]>("server.extraArgs", []);
}
