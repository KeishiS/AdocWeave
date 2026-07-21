import { readFileSync } from "node:fs";
import process from "node:process";

const ROOT = new URL("../", import.meta.url);
const read = (path) => readFileSync(new URL(path, ROOT), "utf8");
const json = (path) => JSON.parse(read(path));
const fail = (message) => {
  throw new Error(message);
};

export const STABLE_TAG = /^v(\d+\.\d+\.\d+)$/;
export const RC_TAG = /^v(\d+\.\d+\.\d+-rc\.[1-9]\d*)$/;

export function versionFromTag(tag) {
  const match = STABLE_TAG.exec(tag) ?? RC_TAG.exec(tag);
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
    assets.push({
      name: `adocweave-cli-${target}.tar.xz`,
      kind: "cli",
      target,
      archive: "tar.xz",
      executable: "adocweave",
    });
  }
  for (const target of targets) {
    assets.push({
      name: `adocweave-lsp-${target}.tar.xz`,
      kind: "lsp",
      target,
      archive: "tar.xz",
      executable: "adocweave-lsp",
    });
  }
  assets.push({
    name: `adocweave-browser-${version}.tar.xz`,
    kind: "browser",
    target: null,
    archive: "tar.xz",
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
    if (asset.kind === "browser") {
      if (actual.kind !== "extra-artifact") fail("browser archive must be a dist extra artifact");
      continue;
    }
    if (actual.kind !== "executable-zip") fail(`native archive has unexpected dist kind: ${name}`);
    if (JSON.stringify(actual.target_triples) !== JSON.stringify([asset.target])) {
      fail(`native archive target mismatch: ${name}`);
    }
    const executables = actual.assets.filter((entry) => entry.kind === "executable").map((entry) => entry.name);
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
    fail("dist plan contains an unplanned public archive");
  }

  const runnerByTarget = Object.fromEntries(
    distPlan.ci.github.artifacts_matrix.include.map((entry) => [entry.targets[0], entry.runner]),
  );
  const expectedRunners = {
    "aarch64-unknown-linux-musl": "ubuntu-24.04-arm",
    "x86_64-unknown-linux-musl": "ubuntu-24.04",
  };
  if (JSON.stringify(runnerByTarget) !== JSON.stringify(expectedRunners)) {
    fail("dist plan runner matrix must use native Ubuntu 24.04 hosts");
  }
}

export function validateDistributionManifest(manifest, plan) {
  const keys = Object.keys(manifest).sort();
  const expectedKeys = ["assets", "contracts", "packageVersion", "schemaVersion", "sourceCommit"];
  if (JSON.stringify(keys) !== JSON.stringify(expectedKeys)) fail("distribution manifest has unknown or missing fields");
  if (manifest.schemaVersion !== 1) fail("distribution manifest schemaVersion must be 1");
  if (manifest.packageVersion !== plan.packageVersion) fail("distribution manifest package version mismatch");
  if (!/^[0-9a-f]{40}$/.test(manifest.sourceCommit)) fail("sourceCommit must be a lowercase 40-character Git commit");
  const contractKeys = ["conformance", "coreApi", "coreProfile", "html", "projection", "wasmApi"];
  if (JSON.stringify(Object.keys(manifest.contracts).sort()) !== JSON.stringify(contractKeys)) {
    fail("distribution manifest contract fields mismatch");
  }
  for (const value of Object.values(manifest.contracts)) {
    if (!Number.isInteger(value) || value < 1) fail("contract versions must be positive integers");
  }
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

function tomlValue(source, key) {
  const match = source.match(new RegExp(`^${key.replaceAll("-", "\\-")}\\s*=\\s*"([^"]+)"`, "m"));
  return match?.[1] ?? fail(`missing TOML field: ${key}`);
}

function verifyRepository() {
  const cargo = read("Cargo.toml");
  const manifest = json("release-manifest.json");
  const plan = json("release/distribution-plan.json");
  const worker = json("web-worker/package.json");
  const extension = read("editors/zed/extension.toml");
  const extensionCargo = read("editors/zed/Cargo.toml");
  const dist = read("dist-workspace.toml");
  const releaseWorkflow = read(".github/workflows/release.yml");
  const nativeSmokeWorkflow = read(".github/workflows/native-artifact-smoke.yml");
  const version = tomlValue(cargo, "version");
  const repository = tomlValue(cargo, "repository");

  for (const profileSetting of ['lto = "thin"', "codegen-units = 1", "debug = 0", 'panic = "abort"', 'strip = "symbols"']) {
    if (!cargo.includes(profileSetting)) fail(`dist profile is missing: ${profileSetting}`);
  }

  const planKeys = ["assets", "distVersion", "packageVersion", "releaseMetadata", "repository", "schemaVersion", "targets"];
  if (JSON.stringify(Object.keys(plan).sort()) !== JSON.stringify(planKeys) || plan.schemaVersion !== 1) {
    fail("distribution plan schema mismatch");
  }

  for (const [name, actual] of [
    ["release manifest", manifest.packageVersion],
    ["distribution plan", plan.packageVersion],
    ["browser package", worker.version],
    ["Zed extension", tomlValue(extension, "version")],
    ["Zed crate", tomlValue(extensionCargo, "version")],
  ]) {
    if (actual !== version) fail(`${name} version ${actual} does not match workspace ${version}`);
  }
  if (plan.repository !== repository || tomlValue(extension, "repository") !== repository) {
    fail("repository URL mismatch in release train");
  }
  if (plan.distVersion !== "0.32.0" || !dist.includes('cargo-dist-version = "0.32.0"')) {
    fail("dist must be pinned to 0.32.0");
  }
  const browserArchive = `target/distrib/adocweave-browser-${version}.tar.xz`;
  if (!dist.includes(`artifacts = ["${browserArchive}"]`) || !dist.includes('build = ["bash", "tools/package-browser-release.sh"]')) {
    fail("browser package must be connected as the versioned dist extra artifact");
  }
  if (!dist.includes('plan-jobs = ["./release-contract"]')) fail("release contract must run in the dist plan phase");
  if (!dist.includes('pr-run-mode = "upload"') || !dist.includes('global-artifacts-jobs = ["./native-artifact-smoke"]')) {
    fail("PR native artifacts must be smoke tested after local builds");
  }
  if (!releaseWorkflow.includes("needs:\n      - plan\n      - build-local-artifacts") ||
      !releaseWorkflow.includes("uses: ./.github/workflows/native-artifact-smoke.yml")) {
    fail("generated release workflow does not gate on native archive smoke tests");
  }
  for (const runner of ["ubuntu-24.04", "ubuntu-24.04-arm"]) {
    if (!nativeSmokeWorkflow.includes(`runner: ${runner}`)) fail(`native smoke workflow is missing ${runner}`);
  }
  for (const runner of [
    'global = "ubuntu-24.04"',
    'aarch64-unknown-linux-musl = "ubuntu-24.04-arm"',
    'x86_64-unknown-linux-musl = "ubuntu-24.04"',
  ]) {
    if (!dist.includes(runner)) fail(`dist runner mapping is missing: ${runner}`);
  }
  const targets = ["aarch64-unknown-linux-musl", "x86_64-unknown-linux-musl"];
  if (JSON.stringify(plan.targets) !== JSON.stringify(targets)) fail("initial target matrix must contain only two sorted Linux musl targets");
  if (JSON.stringify(plan.assets) !== JSON.stringify(expectedAssets(version, targets))) fail("distribution asset plan is not canonical");
  if (JSON.stringify(plan.releaseMetadata) !== JSON.stringify(EXPECTED_RELEASE_METADATA)) {
    fail("release metadata asset plan is not canonical");
  }
  if (!cargo.includes("publish = false") || worker.private !== true || !extensionCargo.includes("publish = false")) {
    fail("non-GitHub package registries must remain disabled");
  }

  for (const crate of ["adocweave", "adocweave-cli", "adocweave-host", "adocweave-lsp", "adocweave-wasm"]) {
    const crateManifest = read(`crates/${crate}/Cargo.toml`);
    for (const inherited of ["version", "license", "homepage", "repository", "publish"]) {
      if (!crateManifest.includes(`${inherited}.workspace = true`)) fail(`${crate} does not inherit ${inherited}`);
    }
  }

  const fixtureText = read("release/adocweave-dist-manifest.fixture.json");
  const fixture = JSON.parse(fixtureText);
  validateDistributionManifest(fixture, plan);
  if (fixtureText !== canonicalJson(fixture)) fail("distribution manifest fixture is not canonical JSON");
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
