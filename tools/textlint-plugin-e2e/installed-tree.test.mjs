import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
  assertConsumerTreeUnchanged,
  verifyInstalledConsumerTree,
} from "./installed-tree.mjs";

const packagePath = "node_modules/example";
const packageEntry = {
  version: "1.2.3",
  resolved: "https://registry.npmjs.org/example/-/example-1.2.3.tgz",
  integrity: "sha512-Zml4dHVyZQ==",
};

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "adocweave-fixed-consumer-test-"));
  mkdirSync(join(root, packagePath), { recursive: true });
  writeFileSync(join(root, packagePath, "package.json"), JSON.stringify({
    name: "example",
    version: packageEntry.version,
  }));
  writeFileSync(join(root, "package-lock.json"), JSON.stringify({
    name: "consumer",
    version: "0.0.0",
    lockfileVersion: 3,
    packages: { "": { name: "consumer", version: "0.0.0" }, [packagePath]: packageEntry },
  }));
  writeFileSync(join(root, "node_modules", ".package-lock.json"), JSON.stringify({
    name: "consumer",
    version: "0.0.0",
    lockfileVersion: 3,
    packages: { [packagePath]: packageEntry },
  }));
  return root;
}

function mutateJson(path, mutation) {
  const value = JSON.parse(readFileSync(path, "utf8"));
  mutation(value);
  writeFileSync(path, JSON.stringify(value));
}

test("固定lockfileと実install treeのname、version、resolved、integrityを照合する", () => {
  const root = fixture();
  try {
    assert.deepEqual(verifyInstalledConsumerTree(root), [{
      path: packagePath,
      name: "example",
      ...packageEntry,
    }]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("hidden lockのintegrity改変を拒否する", () => {
  const root = fixture();
  try {
    mutateJson(join(root, "node_modules", ".package-lock.json"), (value) => {
      value.packages[packagePath].integrity = "sha512-dGFtcGVyZWQ=";
    });
    assert.throws(() => verifyInstalledConsumerTree(root), /integrityが固定lockfileと一致しません/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("package manifestのnameまたはversion改変を拒否する", () => {
  for (const [field, value, message] of [
    ["name", "other", /nameがpathと一致しません/],
    ["version", "9.9.9", /versionが固定lockfileと一致しません/],
  ]) {
    const root = fixture();
    try {
      mutateJson(join(root, packagePath, "package.json"), (manifest) => {
        manifest[field] = value;
      });
      assert.throws(() => verifyInstalledConsumerTree(root), message);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }
});

test("実treeとhidden lockの余分または欠落packageを拒否する", () => {
  const mutations = [
    (root) => rmSync(join(root, packagePath), { recursive: true }),
    (root) => {
      const extra = join(root, "node_modules", "extra");
      mkdirSync(extra);
      writeFileSync(join(extra, "package.json"), JSON.stringify({ name: "extra", version: "1.0.0" }));
    },
    (root) => mutateJson(join(root, "node_modules", ".package-lock.json"), (value) => {
      delete value.packages[packagePath];
    }),
    (root) => mutateJson(join(root, "node_modules", ".package-lock.json"), (value) => {
      value.packages["node_modules/extra"] = { ...packageEntry, version: "1.0.0" };
    }),
  ];
  for (const mutation of mutations) {
    const root = fixture();
    try {
      mutation(root);
      assert.throws(() => verifyInstalledConsumerTree(root), /余分または許可されない欠落package/);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }
});

test("plugin以外のinventory変更を拒否する", () => {
  const before = [{ path: packagePath, name: "example", ...packageEntry }];
  assert.doesNotThrow(() => assertConsumerTreeUnchanged(before, structuredClone(before)));
  const after = structuredClone(before);
  after[0].version = "1.2.4";
  assert.throws(() => assertConsumerTreeUnchanged(before, after), /実install treeが変化しました/);
});

test("固定lockfileのlinkまたは固定されていない取得元を拒否する", () => {
  for (const mutation of [
    (entry) => { entry.link = true; },
    (entry) => { entry.resolved = "file:../example"; },
    (entry) => { entry.resolved = "git+https://example.invalid/example.git"; },
  ]) {
    const root = fixture();
    try {
      mutateJson(join(root, "package-lock.json"), (value) => mutation(value.packages[packagePath]));
      assert.throws(() => verifyInstalledConsumerTree(root), /固定されていない取得元/);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }
});
