import { mkdirSync, writeFileSync } from "node:fs";
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

function table(packages) {
  return `[cols="2,5",options="header"]
|===
|SPDX license expression |Crateとversion

${groupedRows(packages)}
|===`;
}

export function renderThirdPartyNotices(rootMetadata, zedMetadata) {
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
  const output = resolve(root, outputPath);
  mkdirSync(dirname(output), { recursive: true });
  writeFileSync(output, renderThirdPartyNotices(rootMetadata, zedMetadata));
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const [outputPath] = process.argv.slice(2);
  if (!outputPath || process.argv.length !== 3) {
    process.stderr.write("usage: node tools/generate-third-party-notices.mjs OUTPUT_PATH\n");
    process.exit(2);
  }
  try {
    generateThirdPartyNotices(outputPath);
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
