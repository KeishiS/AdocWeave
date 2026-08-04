import { readFileSync } from "node:fs";
import process from "node:process";

import { PUBLIC_PROTOCOL_SCHEMA_VERSION } from "./release-policy.mjs";
import { loadWorkflowPolicyInputs, validateReleaseWorkflowPolicy } from "./release-workflow-policy.mjs";

const ROOT = new URL("../", import.meta.url);
const read = (path) => readFileSync(new URL(path, ROOT), "utf8");
const json = (path) => JSON.parse(read(path));
const fail = (message) => {
  throw new Error(message);
};

export const STABLE_TAG = /^v(\d+\.\d+\.\d+)$/;
export const SUPPORTED_PUBLIC_PROTOCOL_SCHEMA_VERSION = PUBLIC_PROTOCOL_SCHEMA_VERSION;

export function versionFromTag(tag) {
  const match = STABLE_TAG.exec(tag);
  if (!match) {
    fail(`unsupported release tag: ${tag}`);
  }
  return match[1];
}

export function canonicalJson(value) {
  const normalize = (entry) => {
    if (Array.isArray(entry)) return entry.map(normalize);
    if (entry && typeof entry === "object") {
      return Object.fromEntries(
        Object.keys(entry)
          .sort()
          .map((key) => [key, normalize(entry[key])]),
      );
    }
    return entry;
  };
  return `${JSON.stringify(normalize(value), null, 2)}\n`;
}

export function expectedAssets(version, targets) {
  const assets = [];
  for (const target of targets) {
    const executable = `adocweave${target.executableSuffix}`;
    assets.push({
      name: `adocweave-cli-${target.triple}.${target.archive}`,
      kind: "cli",
      target: target.triple,
      archive: target.archive,
      executable,
    });
  }
  for (const target of targets) {
    const executable = `adocweave-lsp${target.executableSuffix}`;
    assets.push({
      name: `adocweave-lsp-${target.triple}.${target.archive}`,
      kind: "lsp",
      target: target.triple,
      archive: target.archive,
      executable,
    });
  }
  assets.push({
    name: `adocweave-browser-${version}.tar.xz`,
    kind: "browser",
    target: null,
    archive: "tar.xz",
    executable: null,
  });
  assets.push({
    name: `adocweave-zed-${version}.tar.xz`,
    kind: "zed",
    target: null,
    archive: "tar.xz",
    executable: null,
  });
  assets.push({
    name: `adocweave-textlint-plugin-asciidoc-${version}.tgz`,
    kind: "textlint-plugin",
    target: null,
    archive: "tgz",
    executable: null,
  });
  assets.push({
    name: `adocweave-vscode-${version}.vsix`,
    kind: "vscode",
    target: null,
    archive: "vsix",
    executable: null,
  });
  return assets;
}

export const EXPECTED_RELEASE_METADATA = [
  { name: "adocweave-dist-manifest.json", kind: "distribution-manifest", format: "canonical-json" },
  { name: "adocweave.spdx.json", kind: "sbom", format: "spdx-json" },
  { name: "sha256.sum", kind: "checksums", format: "sha256" },
];

export function validateDistPlan(distPlan, plan, tag) {
  if (distPlan.dist_version !== plan.distVersion) fail("dist plan version mismatch");
  if (distPlan.announcement_tag !== tag) fail("dist announcement tag mismatch");
  if (versionFromTag(tag) !== plan.packageVersion) fail("dist tag and package version mismatch");

  const releases = new Map(distPlan.releases.map((release) => [release.app_name, release]));
  if (releases.size !== 2 || !releases.has("adocweave-cli") || !releases.has("adocweave-lsp")) {
    fail("dist plan must announce exactly the CLI and LSP packages");
  }
  for (const release of releases.values()) {
    if (release.app_version !== plan.packageVersion) fail(`dist release version mismatch: ${release.app_name}`);
  }

  const planned = new Map(plan.assets.map((asset) => [asset.name, asset]));
  for (const [name, asset] of planned) {
    const actual = distPlan.artifacts[name];
    if (!actual) fail(`dist plan is missing public artifact: ${name}`);
    if (["browser", "textlint-plugin", "zed", "vscode"].includes(asset.kind)) {
      if (actual.kind !== "extra-artifact") fail(`${asset.kind} archive must be a dist extra artifact`);
      continue;
    }
    if (actual.kind !== "executable-zip") fail(`native archive has unexpected dist kind: ${name}`);
    if (JSON.stringify(actual.target_triples) !== JSON.stringify([asset.target])) {
      fail(`native archive target mismatch: ${name}`);
    }
    const executables = actual.assets.filter((entry) => entry.kind === "executable").map((entry) => entry.path);
    if (JSON.stringify(executables) !== JSON.stringify([asset.executable])) {
      fail(`native archive executable mismatch: ${name}`);
    }
    const misc = actual.assets.filter((entry) => entry.kind !== "executable").map((entry) => entry.name).sort();
    if (JSON.stringify(misc) !== JSON.stringify(["LICENSE-APACHE", "LICENSE-MIT", "README.adoc", "THIRD_PARTY_NOTICES.adoc"])) {
      fail(`native archive documentation mismatch: ${name}`);
    }
  }

  const publicArchives = Object.values(distPlan.artifacts)
    .filter((artifact) => artifact.kind === "executable-zip" || artifact.kind === "extra-artifact")
    .map((artifact) => artifact.name)
    .sort();
  if (JSON.stringify(publicArchives) !== JSON.stringify([...planned.keys()].sort())) {
    fail("dist plan contains a missing or unplanned public archive");
  }

  const runnerByTarget = Object.fromEntries(
    distPlan.ci.github.artifacts_matrix.include.map((entry) => [entry.targets[0], entry.runner]),
  );
  const expectedRunners = Object.fromEntries(
    plan.targets
      .map((target) => [target.triple, target.runner])
      .sort(([left], [right]) => left.localeCompare(right)),
  );
  if (JSON.stringify(runnerByTarget) !== JSON.stringify(expectedRunners)) {
    fail("dist plan runner matrix must use the declared native hosts");
  }
}

export function validateDistributionManifest(manifest, plan) {
  const keys = Object.keys(manifest).sort();
  const expectedKeys = ["assets", "packageVersion", "schemaVersion", "sourceCommit"];
  if (JSON.stringify(keys) !== JSON.stringify(expectedKeys)) fail("distribution manifest has unknown or missing fields");
  if (manifest.schemaVersion !== 2) fail("distribution manifest schemaVersion must be 2");
  if (manifest.packageVersion !== plan.packageVersion) fail("distribution manifest package version mismatch");
  if (!/^[0-9a-f]{40}$/.test(manifest.sourceCommit)) fail("sourceCommit must be a lowercase 40-character Git commit");
  const expected = new Map(plan.assets.map((asset) => [asset.name, asset]));
  const names = manifest.assets.map((asset) => asset.name);
  if (new Set(names).size !== names.length || names.some((name, index) => index && name < names[index - 1])) {
    fail("distribution assets must have unique names sorted by name");
  }
  if (names.length !== expected.size) fail("distribution manifest asset count mismatch");
  for (const asset of manifest.assets) {
    const planned = expected.get(asset.name);
    if (!planned) fail(`unplanned distribution asset: ${asset.name}`);
    for (const field of ["kind", "target", "archive", "executable"]) {
      if (asset[field] !== planned[field]) fail(`asset ${asset.name} has invalid ${field}`);
    }
    if (!Number.isInteger(asset.byteSize) || asset.byteSize < 1) fail(`asset ${asset.name} has invalid byteSize`);
    if (!/^[0-9a-f]{64}$/.test(asset.sha256)) fail(`asset ${asset.name} has invalid sha256`);
  }
}

export function validateReleaseTrainVersions(version, components) {
  for (const [name, actual] of Object.entries(components)) {
    if (actual !== version) fail(`${name} version ${actual} does not match workspace ${version}`);
  }
}

export function validatePublicClientReleaseContract(version, vscodePackage, vscodeLock, protocol) {
  validateReleaseTrainVersions(version, {
    "VS Code package": vscodePackage.version,
    "VS Code package lock": vscodeLock.version,
    "VS Code package lock root": vscodeLock.packages?.[""]?.version,
    "public protocol": protocol.packageVersion,
  });
  if (vscodePackage.private !== true) {
    fail("VS Code package must remain private");
  }
  if (vscodeLock.lockfileVersion !== 3) {
    fail("VS Code package lockfileVersion must be 3");
  }
  if (protocol.schemaVersion !== SUPPORTED_PUBLIC_PROTOCOL_SCHEMA_VERSION) {
    fail(
      `public protocol schemaVersion must be ${SUPPORTED_PUBLIC_PROTOCOL_SCHEMA_VERSION}`,
    );
  }
}

function tomlValue(source, key) {
  const match = source.match(new RegExp(`^${key.replaceAll("-", "\\-")}\\s*=\\s*"([^"]+)"`, "m"));
  return match?.[1] ?? fail(`missing TOML field: ${key}`);
}

function verifyRepository() {
  const cargo = read("Cargo.toml");
  const manifest = json("release-manifest.json");
  const plan = json("release/distribution-plan.json");
  const platformFixture = json("release/platform-selection.fixture.json");
  const vscodePlatforms = json("editors/vscode/resources/platforms.json");
  const vscodePackage = json("editors/vscode/package.json");
  const vscodeLock = json("editors/vscode/package-lock.json");
  const protocol = json("protocol/public-api.json");
  const conformance = json("crates/adocweave/conformance/cases.json");
  const publicConformance = json("fixtures/public-conformance.json");
  const worker = json("web-worker/package.json");
  const textlintPlugin = json("packages/textlint-plugin-asciidoc/package.json");
  const extension = read("editors/zed/extension.toml");
  const extensionCargo = read("editors/zed/Cargo.toml");
  const dist = read("dist-workspace.toml");
  const makefile = read("Makefile.toml");
  const releaseWorkflow = read(".github/workflows/release.yml");
  const nativeSmokeWorkflow = read(".github/workflows/native-artifact-smoke.yml");
  const version = tomlValue(cargo, "version");
  const repository = tomlValue(cargo, "repository");

  if (/mktemp(?:\s+-d)?\s+(?:["'])?target\//.test(makefile)) {
    fail("cargo-make tasks must not require target to exist before creating temporary files");
  }

  for (const profileSetting of ['lto = "thin"', "codegen-units = 1", "debug = 0", 'panic = "abort"', 'strip = "symbols"']) {
    if (!cargo.includes(profileSetting)) fail(`dist profile is missing: ${profileSetting}`);
  }

  const planKeys = ["assets", "distVersion", "packageVersion", "releaseMetadata", "repository", "schemaVersion", "targets"];
  if (JSON.stringify(Object.keys(plan).sort()) !== JSON.stringify(planKeys) || plan.schemaVersion !== 2) {
    fail("distribution plan schema mismatch");
  }
  const targetKeys = [
    "archive",
    "architecture",
    "executableSuffix",
    "linkage",
    "minimumOsVersion",
    "os",
    "runner",
    "triple",
  ].sort();
  const triples = plan.targets.map((target) => target.triple);
  if (new Set(triples).size !== triples.length) {
    fail("distribution targets must have unique triples");
  }
  for (const target of plan.targets) {
    if (JSON.stringify(Object.keys(target).sort()) !== JSON.stringify(targetKeys)) {
      fail(`distribution target ${target.triple ?? "<unknown>"} has unknown or missing fields`);
    }
    if (!["linux", "darwin", "win32"].includes(target.os) ||
        !["arm64", "x64"].includes(target.architecture) ||
        !["tar.xz", "zip"].includes(target.archive) ||
        !["", ".exe"].includes(target.executableSuffix) ||
        !["static", "system"].includes(target.linkage)) {
      fail(`distribution target ${target.triple} has an invalid platform contract`);
    }
    if (target.archive !== "zip" ||
        (target.os === "win32") !== (target.executableSuffix === ".exe")) {
      fail(`distribution target ${target.triple} has an invalid archive contract`);
    }
  }

  validateReleaseTrainVersions(version, {
    "release manifest": manifest.packageVersion,
    "distribution plan": plan.packageVersion,
    "browser package": worker.version,
    "textlint plugin package": textlintPlugin.version,
    "cross-runtime conformance manifest": conformance.packageVersion,
    "public conformance manifest": publicConformance.packageVersion,
    "Zed extension": tomlValue(extension, "version"),
    "Zed crate": tomlValue(extensionCargo, "version"),
  });
  validatePublicClientReleaseContract(version, vscodePackage, vscodeLock, protocol);
  if (plan.repository !== repository || tomlValue(extension, "repository") !== repository) {
    fail("repository URL mismatch in release train");
  }
  if (plan.distVersion !== "0.32.0" || !dist.includes('cargo-dist-version = "0.32.0"')) {
    fail("dist must be pinned to 0.32.0");
  }
  if (!dist.includes('checksum = "false"')) {
    fail("dist per-archive checksums must be disabled in favor of the canonical checksum list");
  }
  if (!dist.includes('unix-archive = ".zip"') || !dist.includes('windows-archive = ".zip"')) {
    fail("native archives must use one ZIP contract on every platform");
  }
  const browserArchive = `target/distrib/adocweave-browser-${version}.tar.xz`;
  if (!dist.includes(`artifacts = ["${browserArchive}"]`) ||
      !dist.includes('build = ["bash", "tools/package-browser-release.sh"]')) {
    fail("browser package must be connected as the versioned dist extra artifact");
  }
  const zedArchive = `target/distrib/adocweave-zed-${version}.tar.xz`;
  if (!dist.includes(`artifacts = ["${zedArchive}"]`) ||
      !dist.includes('build = ["bash", "tools/package-zed-release.sh"]')) {
    fail("Zed package must be connected as the versioned dist extra artifact");
  }
  const textlintPluginArchive = `target/distrib/adocweave-textlint-plugin-asciidoc-${version}.tgz`;
  if (!dist.includes(`artifacts = ["${textlintPluginArchive}"]`) ||
      !dist.includes('build = ["bash", "tools/package-textlint-plugin-release.sh"]')) {
    fail("textlint plugin must be connected as the versioned dist extra artifact");
  }
  const vscodeArchive = `target/distrib/adocweave-vscode-${version}.vsix`;
  if (!dist.includes(`artifacts = ["${vscodeArchive}"]`) ||
      !dist.includes('build = ["bash", "tools/package-vscode-release.sh"]')) {
    fail("VS Code package must be connected as the versioned dist extra artifact");
  }
  if (!dist.includes('plan-jobs = ["./release-contract"]')) fail("release contract must run in the dist plan phase");
  if (!dist.includes('pr-run-mode = "plan"') || !dist.includes('global-artifacts-jobs = ["./native-artifact-smoke"]')) {
    fail("PRs must stop after planning while pushed candidates use the native smoke workflow");
  }
  if (!releaseWorkflow.includes("needs: [changes, build-native]") ||
      !releaseWorkflow.includes("uses: ./.github/workflows/native-artifact-smoke.yml")) {
    fail("release workflow does not gate on native archive smoke tests");
  }
  if (!releaseWorkflow.includes("nix develop .#ci-browser -c cargo make release-global-candidate")) {
    fail("release workflow must build and runtime-test the exact global archives before upload");
  }
  if (!nativeSmokeWorkflow.includes("matrix: ${{ fromJSON(inputs.matrix) }}") ||
      !releaseWorkflow.includes("node tools/native-change-plan.mjs")) {
    fail("native smoke workflow must consume the locally verified target plan");
  }
  for (const runner of [
    'global = "ubuntu-24.04"',
    ...plan.targets.map((target) => `${target.triple} = "${target.runner}"`),
  ]) {
    if (!dist.includes(runner)) fail(`dist runner mapping is missing: ${runner}`);
  }
  if (JSON.stringify(plan.assets) !== JSON.stringify(expectedAssets(version, plan.targets))) fail("distribution asset plan is not canonical");
  const expectedPlatformSelection = plan.targets.map((target) => ({
    architecture: target.architecture,
    archive: target.archive,
    executable: `adocweave-lsp${target.executableSuffix}`,
    minimumOsVersion: target.minimumOsVersion,
    os: target.os,
    target: target.triple,
  }));
  if (platformFixture.schemaVersion !== 1 ||
      JSON.stringify(platformFixture.supported) !== JSON.stringify(expectedPlatformSelection)) {
    fail("managed client platform fixture does not match the distribution targets");
  }
  if (JSON.stringify(vscodePlatforms) !== JSON.stringify(platformFixture)) {
    fail("VS Code managed platform resource does not match the shared fixture");
  }
  if (JSON.stringify(json("editors/zed/platforms.json")) !== JSON.stringify(platformFixture)) {
    fail("Zed managed platform resource does not match the shared fixture");
  }
  if (JSON.stringify(plan.releaseMetadata) !== JSON.stringify(EXPECTED_RELEASE_METADATA)) {
    fail("release metadata asset plan is not canonical");
  }
  if (!cargo.includes("publish = false") || worker.private !== true || textlintPlugin.private !== true ||
      !extensionCargo.includes("publish = false")) {
    fail("non-GitHub package registries must remain disabled");
  }
  for (const field of ["dependencies", "optionalDependencies", "bundledDependencies"]) {
    const value = textlintPlugin[field];
    if (value && (Array.isArray(value) ? value.length : Object.keys(value).length) !== 0) {
      fail(`textlint plugin must have zero runtime npm dependencies: ${field}`);
    }
  }
  if (textlintPlugin.engines?.node !== ">=20.18.0 <25" ||
      JSON.stringify(textlintPlugin.peerDependencies) !== JSON.stringify({ textlint: "15.8.0" })) {
    fail("textlint plugin runtime compatibility range is not canonical");
  }
  for (const name of ["preinstall", "install", "postinstall", "prepare", "prepack", "postpack"]) {
    if (textlintPlugin.scripts?.[name]) fail(`textlint plugin must not define ${name}`);
  }

  for (const crate of [
    "adocweave",
    "adocweave-cli",
    "adocweave-host",
    "adocweave-lsp",
    "adocweave-textlint",
    "adocweave-textlint-wasm",
    "adocweave-wasm",
  ]) {
    const crateManifest = read(`crates/${crate}/Cargo.toml`);
    for (const inherited of ["version", "license", "homepage", "repository", "publish"]) {
      if (!crateManifest.includes(`${inherited}.workspace = true`)) fail(`${crate} does not inherit ${inherited}`);
    }
  }

  const fixtureText = read("release/adocweave-dist-manifest.fixture.json");
  const fixture = JSON.parse(fixtureText);
  validateDistributionManifest(fixture, plan);
  if (fixtureText !== canonicalJson(fixture)) fail("distribution manifest fixture is not canonical JSON");
  validateReleaseWorkflowPolicy(loadWorkflowPolicyInputs());
  return { version, manifest };
}

export function main(args) {
  const { version } = verifyRepository();
  const tagArg = args.find((arg) => arg.startsWith("--tag="));
  if (tagArg && versionFromTag(tagArg.slice(6)) !== version) fail(`tag version does not match release train ${version}`);
  process.stdout.write(`release contract verified: ${version}\n`);
}

if (process.argv[1] && import.meta.url === new URL(process.argv[1], "file:").href) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
