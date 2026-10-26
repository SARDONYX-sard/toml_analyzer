import * as fs from "node:fs";
import * as vscode from "vscode";
import { LanguageClient, type LanguageClientOptions, type ServerOptions } from "vscode-languageclient/node";

import { serverArgs, serverPath, settings } from "./settings";
import { log, state } from "./state";

/** Starts the language server and connects the client to it. */
export async function startServer(context: vscode.ExtensionContext): Promise<void> {
  const command = serverPath(context);
  if (!fs.existsSync(command)) {
    void vscode.window.showErrorMessage(
      `TOML language server not found: ${command}. Build it with \`cargo build --release\` or set \`toml.server.path\`.`,
    );
    return;
  }

  const args = serverArgs();
  const serverOptions: ServerOptions = {
    run: { command, args },
    debug: { command, args, options: { env: { ...process.env, RUST_LOG: "debug" } } },
  };
  const clientOptions: LanguageClientOptions = {
    documentSelector: [
      { scheme: "file", language: "toml" },
      { scheme: "untitled", language: "toml" },
    ],
    initializationOptions: settings(context),
    outputChannel: state.output,
    synchronize: {
      // Schemas are JSON files: a change drops them from the server at once.
      fileEvents: vscode.workspace.createFileSystemWatcher("**/*.json"),
    },
  };

  const client = new LanguageClient("toml", "TOML Analyzer", serverOptions, clientOptions);
  state.client = client;
  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration("toml") && client.isRunning()) {
        void client.sendNotification("workspace/didChangeConfiguration", { settings: settings(context) });
      }
    }),
    // Installed or removed extensions bring their `tomlValidation`.
    vscode.extensions.onDidChange(() => {
      if (client.isRunning()) {
        void client.sendNotification("workspace/didChangeConfiguration", { settings: settings(context) });
      }
    }),
  );

  try {
    await client.start();
  } catch (error) {
    log(`Failed to start the TOML language server: ${String(error)}`);
    void vscode.window.showErrorMessage("Failed to start the TOML language server. See the output for details.");
  }
}

/** Stops the server, when the extension is deactivated. */
export async function stopServer(): Promise<void> {
  await state.client?.stop();
  state.client = undefined;
}

/** `toml.restartServer`: a new server process. */
export async function restartServer(): Promise<void> {
  const client = state.client;
  if (!client) {
    void vscode.window.showErrorMessage("The TOML language server is not running.");
    return;
  }
  await client.restart();
}
