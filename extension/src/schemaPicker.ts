// Choosing the schema of a file by hand, when the automatic one is wrong.
//
// The choice is written to `toml.schemas` (the workspace settings, or the
// user settings without a workspace): `{ "<url or path>": ["<glob>", ...] }`,
// which users can also edit by hand. `"none"` as the URL means no schema.
import * as path from "node:path";
import * as vscode from "vscode";

import { type CatalogSchema, type DocumentSchemaResult, SchemaCatalogRequest } from "./protocol";
import { runningClient } from "./state";

type Associations = Record<string, string[]>;

/** The URL of `toml.schemas` meaning "no schema". */
export const NO_SCHEMA = "none";

/** Where `toml.schemas` is written, and its current value there. */
function target(): { target: vscode.ConfigurationTarget; value: Associations } {
  const inspect = vscode.workspace.getConfiguration("toml").inspect<Associations>("schemas");
  if (vscode.workspace.workspaceFolders?.length) {
    return { target: vscode.ConfigurationTarget.Workspace, value: { ...(inspect?.workspaceValue ?? {}) } };
  }
  return { target: vscode.ConfigurationTarget.Global, value: { ...(inspect?.globalValue ?? {}) } };
}

/** `file` relative to the first workspace folder, which the server resolves paths against. */
function workspaceRelative(file: string): string | undefined {
  const root = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  if (!root) return undefined;
  const rel = path.relative(root, file);
  if (rel.startsWith("..") || path.isAbsolute(rel)) return undefined;
  // `./`: a pattern with a `/` is a path, not a file name.
  return `./${rel.split(path.sep).join("/")}`;
}

/** Writes `url` for `pattern`, removing the pattern from other schemas. */
async function associate(url: string, pattern: string): Promise<void> {
  const { target: where, value } = target();
  for (const [key, patterns] of Object.entries(value)) {
    const kept = patterns.filter((p) => p !== pattern);
    if (kept.length > 0) value[key] = kept;
    else delete value[key];
  }
  value[url] = [...(value[url] ?? []), pattern];
  await vscode.workspace.getConfiguration("toml").update("schemas", value, where);
}

/** Asks which files the choice applies to. */
async function chooseFiles(document: vscode.Uri): Promise<string | undefined> {
  const name = path.basename(document.fsPath);
  const relative = workspaceRelative(document.fsPath);
  const items: (vscode.QuickPickItem & { pattern?: string; custom?: boolean })[] = [
    {
      label: "$(file) This file only",
      description: relative ?? document.fsPath,
      pattern: relative ?? document.fsPath.split(path.sep).join("/"),
    },
    { label: `$(files) Every file named ${name}`, description: name, pattern: name },
    { label: "$(regex) Files matching a glob…", custom: true },
  ];
  const pick = await vscode.window.showQuickPick(items, { placeHolder: "Apply to" });
  if (!pick) return undefined;
  if (!pick.custom) return pick.pattern;
  return vscode.window.showInputBox({
    prompt: "A glob: without `/` it matches file names, else paths relative to the workspace folder",
    value: `**/${name}`,
  });
}

/** The SchemaStore catalog, waiting while the server fetches it. */
async function catalog(): Promise<CatalogSchema[]> {
  const client = runningClient();
  if (!client) return [];
  return vscode.window.withProgress(
    { location: vscode.ProgressLocation.Window, title: "Loading the schema catalog" },
    async () => {
      for (let i = 0; i < 40; i++) {
        const result = await client.sendRequest(SchemaCatalogRequest);
        if (!result.loading) return result.schemas;
        await new Promise((r) => setTimeout(r, 500));
      }
      return [];
    },
  );
}

/** Asks for a schema: a local file, a URL or path, or one of the catalog. */
async function chooseSchema(): Promise<string | undefined> {
  type Item = vscode.QuickPickItem & { url?: string; action?: "file" | "input" };
  const items: Item[] = [
    { label: "$(folder-opened) Local schema file…", action: "file" },
    { label: "$(link) URL or path…", action: "input" },
  ];
  const schemas = await catalog();
  // The server lists the schemas of TOML files first.
  let group: boolean | undefined;
  for (const s of schemas) {
    if (s.toml !== group) {
      group = s.toml;
      const label = s.toml ? "SchemaStore: TOML files" : "SchemaStore: other files";
      items.push({ label, kind: vscode.QuickPickItemKind.Separator });
    }
    items.push({ label: s.name, description: s.url, detail: s.description, url: s.url });
  }
  const pick = await vscode.window.showQuickPick(items, {
    placeHolder: "The schema of the file",
    matchOnDescription: true,
    matchOnDetail: true,
  });
  if (!pick) return undefined;
  if (pick.action === "file") {
    const files = await vscode.window.showOpenDialog({ filters: { "JSON Schema": ["json"] }, canSelectMany: false });
    const file = files?.[0]?.fsPath;
    return file && (workspaceRelative(file) ?? file);
  }
  if (pick.action === "input") {
    return vscode.window.showInputBox({
      prompt: "A schema URL (https://, file://) or a path, absolute or relative to the workspace folder",
    });
  }
  return pick.url;
}

/** Explains that the file names its schema itself, which settings cannot override. */
function namedInFile(current: DocumentSchemaResult | undefined): boolean {
  if (current?.source !== "directive" && current?.source !== "key") return false;
  void vscode.window.showInformationMessage(
    "This file names its schema itself (`#:schema` or `$schema`), which wins over the settings: edit it there.",
  );
  return true;
}

/** `Use another schema…` */
export async function useAnotherSchema(document: vscode.Uri, current?: DocumentSchemaResult): Promise<void> {
  if (namedInFile(current)) return;
  const url = await chooseSchema();
  if (!url) return;
  const pattern = await chooseFiles(document);
  if (!pattern) return;
  await associate(url, pattern);
}

/** `Use no schema` */
export async function useNoSchema(document: vscode.Uri, current?: DocumentSchemaResult): Promise<void> {
  if (namedInFile(current)) return;
  const pattern = await chooseFiles(document);
  if (!pattern) return;
  await associate(NO_SCHEMA, pattern);
}

/** `Reset to automatic`: removes the association of the settings that applies. */
export async function resetSchema(current: DocumentSchemaResult): Promise<void> {
  const { associationUrl, pattern } = current;
  if (!associationUrl || !pattern) return;
  const config = vscode.workspace.getConfiguration("toml");
  const inspect = config.inspect<Associations>("schemas");
  const scopes: [vscode.ConfigurationTarget, Associations | undefined][] = [
    [vscode.ConfigurationTarget.Workspace, inspect?.workspaceValue],
    [vscode.ConfigurationTarget.Global, inspect?.globalValue],
  ];
  for (const [where, value] of scopes) {
    const patterns = value?.[associationUrl];
    if (!value || !patterns?.includes(pattern)) continue;
    const next = { ...value };
    const kept = patterns.filter((p) => p !== pattern);
    if (kept.length > 0) next[associationUrl] = kept;
    else delete next[associationUrl];
    await config.update("schemas", next, where);
    return;
  }
  void vscode.window.showInformationMessage(
    `The association comes from other settings (\`${associationUrl}\`: \`${pattern}\`): remove it there.`,
  );
}
