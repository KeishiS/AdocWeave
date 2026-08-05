import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { verifyTextlintPluginReproducibility } from "./verify-textlint-plugin-reproducibility.mjs";

test("別のsource、Cargo targetおよびnpm cacheで同じarchiveを構築する", async () => {
  const root = await mkdtemp(join(tmpdir(), "adocweave-textlint-repro-test-"));
  const calls = [];
  const verified = [];
  try {
    const hash = await verifyTextlintPluginReproducibility({
      repositoryRoot: root,
      prepareSource: async (_repositoryRoot, sourceDirectory) => {
        await mkdir(sourceDirectory, { recursive: true });
        await writeFile(
          join(sourceDirectory, "release-manifest.json"),
          JSON.stringify({ packageVersion: "1.2.3" }),
        );
      },
      buildPackage: async (directories) => {
        calls.push(directories);
        await mkdir(directories.outputDirectory, { recursive: true });
        await writeFile(
          join(directories.outputDirectory, "adocweave-textlint-plugin-asciidoc-1.2.3.tgz"),
          "same archive bytes",
        );
      },
      verifyPackage: async ({ archive }) => verified.push(archive),
    });
    assert.match(hash, /^[0-9a-f]{64}$/);
    assert.equal(calls.length, 2);
    for (const field of [
      "cargoTargetDirectory",
      "npmCacheDirectory",
      "outputDirectory",
      "sourceDirectory",
      "wasmOutputDirectory",
    ]) {
      assert.notEqual(calls[0][field], calls[1][field], field);
    }
    assert.equal(verified.length, 2);
    assert.notEqual(verified[0], verified[1]);
  } finally {
    await rm(root, { force: true, recursive: true });
  }
});

test("clean build間のbyte差を拒否する", async () => {
  const root = await mkdtemp(join(tmpdir(), "adocweave-textlint-repro-test-"));
  let count = 0;
  try {
    await assert.rejects(
      verifyTextlintPluginReproducibility({
        repositoryRoot: root,
        prepareSource: async (_repositoryRoot, sourceDirectory) => {
          await mkdir(sourceDirectory, { recursive: true });
          await writeFile(
            join(sourceDirectory, "release-manifest.json"),
            JSON.stringify({ packageVersion: "1.2.3" }),
          );
        },
        buildPackage: async ({ outputDirectory }) => {
          count += 1;
          await mkdir(outputDirectory, { recursive: true });
          await writeFile(
            join(outputDirectory, "adocweave-textlint-plugin-asciidoc-1.2.3.tgz"),
            `archive ${count}`,
          );
        },
        verifyPackage: async () => {},
      }),
      /clean textlint plugin builds differ/,
    );
  } finally {
    await rm(root, { force: true, recursive: true });
  }
});
