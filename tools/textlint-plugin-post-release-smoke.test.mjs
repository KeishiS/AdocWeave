import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import test from "node:test";

import {
  checksumFor,
  runTextlintPluginPostReleaseSmoke,
} from "./textlint-plugin-post-release-smoke.mjs";

const contract = {
  compatibility: { textlintVersion: "15.8.0" },
  identity: {
    packageName: "@adocweave/textlint-plugin-asciidoc",
    pluginName: "@adocweave/asciidoc",
  },
  oneShot: {
    preset: "ja-technical-writing",
    rulePackage: "textlint-rule-preset-ja-technical-writing",
    ruleVersion: "12.0.2",
  },
};

test("公開asset、checksumおよび実URLのnpx経路を順に検査する", async () => {
  const archive = Buffer.from("published archive");
  const hash = createHash("sha256").update(archive).digest("hex");
  const fetched = [];
  const verified = [];
  const npx = [];
  await runTextlintPluginPostReleaseSmoke(
    "https://github.com/KeishiS/adocweave/releases/download/v1.2.3",
    {
      contract,
      fetchAsset: async (url) => {
        fetched.push(url);
        return url.endsWith("sha256.sum")
          ? Buffer.from(`${hash}  adocweave-textlint-plugin-asciidoc-1.2.3.tgz\n`)
          : archive;
      },
      runNpxSmoke: async (url, options) => npx.push({ options, url }),
      verifyPackage: async (path) => verified.push(path),
      version: "1.2.3",
    },
  );
  assert.deepEqual(fetched, [
    "https://github.com/KeishiS/adocweave/releases/download/v1.2.3/adocweave-textlint-plugin-asciidoc-1.2.3.tgz",
    "https://github.com/KeishiS/adocweave/releases/download/v1.2.3/sha256.sum",
  ]);
  assert.equal(verified.length, 1);
  assert.equal(npx[0].url, fetched[0]);
  assert.equal(npx[0].options.contract, contract);
});

test("checksumの欠落、重複および不一致を拒否する", async () => {
  const name = "adocweave-textlint-plugin-asciidoc-1.2.3.tgz";
  const hash = "a".repeat(64);
  assert.equal(checksumFor(`${hash}  ${name}\n`, name), hash);
  assert.throws(() => checksumFor("", name), /exactly one entry/);
  assert.throws(() => checksumFor(`${hash}  ${name}\n${hash}  ${name}\n`, name), /exactly one entry/);
  await assert.rejects(
    runTextlintPluginPostReleaseSmoke(
      "https://github.com/KeishiS/adocweave/releases/download/v1.2.3",
      {
        contract,
        fetchAsset: async (url) => url.endsWith("sha256.sum")
          ? Buffer.from(`${hash}  ${name}\n`)
          : Buffer.from("different"),
        runNpxSmoke: async () => {},
        verifyPackage: async () => {},
        version: "1.2.3",
      },
    ),
    /checksum mismatch/,
  );
});
