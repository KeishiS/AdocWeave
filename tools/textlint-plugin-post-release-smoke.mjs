import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

import { runTextlintPluginNpxSmoke } from "./textlint-plugin-npx-smoke.mjs";
import { loadStrictContract } from "./textlint-plugin-npx-smoke.mjs";

const DEFAULT_MANIFEST = new URL("../release-manifest.json", import.meta.url);

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  const manifest = JSON.parse(await readFile(DEFAULT_MANIFEST, "utf8"));
  const releaseBaseUrl =
    `https://github.com/KeishiS/adocweave/releases/download/v${manifest.packageVersion}`;
  await runTextlintPluginPostReleaseSmoke(releaseBaseUrl);
  process.stdout.write(`textlint plugin post-release smoke passed: v${manifest.packageVersion}\n`);
}

export async function runTextlintPluginPostReleaseSmoke(
  releaseBaseUrl,
  {
    contract,
    fetchAsset = fetchBytes,
    runNpxSmoke = runTextlintPluginNpxSmoke,
    verifyPackage = verifyTextlintPluginPackage,
    version,
  } = {},
) {
  contract ??= await loadStrictContract();
  version ??= JSON.parse(await readFile(DEFAULT_MANIFEST, "utf8")).packageVersion;
  const base = new URL(`${releaseBaseUrl.replace(/\/$/, "")}/`);
  if (base.protocol !== "https:" || base.hostname !== "github.com") {
    throw new Error("post-release smoke requires an HTTPS GitHub Release URL");
  }
  if (typeof version !== "string") {
    throw new Error("release manifest is missing packageVersion");
  }
  const archiveName = `adocweave-textlint-plugin-asciidoc-${version}.tgz`;
  const archiveUrl = new URL(archiveName, base).href;
  const checksumUrl = new URL("sha256.sum", base).href;
  const [archiveBytes, checksumBytes] = await Promise.all([
    fetchAsset(archiveUrl),
    fetchAsset(checksumUrl),
  ]);
  const expected = checksumFor(checksumBytes.toString("utf8"), archiveName);
  const actual = createHash("sha256").update(archiveBytes).digest("hex");
  if (actual !== expected) {
    throw new Error(`published textlint plugin checksum mismatch: ${actual} != ${expected}`);
  }

  const scratch = await mkdtemp(join(tmpdir(), "adocweave-textlint-post-release-"));
  try {
    const archive = join(scratch, archiveName);
    await writeFile(archive, archiveBytes);
    await verifyPackage(archive);
  } finally {
    await rm(scratch, { force: true, maxRetries: 5, recursive: true, retryDelay: 100 });
  }
  await runNpxSmoke(archiveUrl, { contract });
}

export function checksumFor(source, archiveName) {
  const matches = source.split(/\r?\n/).flatMap((line) => {
    const match = line.match(/^([0-9a-f]{64})\s+\*?(.+)$/);
    return match && match[2] === archiveName ? [match[1]] : [];
  });
  if (matches.length !== 1) {
    throw new Error(`checksum list must contain exactly one entry for ${archiveName}`);
  }
  return matches[0];
}

async function fetchBytes(url) {
  const response = await fetch(url, { redirect: "follow" });
  if (!response.ok) throw new Error(`cannot download ${url}: HTTP ${response.status}`);
  return Buffer.from(await response.arrayBuffer());
}

async function verifyTextlintPluginPackage(archive) {
  const result = await runProcess(process.execPath, [
    "tools/verify-textlint-plugin-package.mjs",
    archive,
  ]);
  if (result.code !== 0) {
    throw new Error(
      `published textlint package verification failed with ${result.code}\n` +
      `stdout:\n${result.stdout}\nstderr:\n${result.stderr}`,
    );
  }
}

function runProcess(command, arguments_) {
  return new Promise((resolveProcess, rejectProcess) => {
    const child = spawn(command, arguments_, { stdio: ["ignore", "pipe", "pipe"] });
    const stdout = [];
    const stderr = [];
    child.stdout.on("data", (chunk) => stdout.push(chunk));
    child.stderr.on("data", (chunk) => stderr.push(chunk));
    child.once("error", rejectProcess);
    child.once("close", (code) => resolveProcess({
      code: code ?? 128,
      stderr: Buffer.concat(stderr).toString("utf8"),
      stdout: Buffer.concat(stdout).toString("utf8"),
    }));
  });
}
