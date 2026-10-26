import * as vscode from "vscode";
import { State } from "vscode-languageclient/node";

import { restartServer, startServer, stopServer } from "./server";
import { state } from "./state";
import { clearSchemaCache, createStatusBar, refreshStatus, schemaActions, showStatus } from "./statusBar";

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  state.context = context;
  state.output = vscode.window.createOutputChannel("TOML Analyzer", { log: true });
  context.subscriptions.push(state.output);
  createStatusBar(context);

  context.subscriptions.push(
    vscode.commands.registerCommand("toml.restartServer", restartServer),
    vscode.commands.registerCommand("toml.clearSchemaCache", clearSchemaCache),
    vscode.commands.registerCommand("toml.showStatus", showStatus),
    vscode.commands.registerCommand("toml.schemaActions", schemaActions),
  );

  await startServer(context);
  if (state.client) {
    context.subscriptions.push(
      state.client.onDidChangeState((e) => {
        if (e.newState === State.Running) {
          void refreshStatus();
        }
      }),
    );
    void refreshStatus();
  }
}

export async function deactivate(): Promise<void> {
  await stopServer();
}
