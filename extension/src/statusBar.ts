import * as vscode from "vscode";

import { ClearSchemaCacheRequest, DocumentSchemaRequest, type DocumentSchemaResult, StatusRequest } from "./protocol";
import { NO_SCHEMA, resetSchema, useAnotherSchema, useNoSchema } from "./schemaPicker";
import { log, runningClient, state } from "./state";

let item: vscode.StatusBarItem | undefined;
let current: DocumentSchemaResult | undefined;

/** The schema of the active TOML editor, in the status bar. */
export function createStatusBar(context: vscode.ExtensionContext): void {
  item = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
  item.command = "toml.schemaActions";
  context.subscriptions.push(
    item,
    vscode.window.onDidChangeActiveTextEditor(() => void refreshStatus()),
    // A schema chosen in the settings: the server needs a moment.
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration("toml")) scheduleRefresh();
    }),
    vscode.workspace.onDidChangeTextDocument((e) => {
      if (e.document === vscode.window.activeTextEditor?.document) {
        scheduleRefresh();
      }
    }),
  );
}

let timer: NodeJS.Timeout | undefined;

function scheduleRefresh(): void {
  clearTimeout(timer);
  timer = setTimeout(() => void refreshStatus(), 500);
}

/** Asks the server for the schema of the active editor. */
export async function refreshStatus(): Promise<void> {
  const editor = vscode.window.activeTextEditor;
  const client = runningClient();
  if (!item || !editor || editor.document.languageId !== "toml" || !client) {
    item?.hide();
    return;
  }
  try {
    current = await client.sendRequest(DocumentSchemaRequest, { uri: editor.document.uri.toString() });
  } catch (error) {
    log(`toml/documentSchema failed: ${String(error)}`);
    return;
  }
  if (!current.url && current.associationUrl === NO_SCHEMA) {
    item.text = "$(circle-slash) No schema";
    item.tooltip = `Disabled by toml.schemas: "${current.pattern ?? ""}"`;
  } else if (!current.url) {
    item.text = "$(json) No schema";
    item.tooltip = "No JSON Schema applies to this file";
  } else {
    const name = current.url.replace(/[?#].*$/, "").split("/").pop() ?? current.url;
    const icon = current.status === "ready" ? "$(json)" : current.status === "loading" ? "$(loading~spin)" : "$(error)";
    item.text = `${icon} ${decodeURIComponent(name)}`;
    item.tooltip = `${current.url}\nfrom: ${current.source ?? "?"}\nstatus: ${current.status}`;
    // The server publishes diagnostics when a schema arrives; refresh then too.
    if (current.status === "loading") {
      scheduleRefresh();
    }
  }
  item.show();
}

/** `toml.clearSchemaCache` */
export async function clearSchemaCache(): Promise<void> {
  const client = runningClient();
  if (!client) {
    void vscode.window.showErrorMessage("The TOML language server is not running.");
    return;
  }
  const result = await client.sendRequest(ClearSchemaCacheRequest);
  void vscode.window.setStatusBarMessage(
    `TOML: schema cache cleared (${result.removedFiles} cached files removed)`,
    4000,
  );
  void refreshStatus();
}

/** `toml.showStatus`: the schemas the server knows, in the output channel. */
export async function showStatus(): Promise<void> {
  const client = runningClient();
  if (!client) {
    void vscode.window.showErrorMessage("The TOML language server is not running.");
    return;
  }
  const status = await client.sendRequest(StatusRequest);
  const lines = [
    `toml_analyzer ${status.version}`,
    `cache directory: ${status.cacheDirectory ?? "(disabled)"}`,
    "open documents:",
    ...status.documents.map((d) => `  ${d}`),
    "schemas:",
    ...status.schemas.map(([url, s]) => `  ${url}: ${s}`),
  ];
  state.output?.appendLine(lines.join("\n"));
  state.output?.show(true);
}

/** `toml.schemaActions`: what the status bar item offers. */
export async function schemaActions(): Promise<void> {
  const picks: (vscode.QuickPickItem & { run: () => unknown })[] = [];
  const document = vscode.window.activeTextEditor?.document;
  const shown = current;
  if (shown?.url) {
    const url = shown.url;
    picks.push({ label: "$(go-to-file) Open the schema", detail: url, run: () => openSchema(url) });
  }
  if (document?.languageId === "toml") {
    const uri = document.uri;
    picks.push(
      {
        label: "$(edit) Use another schema…",
        detail: "Wrong schema? Choose one from SchemaStore, a file or a URL, for this file or a glob (saved in toml.schemas)",
        run: () => useAnotherSchema(uri, shown),
      },
      {
        label: "$(circle-slash) Use no schema…",
        detail: "Disable the schema for this file or a glob",
        run: () => useNoSchema(uri, shown),
      },
    );
    if (shown?.source === "settings" && shown.associationUrl && shown.pattern) {
      picks.push({
        label: "$(discard) Reset to automatic",
        detail: `Remove "${shown.associationUrl}": "${shown.pattern}" from toml.schemas`,
        run: () => resetSchema(shown),
      });
    }
  }
  picks.push(
    {
      label: "$(trash) Clear the schema cache",
      detail: "Forget every schema, in memory and on disk, and load them again",
      run: clearSchemaCache,
    },
    { label: "$(info) Show the status", run: showStatus },
  );
  const pick = await vscode.window.showQuickPick(picks, { placeHolder: "JSON Schema" });
  await pick?.run();
}

async function openSchema(url: string): Promise<void> {
  const uri = vscode.Uri.parse(url);
  if (uri.scheme === "file") {
    await vscode.window.showTextDocument(uri);
  } else {
    await vscode.env.openExternal(uri);
  }
}
