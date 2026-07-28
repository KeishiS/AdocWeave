import { isAbsolute } from "node:path";

import { findVerifiedCache, installManagedServer, type InstallerOptions } from "./installer.js";
import { findOnPath } from "./path-search.js";
import type { ManagedPlatform } from "./platform.js";
import { requireCompatibleServer } from "./version.js";

export type ServerSource = "configured" | "managed-cache" | "managed-download" | "path";

export interface SelectedServer {
  readonly command: string;
  readonly source: ServerSource;
}

export interface SelectionOptions {
  readonly allowDownload: boolean;
  readonly configuredPath?: string;
  readonly installer: InstallerOptions;
  readonly platform?: ManagedPlatform;
  readonly version: string;
  readonly warning?: (code: string) => void;
}

export interface SelectionDependencies {
  readonly findOnPath: typeof findOnPath;
  readonly findVerifiedCache: typeof findVerifiedCache;
  readonly installManagedServer: typeof installManagedServer;
  readonly requireCompatibleServer: typeof requireCompatibleServer;
}

const defaultDependencies: SelectionDependencies = {
  findOnPath,
  findVerifiedCache,
  installManagedServer,
  requireCompatibleServer,
};

export async function selectServer(
  options: SelectionOptions,
  dependencies: SelectionDependencies = defaultDependencies,
): Promise<SelectedServer> {
  if (options.configuredPath) {
    if (!isAbsolute(options.configuredPath)) throw new Error("configured-server-path-not-absolute");
    await dependencies.requireCompatibleServer(options.configuredPath, options.version);
    return { command: options.configuredPath, source: "configured" };
  }

  const pathCandidate = await dependencies.findOnPath("adocweave-lsp");
  if (pathCandidate) {
    try {
      await dependencies.requireCompatibleServer(pathCandidate, options.version);
      return { command: pathCandidate, source: "path" };
    } catch {
      options.warning?.("path-server-incompatible");
    }
  }

  if (!options.platform) throw new Error("managed-platform-unsupported");
  const cached = await dependencies.findVerifiedCache(
    options.installer.storagePath,
    options.version,
    options.platform,
  );
  if (cached) {
    await dependencies.requireCompatibleServer(cached, options.version);
    return { command: cached, source: "managed-cache" };
  }
  if (!options.allowDownload) throw new Error("managed-download-disabled");

  const installed = await dependencies.installManagedServer(options.platform, options.installer);
  await dependencies.requireCompatibleServer(installed, options.version);
  return { command: installed, source: "managed-download" };
}
