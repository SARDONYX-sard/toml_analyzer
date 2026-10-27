// `TOML: Format All TOML Files`: formats every TOML file of the workspace
// with the formatter of the server, and saves those that changed.
import { execFile } from "node:child_process";
import * as path from "node:path";
import * as vscode from "vscode";

import { log, runningClient } from "./state";

/** Files written by tools, never formatted. */
const GENERATED = new Set(["cargo.lock", "poetry.lock", "uv.lock", "pdm.lock"]);

/**
 * The TOML files of a git repository, tracked or not, without the ignored
 * ones (`.gitignore`, `.git/info/exclude`, the global excludes). `undefined`
 * outside of a repository or without git.
 */
function gitFiles(folder: vscode.WorkspaceFolder): Promise<vscode.Uri[] | undefined> {
  const args = ["ls-files", "--cached", "--others", "--exclude-standard", "-z", "--", "*.toml"];
  return new Promise((resolve) => {
    execFile("git", args, { cwd: folder.uri.fsPath, maxBuffer: 64 * 1024 * 1024 }, (error, stdout) => {
      if (error) {
        resolve(undefined);
        return;
      }
      const files = stdout.split("\0").filter((f) => f.length > 0);
      resolve(files.map((f) => vscode.Uri.joinPath(folder.uri, f)));
    });
  });
}

/**
 * The TOML files of the workspace: through git when possible, so that
 * `.gitignore` applies; else every file but `files.exclude` and build
 * outputs.
 */
async function tomlFiles(): Promise<vscode.Uri[]> {
  const found = new Map<string, vscode.Uri>();
  for (const folder of vscode.workspace.workspaceFolders ?? []) {
    const files =
      (await gitFiles(folder)) ??
      (await vscode.workspace.findFiles(
        new vscode.RelativePattern(folder, "**/*.toml"),
        "**/{target,node_modules,.git}/**",
      ));
    for (const f of files) found.set(f.toString(), f);
  }
  return [...found.values()]
    .filter((f) => !GENERATED.has(path.basename(f.fsPath).toLowerCase()))
    .sort((a, b) => a.fsPath.localeCompare(b.fsPath));
}

export async function formatAll(): Promise<void> {
  if (!runningClient()) {
    void vscode.window.showErrorMessage("The TOML language server is not running.");
    return;
  }
  const files = await tomlFiles();
  if (files.length === 0) {
    void vscode.window.showInformationMessage("No TOML file in the workspace.");
    return;
  }
  let changed = 0;
  let unchanged = 0;
  const skipped: string[] = [];
  await vscode.window.withProgress(
    { location: vscode.ProgressLocation.Notification, title: "Formatting TOML files", cancellable: true },
    async (progress, token) => {
      for (const uri of files) {
        if (token.isCancellationRequested) break;
        progress.report({ message: vscode.workspace.asRelativePath(uri), increment: 100 / files.length });
        try {
          const document = await vscode.workspace.openTextDocument(uri);
          const editorConfig = vscode.workspace.getConfiguration("editor", document);
          const options: vscode.FormattingOptions = {
            tabSize: editorConfig.get<number>("tabSize", 2),
            insertSpaces: editorConfig.get<boolean>("insertSpaces", true),
          };
          const edits = await vscode.commands.executeCommand<vscode.TextEdit[] | undefined>(
            "vscode.executeFormatDocumentProvider",
            uri,
            options,
          );
          if (!edits || edits.length === 0) {
            // Syntax errors leave the file as it is, without edits.
            unchanged += 1;
            continue;
          }
          const edit = new vscode.WorkspaceEdit();
          edit.set(uri, edits);
          if (!(await vscode.workspace.applyEdit(edit)) || !(await document.save())) {
            skipped.push(vscode.workspace.asRelativePath(uri));
            continue;
          }
          changed += 1;
        } catch {
          skipped.push(vscode.workspace.asRelativePath(uri));
        }
      }
    },
  );
  // In the status bar for a few seconds: no notification to close.
  const parts = [`${changed} formatted`, `${unchanged} unchanged`];
  if (skipped.length > 0) {
    parts.push(`${skipped.length} skipped (see the output)`);
    log(`Format All: skipped ${skipped.join(", ")}`);
  }
  vscode.window.setStatusBarMessage(`TOML: ${parts.join(", ")}`, 5000);
}
