import assert from "node:assert/strict";
import test from "node:test";

import { installLatestCompatibleConsumer } from "./textlint-plugin-consumer-e2e.mjs";
import { runTextlintPluginCompatibilityProbe } from "./textlint-plugin-compatibility-probe.mjs";

test("検証済みcontractのidentityと対応版でlatest依存を観測する", async () => {
  const contract = {
    compatibility: {
      nodeEngine: ">=20.18.0 <25",
      textlintTypesVersion: "15.8.0",
      textlintVersion: "15.8.0",
    },
    identity: {
      packageName: "@adocweave/textlint-plugin-asciidoc",
      pluginName: "@adocweave/asciidoc",
      private: true,
    },
  };
  const calls = [];
  const result = await runTextlintPluginCompatibilityProbe("candidate.tgz", {
    contract,
    runConsumer: async (archive, options) => calls.push({ archive, options }),
  });
  assert.equal(calls.length, 1);
  assert.match(calls[0].archive, /candidate\.tgz$/);
  assert.equal(calls[0].options.contract, contract);
  assert.equal(calls[0].options.installPackage, installLatestCompatibleConsumer);
  assert.deepEqual(result, {
    packageName: "@adocweave/textlint-plugin-asciidoc",
    textlintVersion: "15.8.0",
  });
});
