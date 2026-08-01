import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { chmod, lstat, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
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

async function removeTemporaryDirectory(path: string): Promise<void> {
  await rm(path, {
    force: true,
    maxRetries: 5,
    recursive: true,
    retryDelay: 100,
  });
}

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
    packageVersion: "0.16.0",
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
      version: "0.16.0",
    });
    assert.equal(await readFile(installed, "utf8"), "server");
    assert.equal(await findVerifiedCache(storagePath, "0.16.0", platform), installed);
    assert.equal(
      (await readFile(join(installed, "..", "verified.json"), "utf8")).endsWith("\n"),
      true,
    );
  } finally {
    await removeTemporaryDirectory(storagePath);
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
      version: "0.16.0",
    });
    await assert.rejects(
      installManagedServer(platform, {
        fetcher: releaseFetcher(archive, { archiveHash: "0".repeat(64) }),
        storagePath,
        version: "0.16.0",
      }),
      /managed-download-hash-mismatch/,
    );
    await assert.rejects(
      installManagedServer(platform, {
        fetcher: releaseFetcher(archive, { includeAsset: false }),
        storagePath,
        version: "0.16.0",
      }),
      /lsp-asset-count/,
    );
    assert.equal(await findVerifiedCache(storagePath, "0.16.0", platform), installed);
  } finally {
    await removeTemporaryDirectory(storagePath);
  }
});

test("改変cacheを採用せず、所有markerを残してmanaged serverだけを削除します", async () => {
  const storagePath = await mkdtemp(join(tmpdir(), "adocweave-managed-"));
  const platform = platformForHost("linux", "x64");
  const archive = zipSync({ [platform.executable]: new TextEncoder().encode("server") });
  const installed = await installManagedServer(platform, {
    fetcher: releaseFetcher(archive),
    storagePath,
    version: "0.16.0",
  });
  await writeFile(installed, "tampered");
  assert.equal(await findVerifiedCache(storagePath, "0.16.0", platform), undefined);
  await clearManagedServers(storagePath);
  assert.deepEqual(await readdir(storagePath), [".adocweave-vscode-managed-cache"]);

  const unrelated = await mkdtemp(join(tmpdir(), "adocweave-unrelated-"));
  try {
    await writeFile(join(unrelated, "keep"), "user");
    await clearManagedServers(unrelated);
    assert.equal(await readFile(join(unrelated, "keep"), "utf8"), "user");
  } finally {
    await removeTemporaryDirectory(unrelated);
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
        version: "0.16.0",
      }),
      /managed-download-size-mismatch/,
    );
  } finally {
    await removeTemporaryDirectory(storagePath);
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
    const results = await Promise.all(
      Array.from({ length: 8 }, () =>
        installManagedServer(platform, { fetcher, storagePath, version: "0.16.0" }),
      ),
    );
    assert.ok(results.every((result) => result === results[0]));
    assert.equal(archiveDownloads, 1);
    assert.equal(
      (await lstat(join(storagePath, ".adocweave-vscode-managed-cache"))).isDirectory(),
      true,
    );
  } finally {
    await removeTemporaryDirectory(storagePath);
  }
});

test("以前のfile形式の所有markerを引き続き受け入れます", async () => {
  const storagePath = await mkdtemp(join(tmpdir(), "adocweave-managed-"));
  const platform = platformForHost("linux", "x64");
  const archive = zipSync({ [platform.executable]: new TextEncoder().encode("server") });
  await writeFile(
    join(storagePath, ".adocweave-vscode-managed-cache"),
    "adocweave-vscode-managed-cache-v1\n",
  );
  try {
    const installed = await installManagedServer(platform, {
      fetcher: releaseFetcher(archive),
      storagePath,
      version: "0.16.0",
    });
    assert.equal(await readFile(installed, "utf8"), "server");
    await clearManagedServers(storagePath);
    assert.equal(await findVerifiedCache(storagePath, "0.16.0", platform), undefined);
    assert.equal(
      await readFile(join(storagePath, ".adocweave-vscode-managed-cache"), "utf8"),
      "adocweave-vscode-managed-cache-v1\n",
    );
  } finally {
    await removeTemporaryDirectory(storagePath);
  }
});

test("内容のあるdirectory markerを所有証明として受け入れません", async () => {
  const storagePath = await mkdtemp(join(tmpdir(), "adocweave-managed-"));
  const owner = join(storagePath, ".adocweave-vscode-managed-cache");
  await mkdir(owner);
  await writeFile(join(owner, "unexpected"), "not-owned");
  await writeFile(join(storagePath, "keep"), "user");
  try {
    await assert.rejects(clearManagedServers(storagePath), /managed-cache-owner-mismatch/);
    await assert.rejects(
      installManagedServer(platformForHost("linux", "x64"), {
        fetcher: releaseFetcher(zipSync({ "adocweave-lsp": new TextEncoder().encode("server") })),
        storagePath,
        version: "0.16.0",
      }),
      /managed-cache-owner-mismatch/,
    );
    assert.equal(await readFile(join(storagePath, "keep"), "utf8"), "user");
  } finally {
    await removeTemporaryDirectory(storagePath);
  }
});

test("clearは進行中のinstall完了後にcacheだけを削除します", async () => {
  const storagePath = await mkdtemp(join(tmpdir(), "adocweave-managed-"));
  const platform = platformForHost("linux", "x64");
  const archive = zipSync({ [platform.executable]: new TextEncoder().encode("server") });
  const baseFetcher = releaseFetcher(archive);
  let releaseManifest: (() => void) | undefined;
  let notifyManifestStarted: (() => void) | undefined;
  const manifestStarted = new Promise<void>((resolvePromise) => {
    notifyManifestStarted = resolvePromise;
  });
  const manifestMayContinue = new Promise<void>((resolvePromise) => {
    releaseManifest = resolvePromise;
  });
  let delayed = false;
  const fetcher = (async (input, init) => {
    if (!delayed && String(input).endsWith("adocweave-dist-manifest.json")) {
      delayed = true;
      notifyManifestStarted?.();
      await manifestMayContinue;
    }
    return baseFetcher(input, init);
  }) as typeof fetch;
  try {
    const installing = installManagedServer(platform, {
      fetcher,
      storagePath,
      version: "0.16.0",
    });
    let installFinished = false;
    void installing.then(() => {
      installFinished = true;
    });
    await manifestStarted;
    const clearing = clearManagedServers(storagePath);
    releaseManifest?.();
    await clearing;
    await installing;
    assert.equal(installFinished, true);
    assert.equal(await findVerifiedCache(storagePath, "0.16.0", platform), undefined);
    assert.deepEqual(await readdir(storagePath), [".adocweave-vscode-managed-cache"]);
  } finally {
    await removeTemporaryDirectory(storagePath);
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
        version: "0.16.0",
      }),
    );
    assert.equal(await readFile(join(storagePath, "keep"), "utf8"), "user");
  } finally {
    await chmod(storagePath, 0o700);
    await removeTemporaryDirectory(storagePath);
  }
});

test("Content-Lengthを返さない配信でもmanaged binaryを導入します", async () => {
  // ヘッダーの無い応答は Number(null) が 0 と評価され、0 は有限なので
  // 期待sizeとの不一致として拒否されていました。chunked転送のように
  // Content-Lengthを返さない正常な配信が導入できません。
  const storagePath = await mkdtemp(join(tmpdir(), "adocweave-managed-"));
  const platform = platformForHost("linux", "x64");
  const archive = zipSync({ [platform.executable]: new TextEncoder().encode("server") });
  const withoutContentLength: typeof fetch = async (input, init) => {
    const original = await releaseFetcher(archive)(input, init);
    const body = new Uint8Array(await original.arrayBuffer());
    const stripped = new Response(body);
    Object.defineProperty(stripped, "url", { value: original.url });
    return stripped;
  };
  try {
    const installed = await installManagedServer(platform, {
      fetcher: withoutContentLength,
      storagePath,
      version: "0.16.0",
    });
    assert.equal(await readFile(installed, "utf8"), "server");
  } finally {
    await removeTemporaryDirectory(storagePath);
  }
});

test("Content-Lengthが期待sizeと違う配信は本文を読む前に拒否します", async () => {
  const storagePath = await mkdtemp(join(tmpdir(), "adocweave-managed-"));
  const platform = platformForHost("linux", "x64");
  const archive = zipSync({ [platform.executable]: new TextEncoder().encode("server") });
  const wrongContentLength: typeof fetch = async (input, init) => {
    const original = await releaseFetcher(archive)(input, init);
    if (!String(input).endsWith(".zip")) return original;
    const body = new Uint8Array(await original.arrayBuffer());
    const lying = new Response(body, {
      headers: { "content-length": String(body.byteLength + 1) },
    });
    Object.defineProperty(lying, "url", { value: original.url });
    return lying;
  };
  try {
    await assert.rejects(
      installManagedServer(platform, {
        fetcher: wrongContentLength,
        storagePath,
        version: "0.16.0",
      }),
      /managed-download-size-mismatch/,
    );
  } finally {
    await removeTemporaryDirectory(storagePath);
  }
});
