import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { access, chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { zipSync } from "fflate";

import type { DistributionAsset } from "../src/distribution-manifest.js";
import { platformForHost } from "../src/platform.js";
import {
  clearManagedServers,
  extractManagedBinary,
  findVerifiedCache,
  installManagedServer,
} from "../src/installer.js";

const asset: DistributionAsset = {
  archive: "zip",
  byteSize: 1,
  executable: "adocweave-lsp.exe",
  kind: "lsp",
  name: "adocweave-lsp-x86_64-pc-windows-msvc.zip",
  sha256: "a".repeat(64),
  target: "x86_64-pc-windows-msvc",
};

test("ZIPから期待する実行fileだけを取り出します", () => {
  const archive = zipSync({
    "LICENSE-MIT": new TextEncoder().encode("license"),
    "adocweave-lsp.exe": new TextEncoder().encode("binary"),
  });
  assert.equal(new TextDecoder().decode(extractManagedBinary(archive, asset)), "binary");
});

test("path traversalと大小文字衝突を拒否します", () => {
  assert.throws(
    () =>
      extractManagedBinary(
        zipSync({
          "../adocweave-lsp.exe": new TextEncoder().encode("binary"),
        }),
        asset,
      ),
    /unsafe-path/,
  );
  assert.throws(
    () =>
      extractManagedBinary(
        zipSync({
          "ADOCWEAVE-LSP.EXE": new TextEncoder().encode("first"),
          "adocweave-lsp.exe": new TextEncoder().encode("second"),
        }),
        asset,
      ),
    /duplicate-path/,
  );
});

function digest(bytes: Uint8Array): string {
  return createHash("sha256").update(bytes).digest("hex");
}

function response(body: string | Uint8Array, url: string): Response {
  const bytes = typeof body === "string" ? Buffer.from(body) : Buffer.from(body);
  const value = new Response(bytes, { headers: { "content-length": String(bytes.byteLength) } });
  Object.defineProperty(value, "url", { value: url });
  return value;
}

function releaseFetcher(
  archive: Uint8Array,
  options: { archiveHash?: string; includeAsset?: boolean } = {},
): typeof fetch {
  const platform = platformForHost("linux", "x64");
  const manifest = {
    assets:
      options.includeAsset === false
        ? []
        : [
            {
              archive: "zip",
              byteSize: archive.byteLength,
              executable: platform.executable,
              kind: "lsp",
              name: `adocweave-lsp-${platform.target}.zip`,
              sha256: options.archiveHash ?? digest(archive),
              target: platform.target,
            },
          ],
    packageVersion: "0.15.0",
    schemaVersion: 2,
    sourceCommit: "a".repeat(40),
  };
  return (async (input) => {
    const url = String(input);
    if (url.endsWith("adocweave-dist-manifest.json")) {
      return response(
        `${JSON.stringify(manifest)}\n`,
        "https://objects.githubusercontent.com/manifest",
      );
    }
    return response(archive, "https://objects.githubusercontent.com/archive");
  }) as typeof fetch;
}

test("managed binaryを検証して原子的cacheへ保存し、offlineでも再利用します", async () => {
  const storagePath = await mkdtemp(join(tmpdir(), "adocweave-managed-"));
  const platform = platformForHost("linux", "x64");
  const archive = zipSync({ [platform.executable]: new TextEncoder().encode("server") });
  try {
    const installed = await installManagedServer(platform, {
      fetcher: releaseFetcher(archive),
      storagePath,
      version: "0.15.0",
    });
    assert.equal(await readFile(installed, "utf8"), "server");
    assert.equal(await findVerifiedCache(storagePath, "0.15.0", platform), installed);
    assert.equal(
      (await readFile(join(installed, "..", "verified.json"), "utf8")).endsWith("\n"),
      true,
    );
  } finally {
    await rm(storagePath, { force: true, recursive: true });
  }
});

test("hash不一致と欠落assetは既存の検証済みcacheを破壊しません", async () => {
  const storagePath = await mkdtemp(join(tmpdir(), "adocweave-managed-"));
  const platform = platformForHost("linux", "x64");
  const archive = zipSync({ [platform.executable]: new TextEncoder().encode("server") });
  try {
    const installed = await installManagedServer(platform, {
      fetcher: releaseFetcher(archive),
      storagePath,
      version: "0.15.0",
    });
    await assert.rejects(
      installManagedServer(platform, {
        fetcher: releaseFetcher(archive, { archiveHash: "0".repeat(64) }),
        storagePath,
        version: "0.15.0",
      }),
      /managed-download-hash-mismatch/,
    );
    await assert.rejects(
      installManagedServer(platform, {
        fetcher: releaseFetcher(archive, { includeAsset: false }),
        storagePath,
        version: "0.15.0",
      }),
      /lsp-asset-count/,
    );
    assert.equal(await findVerifiedCache(storagePath, "0.15.0", platform), installed);
  } finally {
    await rm(storagePath, { force: true, recursive: true });
  }
});

test("改変cacheを採用せず、所有markerを確認して完全削除します", async () => {
  const storagePath = await mkdtemp(join(tmpdir(), "adocweave-managed-"));
  const platform = platformForHost("linux", "x64");
  const archive = zipSync({ [platform.executable]: new TextEncoder().encode("server") });
  const installed = await installManagedServer(platform, {
    fetcher: releaseFetcher(archive),
    storagePath,
    version: "0.15.0",
  });
  await writeFile(installed, "tampered");
  assert.equal(await findVerifiedCache(storagePath, "0.15.0", platform), undefined);
  await clearManagedServers(storagePath);
  await assert.rejects(access(storagePath));

  const unrelated = await mkdtemp(join(tmpdir(), "adocweave-unrelated-"));
  try {
    await writeFile(join(unrelated, "keep"), "user");
    await clearManagedServers(unrelated);
    assert.equal(await readFile(join(unrelated, "keep"), "utf8"), "user");
  } finally {
    await rm(unrelated, { force: true, recursive: true });
  }
});

test("Content-Lengthがない巨大manifestを受信中の上限で拒否します", async () => {
  const storagePath = await mkdtemp(join(tmpdir(), "adocweave-managed-"));
  const platform = platformForHost("linux", "x64");
  const chunk = new Uint8Array(600 * 1024);
  const fetcher = (async () => {
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(chunk);
        controller.enqueue(chunk);
        controller.close();
      },
    });
    const value = new Response(body);
    Object.defineProperty(value, "url", {
      value: "https://objects.githubusercontent.com/oversized-manifest",
    });
    return value;
  }) as typeof fetch;
  try {
    await assert.rejects(
      installManagedServer(platform, {
        fetcher,
        storagePath,
        version: "0.15.0",
      }),
      /managed-download-size-mismatch/,
    );
  } finally {
    await rm(storagePath, { force: true, recursive: true });
  }
});

test("同時installはarchiveを一度だけ取得して同じcacheを返します", async () => {
  const storagePath = await mkdtemp(join(tmpdir(), "adocweave-managed-"));
  const platform = platformForHost("linux", "x64");
  const archive = zipSync({ [platform.executable]: new TextEncoder().encode("server") });
  const baseFetcher = releaseFetcher(archive);
  let archiveDownloads = 0;
  const fetcher = (async (input, init) => {
    if (!String(input).endsWith("adocweave-dist-manifest.json")) archiveDownloads += 1;
    return baseFetcher(input, init);
  }) as typeof fetch;
  try {
    const results = await Promise.all([
      installManagedServer(platform, { fetcher, storagePath, version: "0.15.0" }),
      installManagedServer(platform, { fetcher, storagePath, version: "0.15.0" }),
    ]);
    assert.equal(results[0], results[1]);
    assert.equal(archiveDownloads, 1);
  } finally {
    await rm(storagePath, { force: true, recursive: true });
  }
});

test("書込み権限がないstorageでは既存内容を変更しません", {
  skip: process.platform === "win32" || process.getuid?.() === 0,
}, async () => {
  const storagePath = await mkdtemp(join(tmpdir(), "adocweave-managed-"));
  const platform = platformForHost("linux", "x64");
  const archive = zipSync({ [platform.executable]: new TextEncoder().encode("server") });
  await writeFile(join(storagePath, "keep"), "user");
  await chmod(storagePath, 0o500);
  try {
    await assert.rejects(
      installManagedServer(platform, {
        fetcher: releaseFetcher(archive),
        storagePath,
        version: "0.15.0",
      }),
    );
    assert.equal(await readFile(join(storagePath, "keep"), "utf8"), "user");
  } finally {
    await chmod(storagePath, 0o700);
    await rm(storagePath, { force: true, recursive: true });
  }
});
