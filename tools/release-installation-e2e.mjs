import * as nodeProcessControl from "node:child_process";
import * as nodeFileSystem from "node:fs";
import { tmpdir } from "node:os";
import nodePath from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";
import {
  TEMPORARY_DIRECTORY_REMOVAL_OPTIONS,
  archiveEntries,
  createRuntimeAdapters,
  executableNames,
  installationLayout,
  isPathInside,
  missingInstallationAssets,
  requiredInstallationAssets,
  selectedInstallationFamilies,
  validateArchiveEntries,
  vscodePackageContract,
} from "./platform-contract.mjs";

const runtime = createRuntimeAdapters({
  fileSystem: nodeFileSystem,
  processControl: nodeProcessControl,
  time: { clearTimeout, now: Date.now, setTimeout },
  platform: {
    architecture: process.arch,
    environment: process.env,
    os: process.platform,
  },
  pathApi: nodePath,
});
const {
  copyFileSync,
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  readlinkSync,
  realpathSync,
  renameSync,
  rmdirSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} = runtime.fileSystem;
const { execFileSync } = runtime.processControl;
const { basename, delimiter, dirname, join, resolve } = runtime.pathApi;

const [candidateArgument, target, manifestArgument, installationScope] = process.argv.slice(2);
if (!candidateArgument || !target) {
  process.stderr.write(
    "usage: node tools/release-installation-e2e.mjs CANDIDATE_DIRECTORY TARGET [MANIFEST] [SCOPE]\n",
  );
  process.exit(2);
}
const installationFamilies = selectedInstallationFamilies(installationScope);

const distributionPlan = JSON.parse(
  readFileSync(new URL("../release/distribution-plan.json", import.meta.url), "utf8"),
);
const platform = distributionPlan.targets.find(({ triple }) => triple === target);
if (!platform) throw new Error(`unsupported installation target: ${target}`);
if (runtime.platform.os !== platform.os || runtime.platform.architecture !== platform.architecture) {
  throw new Error(`installation host ${runtime.platform.architecture} does not match ${target}`);
}

const candidate = realpathSync(resolve(candidateArgument));
const manifestPath = manifestArgument
  ? resolve(manifestArgument)
  : join(candidate, "adocweave-dist-manifest.json");
const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
const version = manifest.packageVersion;
if (typeof version !== "string" || !version) throw new Error("manifest has no packageVersion");
const requiredAssets = requiredInstallationAssets(
  installationScope,
  target,
  version,
  platform.archive,
);
const missingAssets = missingInstallationAssets(
  readdirSync(candidate, { withFileTypes: true })
    .filter((entry) => entry.isFile())
    .map((entry) => entry.name),
  requiredAssets,
);
if (missingAssets.length > 0) {
  throw new Error(`missing release asset: ${missingAssets[0]}`);
}

const scratch = mkdtempSync(join(tmpdir(), "adocweave-installation-e2e-"));
const home = join(scratch, "home");
const prefix = join(home, ".local");
const {
  activeMarker,
  binDirectory,
  browserRoot,
  currentLink,
  versionRoot,
  vscodeRoot,
  zedRoot,
} = installationLayout(prefix, version, runtime.pathApi);
const previousRoot = join(prefix, "lib", "adocweave", `${version}-previous-fixture`);

function files(directory) {
  if (!existsSync(directory)) return [];
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return files(path);
    return [path];
  });
}

function assertInside(root, candidatePath) {
  const resolvedRoot = realpathSync(root);
  const resolvedPath = realpathSync(candidatePath);
  if (!isPathInside(resolvedRoot, resolvedPath, runtime.pathApi)) {
    throw new Error(`${candidatePath} escapes ${root}`);
  }
}

function archive(name) {
  const path = join(candidate, name);
  if (!existsSync(path)) throw new Error(`missing release asset: ${name}`);
  assertInside(candidate, path);
  return path;
}

function extract(path, destination, expectedRoot, archiveType = "tar.xz") {
  const listing = archiveType === "zip"
    ? execFileSync("unzip", ["-Z1", path], { encoding: "utf8" })
    : execFileSync("tar", ["-tJf", path], { encoding: "utf8" });
  const entries = archiveEntries(listing);
  if (entries.length === 0) throw new Error(`empty release archive: ${basename(path)}`);
  if (
    validateArchiveEntries(entries, expectedRoot).length > 0
  ) {
    throw new Error(`unsafe or unexpected archive path in ${basename(path)}`);
  }
  mkdirSync(destination, { recursive: true });
  if (archiveType === "zip") {
    execFileSync("unzip", ["-q", path, "-d", destination]);
  } else {
    execFileSync("tar", ["-xJf", path, "-C", destination]);
  }
  const root = expectedRoot ? join(destination, expectedRoot) : destination;
  assertInside(destination, root);
  return root;
}

function atomicLink(targetPath, linkPath) {
  mkdirSync(dirname(linkPath), { recursive: true });
  const staging = `${linkPath}.new`;
  rmSync(staging, { force: true });
  symlinkSync(targetPath, staging);
  renameSync(staging, linkPath);
}

function command(name, args = []) {
  return execFileSync(name, args, {
    encoding: "utf8",
    env: {
      HOME: home,
      PATH: [binDirectory, runtime.platform.environment.PATH].filter(Boolean).join(delimiter),
      SystemRoot: runtime.platform.environment.SystemRoot,
      XDG_CACHE_HOME: join(home, ".cache"),
      XDG_CONFIG_HOME: join(home, ".config"),
      XDG_DATA_HOME: join(home, ".local", "share"),
    },
  });
}

function activateNative(root, label, executables) {
  for (const executable of executables) {
    if (!existsSync(join(root, "bin", executable))) {
      throw new Error(`cannot activate incomplete native version: ${label}`);
    }
  }
  if (runtime.platform.os === "win32") {
    mkdirSync(binDirectory, { recursive: true });
    for (const executable of executables) {
      const destination = join(binDirectory, executable);
      const staging = `${destination}.new`;
      const backup = `${destination}.previous`;
      copyFileSync(join(root, "bin", executable), staging);
      rmSync(backup, { force: true });
      if (existsSync(destination)) renameSync(destination, backup);
      try {
        renameSync(staging, destination);
      } catch (error) {
        if (existsSync(backup)) renameSync(backup, destination);
        throw error;
      }
      rmSync(backup, { force: true });
    }
  } else {
    atomicLink(root, currentLink);
    for (const executable of executables) {
      const link = join(binDirectory, executable);
      if (!existsSync(link)) atomicLink(join(currentLink, "bin", executable), link);
    }
  }
  const markerStaging = `${activeMarker}.new`;
  writeFileSync(markerStaging, `${label}\n`);
  renameSync(markerStaging, activeMarker);
}

function installNative(packageName, executable) {
  const archiveRoot = `${packageName}-${target}`;
  const extracted = extract(
    archive(`${archiveRoot}.${platform.archive}`),
    join(scratch, "extract", packageName),
    null,
    platform.archive,
  );
  const destination = join(versionRoot, "bin", executable);
  mkdirSync(dirname(destination), { recursive: true });
  copyFileSync(join(extracted, executable), destination);
  if (runtime.platform.os !== "win32") execFileSync("chmod", ["755", destination]);
}

function installBrowser() {
  const archiveRoot = `adocweave-browser-${version}`;
  const extracted = extract(
    archive(`${archiveRoot}.tar.xz`),
    join(scratch, "extract", "browser"),
    archiveRoot,
  );
  mkdirSync(dirname(browserRoot), { recursive: true });
  renameSync(extracted, browserRoot);
}

function installZed() {
  const archiveRoot = `adocweave-zed-${version}`;
  const extracted = extract(
    archive(`${archiveRoot}.tar.xz`),
    join(scratch, "extract", "zed"),
    archiveRoot,
  );
  mkdirSync(dirname(zedRoot), { recursive: true });
  renameSync(extracted, zedRoot);
}

function installVSCode() {
  const name = `adocweave-vscode-${version}.vsix`;
  const extracted = extract(
    archive(name),
    join(scratch, "extract", "vscode"),
    null,
    "zip",
  );
  const extension = join(extracted, "extension");
  assertInside(extracted, extension);
  mkdirSync(dirname(vscodeRoot), { recursive: true });
  renameSync(extension, vscodeRoot);
}

async function verifyBrowserContract() {
  const modulePath = join(browserRoot, "wasm", "adocweave_wasm.js");
  const wasmPath = join(browserRoot, "wasm", "adocweave_wasm_bg.wasm");
  const contracts = await import(pathToFileURL(join(browserRoot, "worker", "contracts.mjs")));
  const wasm = await import(pathToFileURL(modulePath));
  await wasm.default({ module_or_path: readFileSync(wasmPath) });

  const empty = "xref:record:item[]";
  const authored = "xref:record:explicit[Authored *label*]";
  const failed = "xref:record:private[]";
  const source = `${empty}\n\n${authored}\n\n${failed}`;
  const ranges = [empty, authored, failed].map((text) => {
    const sourceStart = source.indexOf(text);
    return { sourceStart, sourceEnd: sourceStart + Buffer.byteLength(text) };
  });
  const response = wasm.process({
    packageVersion: contracts.PACKAGE_VERSION,
    sourceId: "acceptance:resolved-display-text",
    version: 1,
    generation: 1,
    source,
    renderInputs: {
      references: [
        {
          ...ranges[0],
          outcome: {
            status: "resolved",
            href: "/records/item",
            displayText: "<Public & *plain*>",
          },
        },
        {
          ...ranges[1],
          outcome: {
            status: "resolved",
            href: "/records/explicit",
            displayText: "must not replace authored label",
          },
        },
        {
          ...ranges[2],
          outcome: { status: "failed", kind: "missing-target" },
        },
      ],
    },
    renderPolicy: {
      activeUrls: { allowResolvedRootRelative: true },
      unresolvedReferences: "label-only",
    },
  });

  const expected =
    '<p><a href="/records/item">&lt;Public &amp; *plain*&gt;</a></p>\n' +
    '<p><a href="/records/explicit">Authored <strong>label</strong></a></p>\n' +
    "<p></p>\n";
  if (response.html !== expected) throw new Error(`browser resolved text mismatch: ${response.html}`);
  const edges = response.projection.referenceEdges;
  if (edges[0].resolution.displayText !== "<Public & *plain*>") {
    throw new Error("browser projection omitted resolved display text");
  }
  const failure = edges[2].resolution;
  if (
    failure.status !== "failed" ||
    failure.kind !== "missing-reference-target" ||
    Object.keys(failure).sort().join(",") !== "kind,status"
  ) {
    throw new Error(`browser failure projection is not kind-only: ${JSON.stringify(failure)}`);
  }
}

try {
  mkdirSync(home);
  const before = files(home).map((path) => path.slice(home.length + 1));

  if (installationFamilies.native) {
    installNative("adocweave-cli", `adocweave${platform.executableSuffix}`);
    installNative("adocweave-lsp", `adocweave-lsp${platform.executableSuffix}`);
  }
  if (installationFamilies.global) {
    installBrowser();
    installZed();
    installVSCode();
  }
  const executables = executableNames(platform.executableSuffix);
  if (installationFamilies.native) {
    cpSync(versionRoot, previousRoot, { recursive: true });
    activateNative(versionRoot, version, executables);
    if (runtime.platform.os !== "win32" && readlinkSync(currentLink) !== versionRoot) {
      throw new Error("current version link is not pinned");
    }

    for (const executable of executables) {
      const actual = JSON.parse(command(executable, ["--version", "--json"]));
      if (actual.packageVersion !== version) throw new Error(`${executable} version mismatch`);
    }
    activateNative(previousRoot, `${version}-previous-fixture`, executables);
    if (readFileSync(activeMarker, "utf8") !== `${version}-previous-fixture\n`) {
      throw new Error("native rollback did not select the previous version");
    }
    activateNative(versionRoot, version, executables);
    try {
      activateNative(join(prefix, "lib", "adocweave", "incomplete"), "incomplete", executables);
      throw new Error("incomplete native update was accepted");
    } catch (error) {
      if (error instanceof Error && error.message === "incomplete native update was accepted") {
        throw error;
      }
    }
    if (readFileSync(activeMarker, "utf8") !== `${version}\n`) {
      throw new Error("failed native update changed the active version");
    }
  }
  if (installationFamilies.global) {
    if (!existsSync(join(browserRoot, "worker", "index.mjs"))) throw new Error("browser public entry point is missing");
    if (!existsSync(join(browserRoot, "wasm", "adocweave_wasm_bg.wasm"))) throw new Error("browser WASM is missing");
    if (!existsSync(join(zedRoot, "extension.toml"))) throw new Error("Zed extension manifest is missing");
    const vscodeManifest = JSON.parse(readFileSync(join(vscodeRoot, "package.json"), "utf8"));
    if (!vscodePackageContract(vscodeManifest, version)) {
      throw new Error("VS Code extension manifest mismatch");
    }
    await verifyBrowserContract();
  }

  if (installationFamilies.native) {
    for (const executable of executables) rmSync(join(binDirectory, executable));
    if (runtime.platform.os !== "win32") rmSync(currentLink);
    rmSync(activeMarker);
    rmSync(versionRoot, { recursive: true });
    rmSync(previousRoot, { recursive: true });
  }
  if (installationFamilies.global) {
    rmSync(join(prefix, "share", "adocweave", version), { recursive: true });
  }
  for (const directory of [
    join(prefix, "share", "adocweave"),
    join(prefix, "share"),
    join(prefix, "lib", "adocweave"),
    join(prefix, "lib"),
    binDirectory,
    prefix,
  ]) {
    if (existsSync(directory) && readdirSync(directory).length === 0) rmdirSync(directory);
  }

  const after = files(home).map((path) => path.slice(home.length + 1));
  if (JSON.stringify(after) !== JSON.stringify(before)) {
    throw new Error(`managed files remain after uninstall: ${after.join(", ")}`);
  }
  process.stdout.write(`release installation E2E passed: ${version} ${target}\n`);
} finally {
  rmSync(scratch, TEMPORARY_DIRECTORY_REMOVAL_OPTIONS);
}
