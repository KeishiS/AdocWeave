import { createHash, randomUUID } from "node:crypto";
import { constants, type Dirent } from "node:fs";
import {
  access,
  mkdir,
  open,
  readFile,
  readdir,
  rename,
  rm,
  stat,
  writeFile,
} from "node:fs/promises";
import { basename, join, parse, resolve } from "node:path";

import { unzipSync } from "fflate";

import {
  parseDistributionManifest,
  selectLspAsset,
  type DistributionAsset,
} from "./distribution-manifest.js";
import type { ManagedPlatform } from "./platform.js";

const REPOSITORY = "KeishiS/adocweave";
const MANIFEST_NAME = "adocweave-dist-manifest.json";
const MAX_MANIFEST_BYTES = 1024 * 1024;
const MAX_ARCHIVE_BYTES = 64 * 1024 * 1024;
const MAX_DECOMPRESSED_BYTES = 128 * 1024 * 1024;
const MAX_BINARY_BYTES = 64 * 1024 * 1024;
const LOCK_STALE_MS = 5 * 60 * 1_000;
const LOCK_WAIT_MS = 15_000;
const DOWNLOAD_TIMEOUT_MS = 30_000;
const OWNER_MARKER = ".adocweave-vscode-managed-cache";
const OWNER_MARKER_CONTENT = "adocweave-vscode-managed-cache-v1\n";

interface CacheMarker {
  readonly asset: string;
  readonly assetByteSize: number;
  readonly assetSha256: string;
  readonly binarySha256: string;
  readonly packageVersion: string;
  readonly schemaVersion: 1;
  readonly sourceCommit: string;
  readonly target: string;
}

export interface InstallerOptions {
  readonly fetcher?: typeof fetch;
  readonly signal?: AbortSignal;
  readonly storagePath: string;
  readonly version: string;
}

function sha256(bytes: Uint8Array): string {
  return createHash("sha256").update(bytes).digest("hex");
}

function releaseUrl(version: string, name: string): URL {
  return new URL(
    `https://github.com/${REPOSITORY}/releases/download/v${encodeURIComponent(version)}/${encodeURIComponent(name)}`,
  );
}

function trustedResponseUrl(value: string): boolean {
  const url = new URL(value);
  return (
    url.protocol === "https:" &&
    (url.hostname === "github.com" ||
      url.hostname === "objects.githubusercontent.com" ||
      url.hostname === "release-assets.githubusercontent.com")
  );
}

async function download(
  url: URL,
  fetcher: typeof fetch,
  signal: AbortSignal | undefined,
  expectedBytes?: number,
  maximumBytes = MAX_ARCHIVE_BYTES,
): Promise<Uint8Array> {
  const timeout = AbortSignal.timeout(DOWNLOAD_TIMEOUT_MS);
  const combined = signal ? AbortSignal.any([signal, timeout]) : timeout;
  const response = await fetcher(url, {
    headers: { Accept: "application/octet-stream" },
    redirect: "follow",
    signal: combined,
  });
  if (!response.ok || !trustedResponseUrl(response.url)) {
    throw new Error("managed-download-failed");
  }
  const declared = Number(response.headers.get("content-length"));
  if (
    (Number.isFinite(declared) && declared > maximumBytes) ||
    (expectedBytes !== undefined && Number.isFinite(declared) && declared !== expectedBytes)
  ) {
    throw new Error("managed-download-size-mismatch");
  }
  if (expectedBytes !== undefined && expectedBytes > maximumBytes) {
    throw new Error("managed-download-size-mismatch");
  }
  if (!response.body) throw new Error("managed-download-failed");
  const chunks: Uint8Array[] = [];
  let byteLength = 0;
  const reader = response.body.getReader();
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      byteLength += value.byteLength;
      if (
        byteLength > maximumBytes ||
        (expectedBytes !== undefined && byteLength > expectedBytes)
      ) {
        await reader.cancel();
        throw new Error("managed-download-size-mismatch");
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  if (byteLength === 0 || (expectedBytes !== undefined && byteLength !== expectedBytes)) {
    throw new Error("managed-download-size-mismatch");
  }
  const bytes = new Uint8Array(byteLength);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return bytes;
}

function safeArchiveName(name: string): boolean {
  const normalized = name.replaceAll("\\", "/").replace(/\/+$/, "");
  return (
    normalized.length > 0 &&
    !normalized.startsWith("/") &&
    !normalized.includes(":") &&
    normalized
      .split("/")
      .every((component) => component !== "" && component !== "." && component !== "..")
  );
}

export function extractManagedBinary(archive: Uint8Array, asset: DistributionAsset): Uint8Array {
  let expectedEntries = 0;
  let decompressedBytes = 0;
  const seen = new Set<string>();
  const files = unzipSync(archive, {
    filter(entry) {
      if (!safeArchiveName(entry.name)) throw new Error("managed-archive-unsafe-path");
      const folded = entry.name.replaceAll("\\", "/").toLocaleLowerCase("en-US");
      if (seen.has(folded)) throw new Error("managed-archive-duplicate-path");
      seen.add(folded);
      decompressedBytes += entry.originalSize;
      if (decompressedBytes > MAX_DECOMPRESSED_BYTES) {
        throw new Error("managed-archive-size-limit");
      }
      if (entry.name.replaceAll("\\", "/") === asset.executable) {
        expectedEntries += 1;
        if (entry.originalSize < 1 || entry.originalSize > MAX_BINARY_BYTES) {
          throw new Error("managed-binary-size-limit");
        }
        return true;
      }
      return false;
    },
  });
  if (expectedEntries !== 1 || Object.keys(files).length !== 1) {
    throw new Error("managed-archive-binary-count");
  }
  const binary = files[asset.executable];
  if (!binary || binary.byteLength < 1 || binary.byteLength > MAX_BINARY_BYTES) {
    throw new Error("managed-archive-binary-missing");
  }
  return binary;
}

function markerPath(directory: string): string {
  return join(directory, "verified.json");
}

function exactObjectKeys(value: object, expected: readonly string[]): boolean {
  const actual = Object.keys(value).sort();
  const sortedExpected = [...expected].sort();
  return (
    actual.length === sortedExpected.length &&
    actual.every((key, index) => key === sortedExpected[index])
  );
}

async function verifyCacheDirectory(
  directory: string,
  version: string,
  platform: ManagedPlatform,
): Promise<string | undefined> {
  let marker: CacheMarker;
  try {
    marker = JSON.parse(await readFile(markerPath(directory), "utf8")) as CacheMarker;
  } catch {
    return undefined;
  }
  if (
    !marker ||
    typeof marker !== "object" ||
    !exactObjectKeys(marker, [
      "asset",
      "assetByteSize",
      "assetSha256",
      "binarySha256",
      "packageVersion",
      "schemaVersion",
      "sourceCommit",
      "target",
    ]) ||
    marker.schemaVersion !== 1 ||
    marker.packageVersion !== version ||
    marker.target !== platform.target ||
    marker.asset !== `adocweave-lsp-${platform.target}.zip` ||
    !Number.isSafeInteger(marker.assetByteSize) ||
    marker.assetByteSize < 1 ||
    !/^[0-9a-f]{40}$/.test(marker.sourceCommit) ||
    !/^[0-9a-f]{64}$/.test(marker.assetSha256) ||
    !/^[0-9a-f]{64}$/.test(marker.binarySha256) ||
    basename(directory) !== marker.assetSha256
  ) {
    return undefined;
  }
  const binary = join(directory, platform.executable);
  try {
    const bytes = await readFile(binary);
    if (!(await stat(binary)).isFile()) return undefined;
    if (sha256(bytes) !== marker.binarySha256) return undefined;
    await access(binary, process.platform === "win32" ? constants.F_OK : constants.X_OK);
    return binary;
  } catch {
    return undefined;
  }
}

async function ensureManagedRoot(storagePath: string): Promise<string> {
  const root = resolve(storagePath);
  if (root === parse(root).root) throw new Error("managed-cache-invalid-root");
  await mkdir(root, { recursive: true });
  const owner = join(root, OWNER_MARKER);
  try {
    await writeFile(owner, OWNER_MARKER_CONTENT, { flag: "wx", mode: 0o600 });
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
    if ((await readFile(owner, "utf8")) !== OWNER_MARKER_CONTENT) {
      throw new Error("managed-cache-owner-mismatch");
    }
  }
  return root;
}

export async function findVerifiedCache(
  storagePath: string,
  version: string,
  platform: ManagedPlatform,
): Promise<string | undefined> {
  const root = join(storagePath, version, platform.target);
  let entries: Dirent[];
  try {
    entries = await readdir(root, { withFileTypes: true });
  } catch {
    return undefined;
  }
  for (const entry of entries.sort((left, right) => left.name.localeCompare(right.name))) {
    if (!entry.isDirectory() || !/^[0-9a-f]{64}$/.test(entry.name)) continue;
    const binary = await verifyCacheDirectory(join(root, entry.name), version, platform);
    if (binary) return binary;
  }
  return undefined;
}

async function acquireLock(
  path: string,
  signal: AbortSignal | undefined,
): Promise<() => Promise<void>> {
  const started = Date.now();
  while (Date.now() - started < LOCK_WAIT_MS) {
    signal?.throwIfAborted();
    try {
      const token = `${process.pid}:${randomUUID()}\n`;
      const handle = await open(path, "wx", 0o600);
      await handle.writeFile(token);
      return async () => {
        await handle.close();
        try {
          if ((await readFile(path, "utf8")) === token) await rm(path);
        } catch (error) {
          if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
        }
      };
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
      try {
        if (Date.now() - (await stat(path)).mtimeMs > LOCK_STALE_MS) {
          await rm(path, { force: true });
          continue;
        }
      } catch {
        continue;
      }
      await new Promise((resolvePromise) => setTimeout(resolvePromise, 100));
    }
  }
  throw new Error("managed-cache-lock-timeout");
}

export async function installManagedServer(
  platform: ManagedPlatform,
  options: InstallerOptions,
): Promise<string> {
  const fetcher = options.fetcher ?? fetch;
  const storageRoot = await ensureManagedRoot(options.storagePath);
  const root = join(storageRoot, options.version, platform.target);
  await mkdir(root, { recursive: true });
  const release = await download(
    releaseUrl(options.version, MANIFEST_NAME),
    fetcher,
    options.signal,
    undefined,
    MAX_MANIFEST_BYTES,
  );
  const manifest = parseDistributionManifest(new TextDecoder().decode(release), options.version);
  const asset = selectLspAsset(manifest, platform);
  const destination = join(root, asset.sha256);
  const cached = await verifyCacheDirectory(destination, options.version, platform);
  if (cached) return cached;

  const releaseLock = join(root, ".install.lock");
  const releaseLockCleanup = await acquireLock(releaseLock, options.signal);
  try {
    const afterLock = await verifyCacheDirectory(destination, options.version, platform);
    if (afterLock) return afterLock;
    const archive = await download(
      releaseUrl(options.version, asset.name),
      fetcher,
      options.signal,
      asset.byteSize,
    );
    if (sha256(archive) !== asset.sha256) throw new Error("managed-download-hash-mismatch");
    const binary = extractManagedBinary(archive, asset);
    const staging = join(root, `.staging-${randomUUID()}`);
    await mkdir(staging, { mode: 0o700 });
    try {
      const binaryPath = join(staging, platform.executable);
      await writeFile(binaryPath, binary, { mode: 0o755 });
      const marker: CacheMarker = {
        asset: asset.name,
        assetByteSize: asset.byteSize,
        assetSha256: asset.sha256,
        binarySha256: sha256(binary),
        packageVersion: options.version,
        schemaVersion: 1,
        sourceCommit: manifest.sourceCommit,
        target: platform.target,
      };
      await writeFile(markerPath(staging), `${JSON.stringify(marker)}\n`, { mode: 0o600 });
      try {
        await rename(staging, destination);
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
      }
    } finally {
      await rm(staging, { force: true, recursive: true });
    }
    const installed = await verifyCacheDirectory(destination, options.version, platform);
    if (!installed) throw new Error("managed-cache-commit-failed");
    return installed;
  } finally {
    await releaseLockCleanup();
  }
}

export async function clearManagedServers(storagePath: string): Promise<void> {
  const root = resolve(storagePath);
  if (root === parse(root).root) throw new Error("managed-cache-invalid-root");
  try {
    if ((await readFile(join(root, OWNER_MARKER), "utf8")) !== OWNER_MARKER_CONTENT) {
      throw new Error("managed-cache-owner-mismatch");
    }
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return;
    throw error;
  }
  await rm(root, { force: true, recursive: true });
}
