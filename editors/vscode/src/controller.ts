import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { join } from "node:path";

import * as vscode from "vscode";
import {
  LanguageClient,
  type LanguageClientOptions,
  type ServerOptions,
  type StreamInfo,
} from "vscode-languageclient/node";

import { configuredServerPath } from "./configuration.js";
import { platformForHost, type ManagedPlatform } from "./platform.js";
import { selectServer, type SelectedServer } from "./server-selection.js";

const STOP_TIMEOUT_MS = 5_000;

function waitForExit(child: ChildProcessWithoutNullStreams, timeout: number): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null) return Promise.resolve();
  return new Promise((resolvePromise) => {
    const timer = setTimeout(resolvePromise, timeout);
    child.once("exit", () => {
      clearTimeout(timer);
      resolvePromise();
    });
  });
}

export class ServerController implements vscode.Disposable {
  readonly #context: vscode.ExtensionContext;
  readonly #output: vscode.LogOutputChannel;
  #abort?: AbortController;
  #child?: ChildProcessWithoutNullStreams;
  #client?: LanguageClient;
  #disposed = false;
  #generation = 0;
  #queue: Promise<void> = Promise.resolve();

  constructor(context: vscode.ExtensionContext, output: vscode.LogOutputChannel) {
    this.#context = context;
    this.#output = output;
  }

  restart(): Promise<void> {
    const generation = ++this.#generation;
    const abort = new AbortController();
    this.#abort?.abort();
    this.#abort = abort;
    this.#queue = this.#queue
      .catch(() => undefined)
      .then(async () => {
        if (abort.signal.aborted) return;
        const selected = await this.#select(generation, abort.signal);
        if (!selected || this.#disposed || generation !== this.#generation) return;
        await this.#stop();
        if (!this.#disposed && generation === this.#generation)
          await this.#start(generation, selected);
      })
      .catch((error: unknown) => {
        if (abort.signal.aborted || generation !== this.#generation || this.#disposed) return;
        const code = error instanceof Error ? error.message : "unknown-error";
        this.#output.appendLine(`Language Serverを起動できません：${code}`);
        void vscode.window.showErrorMessage(`AdocWeave Language Serverを起動できません：${code}`);
      });
    return this.#queue;
  }

  async clearManagedServer(): Promise<void> {
    const { clearManagedServers } = await import("./installer.js");
    ++this.#generation;
    this.#abort?.abort();
    this.#queue = this.#queue
      .catch(() => undefined)
      .then(async () => {
        await this.#stop();
        await clearManagedServers(this.#managedStoragePath());
        this.#output.appendLine("管理対象Language Serverを削除しました。");
      });
    await this.#queue;
    if (!this.#disposed) await this.restart();
  }

  async dispose(): Promise<void> {
    this.#disposed = true;
    ++this.#generation;
    this.#abort?.abort();
    this.#queue = this.#queue.catch(() => undefined).then(() => this.#stop());
    await this.#queue;
  }

  async #select(generation: number, signal: AbortSignal): Promise<SelectedServer | undefined> {
    const configuration = vscode.workspace.getConfiguration("adocweave");
    const inspected = configuration.inspect<string>("server.path");
    const configured = configuredServerPath(inspected, vscode.workspace.isTrusted);
    if (configured.workspaceValueIgnored) {
      this.#output.appendLine("未信頼workspaceのLanguage Server path設定を無視しました。");
    }
    const version = String(this.#context.extension.packageJSON.version);
    let platform: ManagedPlatform | undefined;
    try {
      platform = platformForHost();
    } catch {
      platform = undefined;
    }
    const selected = await selectServer({
      allowDownload: configuration.get<boolean>("server.download", true),
      configuredPath: configured.path,
      installer: {
        signal,
        storagePath: this.#managedStoragePath(),
        version,
      },
      platform,
      version,
      warning: (code) => this.#output.appendLine(`Language Server候補を使用しません：${code}`),
    });
    if (generation !== this.#generation || signal.aborted) return undefined;
    return selected;
  }

  async #start(generation: number, selected: SelectedServer): Promise<void> {
    const serverOptions: ServerOptions = async (): Promise<StreamInfo> => {
      const child = spawn(selected.command, [], {
        env: process.env,
        shell: false,
        stdio: ["pipe", "pipe", "pipe"],
        windowsHide: true,
      });
      this.#child = child;
      let stderrReported = false;
      child.stderr.on("data", () => {
        if (!stderrReported) {
          stderrReported = true;
          this.#output.appendLine("Language Serverがstderrへ出力しました。");
        }
      });
      child.once("error", () => {
        this.#output.appendLine("Language Server processでエラーが発生しました。");
      });
      return { reader: child.stdout, writer: child.stdin };
    };
    const clientOptions: LanguageClientOptions = {
      documentSelector: [
        { language: "asciidoc", scheme: "file" },
        { language: "asciidoc", scheme: "untitled" },
      ],
      outputChannel: this.#output,
    };
    const client = new LanguageClient(
      "adocweave",
      "AdocWeave Language Server",
      serverOptions,
      clientOptions,
    );
    this.#client = client;
    await client.start();
    if (generation !== this.#generation) {
      await this.#stop();
      return;
    }
    this.#output.appendLine(`Language Serverを起動しました（${selected.source}）。`);
  }

  async #stop(): Promise<void> {
    const client = this.#client;
    const child = this.#child;
    this.#client = undefined;
    this.#child = undefined;
    if (client) {
      try {
        await client.stop(STOP_TIMEOUT_MS);
      } catch {
        this.#output.appendLine("Language Serverの正常終了が時間内に完了しませんでした。");
      }
    }
    if (child && child.exitCode === null && child.signalCode === null) {
      child.kill();
      await waitForExit(child, 2_000);
      if (child.exitCode === null && child.signalCode === null) {
        child.kill("SIGKILL");
        await waitForExit(child, 2_000);
      }
      if (child.exitCode === null && child.signalCode === null) {
        throw new Error("language-server-process-did-not-exit");
      }
    }
  }

  #managedStoragePath(): string {
    return join(this.#context.globalStorageUri.fsPath, "servers");
  }
}
