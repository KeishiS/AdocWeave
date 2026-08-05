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
  writeFileSync(join(root, packagePath, "index.js"), "export const value = 1;\n");
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
    const inventory = verifyInstalledConsumerTree(root);
    assert.deepEqual(inventory.map(({ contentDigest: _, ...entry }) => entry), [{
      path: packagePath,
      name: "example",
      ...packageEntry,
    }]);
    assert.match(inventory[0].contentDigest, /^sha256-[A-Za-z0-9+/]+={0,2}$/);
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
  const before = [{ path: packagePath, name: "example", ...packageEntry, contentDigest: "sha256-before" }];
  assert.doesNotThrow(() => assertConsumerTreeUnchanged(before, structuredClone(before)));
  const after = structuredClone(before);
  after[0].version = "1.2.4";
  assert.throws(() => assertConsumerTreeUnchanged(before, after), /実install treeが変化しました/);
});

test("plugin追加前後のpackage本文改変を拒否する", () => {
  const root = fixture();
  try {
    const before = verifyInstalledConsumerTree(root);
    writeFileSync(join(root, packagePath, "index.js"), "export const value = 999;\n");
    const after = verifyInstalledConsumerTree(root);
    assert.notEqual(after[0].contentDigest, before[0].contentDigest);
    assert.throws(
      () => assertConsumerTreeUnchanged(before, after),
      /実install treeが変化しました/,
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

function optionalFixture(entries, installedPaths) {
  const root = mkdtempSync(join(tmpdir(), "adocweave-optional-consumer-test-"));
  mkdirSync(join(root, "node_modules"), { recursive: true });
  const packages = { "": { name: "consumer", version: "0.0.0" }, ...entries };
  const installed = {};
  for (const path of installedPaths) {
    mkdirSync(join(root, path), { recursive: true });
    const parts = path.slice(path.lastIndexOf("node_modules/") + "node_modules/".length).split("/");
    const name = parts[0].startsWith("@") ? `${parts[0]}/${parts[1]}` : parts[0];
    writeFileSync(join(root, path, "package.json"), JSON.stringify({
      name,
      version: entries[path].version,
    }));
    installed[path] = entries[path];
  }
  writeFileSync(join(root, "package-lock.json"), JSON.stringify({ lockfileVersion: 3, packages }));
  writeFileSync(join(root, "node_modules", ".package-lock.json"), JSON.stringify({
    lockfileVersion: 3,
    packages: installed,
  }));
  return root;
}

test("省略されたoptional親packageの子孫も期待treeから除外する", () => {
  const parent = "node_modules/darwin-parent";
  const child = `${parent}/node_modules/child`;
  const entries = {
    [parent]: { ...packageEntry, optional: true, os: ["darwin"] },
    [child]: { ...packageEntry, optional: true },
  };
  const root = optionalFixture(entries, []);
  try {
    assert.deepEqual(
      verifyInstalledConsumerTree(root, { platform: { os: "linux", cpu: "x64", libc: "glibc" } }),
      [],
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("Linux以外ではlibc制約をOS判定に使用しない", () => {
  const path = "node_modules/libc-marked";
  const entries = { [path]: { ...packageEntry, optional: true, libc: ["glibc"] } };
  const root = optionalFixture(entries, [path]);
  try {
    assert.equal(
      verifyInstalledConsumerTree(root, { platform: { os: "darwin", cpu: "arm64" } }).length,
      1,
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
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
