import assert from "node:assert/strict";
import { lstatSync, readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

export const PLUGIN_PATH = "node_modules/@adocweave/textlint-plugin-asciidoc";

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function packageNameFromPath(path) {
  const marker = "node_modules/";
  const parts = path.slice(path.lastIndexOf(marker) + marker.length).split("/");
  return parts[0].startsWith("@") ? `${parts[0]}/${parts[1]}` : parts[0];
}

function installedPackagePaths(nodeModules, prefix = "node_modules") {
  const paths = [];
  for (const entry of readdirSync(nodeModules, { withFileTypes: true })) {
    if (entry.name === ".bin" || entry.name === ".package-lock.json") continue;
    const entryPath = join(nodeModules, entry.name);
    if (entry.name.startsWith("@")) {
      if (!entry.isDirectory()) throw new Error(`npm scopeがディレクトリではありません: ${entryPath}`);
      for (const scoped of readdirSync(entryPath, { withFileTypes: true })) {
        if (!scoped.isDirectory()) throw new Error(`npm packageがディレクトリではありません: ${scoped.name}`);
        collectPackage(join(entryPath, scoped.name), `${prefix}/${entry.name}/${scoped.name}`, paths);
      }
    } else {
      if (!entry.isDirectory()) throw new Error(`npm packageがディレクトリではありません: ${entryPath}`);
      collectPackage(entryPath, `${prefix}/${entry.name}`, paths);
    }
  }
  return paths;
}

function collectPackage(directory, path, paths) {
  if (lstatSync(directory).isSymbolicLink()) throw new Error(`npm packageがsymlinkです: ${path}`);
  paths.push(path);
  const nested = join(directory, "node_modules");
  try {
    paths.push(...installedPackagePaths(nested, `${path}/node_modules`));
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
}

function comparableEntry(path, lockEntry, manifest) {
  return {
    path,
    name: manifest.name,
    version: manifest.version,
    resolved: lockEntry.resolved,
    integrity: lockEntry.integrity,
  };
}

function matchesConstraint(values, current) {
  if (!Array.isArray(values) || values.length === 0) return true;
  if (values.includes(`!${current}`)) return false;
  const positive = values.filter((value) => !value.startsWith("!"));
  return positive.length === 0 || positive.includes(current);
}

function currentLibc() {
  return process.report?.getReport?.().header?.glibcVersionRuntime ? "glibc" : "musl";
}

function omittedForPlatform(entry) {
  return entry.optional === true && (!matchesConstraint(entry.os, process.platform) ||
    !matchesConstraint(entry.cpu, process.arch) ||
    !matchesConstraint(entry.libc, currentLibc()));
}

function assertFixedRegistryEntry(path, entry) {
  if (entry.link === true || typeof entry.resolved !== "string" ||
      !/^https:\/\/registry\.npmjs\.org\/.+\.tgz$/.test(entry.resolved) ||
      typeof entry.integrity !== "string" || !entry.integrity.startsWith("sha512-")) {
    throw new Error(`${path}にlink、file、git、aliasまたは固定されていない取得元があります`);
  }
}

export function verifyInstalledConsumerTree(root, { allowPlugin = false } = {}) {
  const lock = readJson(join(root, "package-lock.json"));
  const installedLock = readJson(join(root, "node_modules", ".package-lock.json"));
  if (lock.lockfileVersion !== 3 || installedLock.lockfileVersion !== 3) {
    throw new Error("consumerのnpm lockfileVersionを解釈できません");
  }

  const allowed = new Set(allowPlugin ? [PLUGIN_PATH] : []);
  const allExpectedPaths = Object.keys(lock.packages ?? {}).filter(Boolean).sort();
  for (const path of allExpectedPaths) assertFixedRegistryEntry(path, lock.packages[path]);
  const expectedPaths = allExpectedPaths.filter((path) => !omittedForPlatform(lock.packages[path]));
  const recordedPaths = Object.keys(installedLock.packages ?? {})
    .filter((path) => !allowed.has(path))
    .sort();
  const actualPaths = installedPackagePaths(join(root, "node_modules"))
    .filter((path) => !allowed.has(path))
    .sort();
  assert.deepEqual(recordedPaths, expectedPaths, "hidden lockに余分または許可されない欠落packageがあります");
  assert.deepEqual(actualPaths, expectedPaths, "実install treeに余分または許可されない欠落packageがあります");

  const inventory = [];
  for (const path of expectedPaths) {
    const expected = lock.packages[path];
    const recorded = installedLock.packages[path];
    for (const field of ["version", "resolved", "integrity"]) {
      if (typeof expected[field] !== "string" || expected[field].length === 0) {
        throw new Error(`${path}の${field}が固定lockfileにありません`);
      }
      assert.equal(recorded[field], expected[field], `${path}の${field}が固定lockfileと一致しません`);
    }
    const manifest = readJson(join(root, path, "package.json"));
    assert.equal(manifest.name, packageNameFromPath(path), `${path}のnameがpathと一致しません`);
    assert.equal(manifest.version, expected.version, `${path}のversionが固定lockfileと一致しません`);
    inventory.push(comparableEntry(path, recorded, manifest));
  }
  return inventory;
}

export function assertConsumerTreeUnchanged(before, after) {
  assert.deepEqual(after, before, "plugin追加によりconsumerの実install treeが変化しました");
}
