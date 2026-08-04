import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const root = fileURLToPath(new URL("..", import.meta.url));

function fail(message) {
  throw new Error(message);
}

function packageKey(pkg) {
  return `${pkg.name} ${pkg.version} ${pkg.license}`;
}

export function thirdPartyPackages(metadata) {
  const workspace = new Set(metadata.workspace_members);
  return metadata.packages
    .filter((pkg) => !workspace.has(pkg.id))
    .map((pkg) => {
      if (!pkg.license) fail(`${pkg.name} ${pkg.version} has no license metadata`);
      return { name: pkg.name, version: pkg.version, license: pkg.license };
    })
    .sort((left, right) => packageKey(left).localeCompare(packageKey(right)));
}

export function reachableThirdPartyPackages(metadata, rootPackageName) {
  const roots = metadata.packages.filter((pkg) => pkg.name === rootPackageName && !pkg.source);
  if (roots.length !== 1) fail(`workspace packageを一意に特定できません: ${rootPackageName}`);
  const workspace = new Set(metadata.workspace_members);
  const nodes = new Map((metadata.resolve?.nodes ?? []).map((node) => [node.id, node]));
  const visited = new Set();
  const pending = [roots[0].id];
  while (pending.length > 0) {
    const id = pending.pop();
    if (visited.has(id)) continue;
    visited.add(id);
    for (const dependency of nodes.get(id)?.deps ?? []) pending.push(dependency.pkg);
  }
  return metadata.packages
    .filter((pkg) => visited.has(pkg.id) && !workspace.has(pkg.id))
    .map((pkg) => {
      if (!pkg.license) fail(`${pkg.name} ${pkg.version} has no license metadata`);
      return { name: pkg.name, version: pkg.version, license: pkg.license };
    })
    .sort((left, right) => packageKey(left).localeCompare(packageKey(right)));
}

export function npmRuntimePackages(packageManifest, packageLock) {
  const dependencies = Object.keys(packageManifest.dependencies ?? {});
  return dependencies
    .map((name) => {
      const entry = packageLock.packages?.[`node_modules/${name}`];
      if (!entry?.version) fail(`${name} has no locked npm package`);
      if (!entry.license) fail(`${name} ${entry.version} has no license metadata`);
      return { name, version: entry.version, license: entry.license };
    })
    .sort((left, right) => packageKey(left).localeCompare(packageKey(right)));
}

function groupedRows(packages) {
  const grouped = new Map();
  for (const pkg of packages) {
    const entries = grouped.get(pkg.license) ?? [];
    entries.push(`${pkg.name} ${pkg.version}`);
    grouped.set(pkg.license, entries);
  }
  return [...grouped]
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([license, entries]) => `|${license}\n|${entries.join(", ")}`)
    .join("\n\n");
}

function table(packages, subject = "Crateとversion") {
  return `[cols="2,5",options="header"]
|===
|SPDX license expression |${subject}

${groupedRows(packages)}
|===`;
}

export function renderThirdPartyNotices(rootMetadata, zedMetadata, vscodePackages = []) {
  const rootPackages = thirdPartyPackages(rootMetadata);
  const rootKeys = new Set(rootPackages.map(packageKey));
  const zedOnlyPackages = thirdPartyPackages(zedMetadata)
    .filter((pkg) => !rootKeys.has(packageKey(pkg)));

  return `= Third-party notices

このファイルはroot workspaceとZed拡張のlockfileから、配布時に生成される。各項目にはSPDX license expressionと
crate versionを記載する。各licenseの全文と著作権表示は、crate packageおよび記載されたSPDX licenseを参照する。
この表はAdocWeave自身の\`MIT OR Apache-2.0\` licenseを置き換えない。

${table(rootPackages)}

== Zed開発拡張archiveの追加依存

Zed開発拡張はsource archiveとして配布され、初回導入時にZedが追加crateをbuildする。root workspaceにも同一の
name・version・licenseで含まれるcrateは重複記載しない。

${table(zedOnlyPackages)}

== VS Code拡張の実行時依存

VSIXへ同梱するnpm packageを記載する。開発時だけ使用するpackageは含めない。

${table(vscodePackages, "npm packageとversion")}
`;
}

export function renderTextlintPluginNotices(rootMetadata) {
  const packages = reachableThirdPartyPackages(rootMetadata, "adocweave-textlint-wasm");
  return `= Third-party notices

このファイルには、同梱するNode.js向けWebAssemblyから到達するRust crateのSPDX license expressionと
versionを記載します。各licenseの全文と著作権表示は、crate packageおよび記載されたSPDX licenseを
参照してください。この表はAdocWeave自身の\`MIT OR Apache-2.0\` licenseを置き換えません。

${table(packages)}
`;
}

function cargoMetadata(args) {
  const result = spawnSync("cargo", ["metadata", "--locked", "--format-version=1", ...args], {
    cwd: root,
    encoding: "utf8",
  });
  if (result.status !== 0) fail(result.stderr || "cargo metadata failed");
  return JSON.parse(result.stdout);
}

export function generateThirdPartyNotices(outputPath) {
  const rootMetadata = cargoMetadata([]);
  const zedMetadata = cargoMetadata(["--manifest-path", "editors/zed/Cargo.toml"]);
  const vscodeManifest = JSON.parse(readFileSync(new URL("../editors/vscode/package.json", import.meta.url), "utf8"));
  const vscodeLock = JSON.parse(readFileSync(new URL("../editors/vscode/package-lock.json", import.meta.url), "utf8"));
  const vscodePackages = npmRuntimePackages(vscodeManifest, vscodeLock);
  const output = resolve(root, outputPath);
  mkdirSync(dirname(output), { recursive: true });
  writeFileSync(output, renderThirdPartyNotices(rootMetadata, zedMetadata, vscodePackages));
}

export function generateTextlintPluginNotices(outputPath) {
  const output = resolve(root, outputPath);
  mkdirSync(dirname(output), { recursive: true });
  writeFileSync(output, renderTextlintPluginNotices(cargoMetadata([])));
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  const textlintPlugin = args[0] === "--textlint-plugin";
  const outputPath = textlintPlugin ? args[1] : args[0];
  if (!outputPath || args.length !== (textlintPlugin ? 2 : 1)) {
    process.stderr.write("usage: node tools/generate-third-party-notices.mjs [--textlint-plugin] OUTPUT_PATH\n");
    process.exit(2);
  }
  try {
    if (textlintPlugin) generateTextlintPluginNotices(outputPath);
    else generateThirdPartyNotices(outputPath);
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
