import * as vscode from "vscode";

import { ServerController } from "./controller.js";

let controller: ServerController | undefined;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  const output = vscode.window.createOutputChannel("AdocWeave", { log: true });
  controller = new ServerController(context, output);
  context.subscriptions.push(
    output,
    vscode.commands.registerCommand("adocweave.restartServer", () => controller?.restart()),
    vscode.commands.registerCommand("adocweave.clearManagedServer", () =>
      controller?.clearManagedServer(),
    ),
    vscode.workspace.onDidChangeConfiguration((event) => {
      if (
        event.affectsConfiguration("adocweave.server.path") ||
        event.affectsConfiguration("adocweave.server.download")
      ) {
        void controller?.restart();
      }
    }),
    vscode.workspace.onDidGrantWorkspaceTrust(() => {
      void controller?.restart();
    }),
  );
  await controller.restart();
}

export async function deactivate(): Promise<void> {
  const active = controller;
  controller = undefined;
  await active?.dispose();
}
