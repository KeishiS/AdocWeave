import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { basename, join, resolve } from "node:path";

import { canonicalJson, validateDistributionManifest } from "./release-contract.mjs";

export const RELEASE_METADATA_TOOL_VERSION = 1;
const ROOT = new URL("../", import.meta.url);
const readJson = (path) => JSON.parse(readFileSync(new URL(path, ROOT), "utf8"));
const plan = readJson("release/distribution-plan.json");
const contracts = readJson("release-manifest.json").contracts;
const compareText = (left, right) => left < right ? -1 : left > right ? 1 : 0;

function fail(message) {
  throw new Error(message);
}

function digest(algorithm, bytes) {
  return createHash(algorithm).update(bytes).digest("hex");
}

const sha1 = (bytes) => digest("sha1", bytes);
const sha256 = (bytes) => digest("sha256", bytes);

function spdxId(prefix, value) {
  return `SPDXRef-${prefix}-${sha256(value).slice(0, 16)}`;
}

function archiveFiles(path, archiveName) {
  const members = execFileSync("tar", ["-tJf", path], { encoding: "utf8" })
    .trimEnd()
    .split("\n")
    .filter(Boolean);
  const files = [];
  for (const member of members) {
    if (member.startsWith("/") || member.split("/").includes("..")) {
      fail(`unsafe archive member in ${archiveName}: ${member}`);
    }
    if (member.endsWith("/")) continue;
    const contents = execFileSync("tar", ["-xJOf", path, member], { maxBuffer: 64 * 1024 * 1024 });
    files.push({
      SPDXID: spdxId("File", `${archiveName}\0${member}`),
      checksums: [
        { algorithm: "SHA1", checksumValue: sha1(contents) },
        { algorithm: "SHA256", checksumValue: sha256(contents) },
      ],
      copyrightText: "NOASSERTION",
      fileName: `./${archiveName}!/${member}`,
      licenseConcluded: "NOASSERTION",
    });
  }
  return files.sort((left, right) => compareText(left.fileName, right.fileName));
}

function cargoPackages() {
  const metadata = JSON.parse(execFileSync("cargo", ["metadata", "--format-version=1", "--locked"], {
    cwd: ROOT,
    encoding: "utf8",
    maxBuffer: 32 * 1024 * 1024,
  }));
  return metadata.packages.map((entry) => ({
    SPDXID: spdxId("CargoPackage", `${entry.name}\0${entry.version}\0${entry.source ?? "workspace"}`),
    downloadLocation: entry.source?.startsWith("registry+")
      ? `https://crates.io/crates/${entry.name}/${entry.version}`
      : "NOASSERTION",
    externalRefs: [{
      referenceCategory: "PACKAGE-MANAGER",
      referenceLocator: `pkg:cargo/${encodeURIComponent(entry.name)}@${entry.version}`,
      referenceType: "purl",
    }],
    filesAnalyzed: false,
    copyrightText: "NOASSERTION",
    licenseConcluded: "NOASSERTION",
    licenseDeclared: entry.license ?? "NOASSERTION",
    name: entry.name,
    versionInfo: entry.version,
  })).sort((left, right) => compareText(left.SPDXID, right.SPDXID));
}

function frontendPackage() {
  const entry = readJson("web-worker/package.json");
  const dependencies = Object.entries(entry.dependencies ?? {}).sort(([left], [right]) => compareText(left, right));
  if (dependencies.length !== 0) {
    fail("frontend runtime dependencies require explicit locked SBOM support");
  }
  const [namespace, packageName] = entry.name.startsWith("@") ? entry.name.split("/", 2) : [null, entry.name];
  const purlName = namespace
    ? `${encodeURIComponent(namespace)}/${encodeURIComponent(packageName)}`
    : encodeURIComponent(packageName);
  return {
    SPDXID: spdxId("NpmPackage", `${entry.name}\0${entry.version}`),
    downloadLocation: "NOASSERTION",
    copyrightText: "NOASSERTION",
    externalRefs: [{
      referenceCategory: "PACKAGE-MANAGER",
      referenceLocator: `pkg:npm/${purlName}@${entry.version}`,
      referenceType: "purl",
    }],
    filesAnalyzed: false,
    licenseConcluded: "NOASSERTION",
    licenseDeclared: "MIT OR Apache-2.0",
    name: entry.name,
    versionInfo: entry.version,
  };
}

function commitTimestamp(commit) {
  const value = execFileSync("git", ["show", "-s", "--format=%cI", commit], {
    cwd: ROOT,
    encoding: "utf8",
  }).trim();
  return new Date(value).toISOString().replace(".000Z", "Z");
}

export function buildMetadata(directory, sourceCommit) {
  if (!/^[0-9a-f]{40}$/.test(sourceCommit)) fail("source commit must be a lowercase 40-character Git commit");
  const assets = plan.assets.map((planned) => {
    const path = join(directory, planned.name);
    let bytes;
    try {
      bytes = readFileSync(path);
    } catch {
      fail(`missing release archive: ${planned.name}`);
    }
    return {
      ...planned,
      byteSize: bytes.length,
      sha256: sha256(bytes),
      path,
    };
  }).sort((left, right) => compareText(left.name, right.name));

  const distributionManifest = {
    assets: assets.map(({ path: _path, ...asset }) => asset),
    contracts,
    packageVersion: plan.packageVersion,
    schemaVersion: 1,
    sourceCommit,
  };
  validateDistributionManifest(distributionManifest, plan);

  const cargo = cargoPackages();
  const frontend = frontendPackage();
  const archivePackages = [];
  const files = [];
  const relationships = [];
  for (const asset of assets) {
    const packageId = spdxId("Archive", asset.name);
    const archiveEntries = archiveFiles(asset.path, asset.name);
    files.push(...archiveEntries);
    archivePackages.push({
      SPDXID: packageId,
      checksums: [{ algorithm: "SHA256", checksumValue: asset.sha256 }],
      copyrightText: "NOASSERTION",
      // RC tags cannot be derived from packageVersion alone.  Recording an
      // invented stable URL would make the SBOM factually wrong.
      downloadLocation: "NOASSERTION",
      filesAnalyzed: true,
      licenseConcluded: "NOASSERTION",
      licenseDeclared: "MIT OR Apache-2.0",
      name: asset.name,
      packageFileName: asset.name,
      packageVerificationCode: {
        packageVerificationCodeValue: sha1(archiveEntries
          .map((entry) => entry.checksums.find((checksum) => checksum.algorithm === "SHA1").checksumValue)
          .sort(compareText)
          .join("")),
      },
      versionInfo: plan.packageVersion,
    });
    relationships.push({ spdxElementId: "SPDXRef-DOCUMENT", relationshipType: "DESCRIBES", relatedSpdxElement: packageId });
    for (const file of archiveEntries) {
      relationships.push({ spdxElementId: packageId, relationshipType: "CONTAINS", relatedSpdxElement: file.SPDXID });
    }
    const dependencies = asset.kind === "browser" ? [...cargo, frontend] : cargo;
    for (const dependency of dependencies) {
      relationships.push({ spdxElementId: packageId, relationshipType: "DEPENDS_ON", relatedSpdxElement: dependency.SPDXID });
    }
  }
  const packages = [...archivePackages, ...cargo, frontend]
    .sort((left, right) => compareText(left.SPDXID, right.SPDXID));
  relationships.sort((left, right) =>
    compareText(
      `${left.spdxElementId}\0${left.relationshipType}\0${left.relatedSpdxElement}`,
      `${right.spdxElementId}\0${right.relationshipType}\0${right.relatedSpdxElement}`,
    ));
  const sbom = {
    SPDXID: "SPDXRef-DOCUMENT",
    creationInfo: {
      created: commitTimestamp(sourceCommit),
      creators: [`Tool: adocweave-release-metadata/${RELEASE_METADATA_TOOL_VERSION}`],
    },
    dataLicense: "CC0-1.0",
    documentNamespace: `${plan.repository}/releases/sbom/${sourceCommit}`,
    files,
    name: `AdocWeave ${plan.packageVersion} release assets`,
    packages,
    relationships,
    spdxVersion: "SPDX-2.3",
  };

  const manifestText = canonicalJson(distributionManifest);
  const sbomText = canonicalJson(sbom);
  const checksums = [
    ...assets.map((asset) => [asset.name, asset.sha256]),
    ["adocweave-dist-manifest.json", sha256(manifestText)],
    ["adocweave.spdx.json", sha256(sbomText)],
  ].sort(([left], [right]) => compareText(left, right));
  const checksumText = `${checksums.map(([name, digest]) => `${digest}  ${name}`).join("\n")}\n`;
  return { manifestText, sbomText, checksumText };
}

export function writeMetadata(directory, sourceCommit) {
  const metadata = buildMetadata(directory, sourceCommit);
  writeFileSync(join(directory, "adocweave-dist-manifest.json"), metadata.manifestText);
  writeFileSync(join(directory, "adocweave.spdx.json"), metadata.sbomText);
  writeFileSync(join(directory, "sha256.sum"), metadata.checksumText);
}

export function verifyMetadata(directory, sourceCommit) {
  const expected = buildMetadata(directory, sourceCommit);
  for (const [name, text] of [
    ["adocweave-dist-manifest.json", expected.manifestText],
    ["adocweave.spdx.json", expected.sbomText],
    ["sha256.sum", expected.checksumText],
  ]) {
    if (readFileSync(join(directory, name), "utf8") !== text) fail(`release metadata mismatch: ${name}`);
  }
  const entries = readdirSync(directory, { withFileTypes: true });
  if (entries.some((entry) => !entry.isFile())) {
    fail("release directory must contain public asset files only");
  }
  const actual = new Set(entries.map((entry) => entry.name));
  const expectedNames = new Set([...plan.assets.map((asset) => asset.name), ...plan.releaseMetadata.map((entry) => entry.name)]);
  if (actual.size !== expectedNames.size || [...actual].some((name) => !expectedNames.has(name))) {
    fail("release directory contains a missing, duplicate, or unplanned public asset");
  }
}

function main(args) {
  const [command, directoryArg, commitArg] = args;
  if (!new Set(["generate", "verify"]).has(command) || !directoryArg) {
    fail("usage: release-metadata.mjs generate|verify ARTIFACT_DIRECTORY [SOURCE_COMMIT]");
  }
  const directory = resolve(directoryArg);
  const commit = commitArg ?? execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();
  if (command === "generate") writeMetadata(directory, commit);
  else verifyMetadata(directory, commit);
  process.stdout.write(`release metadata ${command}d: ${basename(directory)} @ ${commit}\n`);
}

if (process.argv[1] && import.meta.url === new URL(process.argv[1], "file:").href) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
