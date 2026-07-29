import * as nodeProcessControl from "node:child_process";
import * as nodeFileSystem from "node:fs";
import { tmpdir } from "node:os";
import nodePath from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import {
  archiveEntries,
  createRuntimeAdapters,
  macosMinimumVersion,
  unexpectedMacosDependencies,
  importedWindowsDlls,
  unexpectedWindowsDlls,
  validateArchiveEntries,
} from "./platform-contract.mjs";
import {
  createNativeSmokeDeadline,
  removeNativeSmokeDirectory,
  smokeLsp,
} from "./native-lsp-smoke.mjs";

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
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  realpathSync,
  renameSync,
  writeFileSync,
} = runtime.fileSystem;
const { execFileSync, spawn } = runtime.processControl;
const { basename, join, resolve, sep } = runtime.pathApi;

const [artifactDirectory, target] = process.argv.slice(2);
if (!artifactDirectory || !target) {
  process.stderr.write("usage: node tools/native-release-smoke.mjs ARTIFACT_DIRECTORY TARGET\n");
  process.exit(2);
}

const plan = JSON.parse(readFileSync(new URL("../release/distribution-plan.json", import.meta.url), "utf8"));
const platform = plan.targets.find(({ triple }) => triple === target);
if (!platform) throw new Error(`unsupported smoke target: ${target}`);
if (runtime.platform.os !== platform.os || runtime.platform.architecture !== platform.architecture) {
  throw new Error(`smoke host ${runtime.platform.architecture} does not match ${target}`);
}

const manifest = JSON.parse(readFileSync(new URL("../release-manifest.json", import.meta.url), "utf8"));
const workspaceRoot = realpathSync(fileURLToPath(new URL("../", import.meta.url)));
const scratch = mkdtempSync(join(tmpdir(), "adocweave-native-smoke-"));

function filesRecursively(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    return entry.isDirectory() ? filesRecursively(path) : [path];
  });
}

function archive(name) {
  const expected = `${name}-${target}.${platform.archive}`;
  const matches = filesRecursively(resolve(artifactDirectory)).filter((path) => basename(path) === expected);
  if (matches.length !== 1) throw new Error(`expected exactly one ${expected}, found ${matches.length}`);
  return matches[0];
}

function extract(archivePath, executable) {
  const destination = join(scratch, `extract-${executable}`);
  mkdirSync(destination);
  const entries = archiveEntries(platform.archive === "zip"
    ? execFileSync("unzip", ["-Z1", archivePath], { encoding: "utf8" })
    : execFileSync("tar", ["-tJf", archivePath], { encoding: "utf8" }));
  const expectedEntries = [
    "LICENSE-APACHE",
    "LICENSE-MIT",
    "README.adoc",
    "THIRD_PARTY_NOTICES.adoc",
    executable,
  ].sort();
  if (JSON.stringify(entries.sort()) !== JSON.stringify(expectedEntries)) {
    throw new Error(`${basename(archivePath)} has an unexpected archive layout:\n${entries.join("\n")}`);
  }
  if (validateArchiveEntries(entries).length > 0) {
    throw new Error(`${basename(archivePath)} contains an unsafe path`);
  }
  if (platform.archive === "zip") {
    execFileSync("unzip", ["-q", archivePath, "-d", destination]);
  } else {
    execFileSync("tar", ["-xJf", archivePath, "-C", destination]);
  }
  const binary = realpathSync(join(destination, executable));
  if (!binary.startsWith(`${realpathSync(destination)}${sep}`) || binary.startsWith(`${workspaceRoot}${sep}`)) {
    throw new Error(`smoke test selected a binary outside the extracted archive: ${binary}`);
  }
  verifyBinary(binary, executable);
  return binary;
}

function verifyBinary(binary, executable) {
  const bytes = readFileSync(binary);
  if (platform.os === "linux") {
    execFileSync("test", ["-x", binary]);
    const header = execFileSync("readelf", ["-h", binary], { encoding: "utf8" });
    const machine = platform.architecture === "arm64" ? "AArch64" : "Advanced Micro Devices X86-64";
    if (!header.includes(`Machine:                           ${machine}`)) {
      throw new Error(`${executable} has the wrong ELF architecture`);
    }
    const dynamic = execFileSync("readelf", ["-d", binary], { encoding: "utf8" });
    if (/\(NEEDED\)/.test(dynamic) && runtime.platform.environment.ADOCWEAVE_SMOKE_ALLOW_DYNAMIC !== "1") {
      throw new Error(`${executable} has an unexpected dynamic dependency`);
    }
    return;
  }
  if (platform.os === "darwin") {
    execFileSync("test", ["-x", binary]);
    const description = execFileSync("file", ["-b", binary], { encoding: "utf8" });
    const architecture = platform.architecture === "arm64" ? "arm64" : "x86_64";
    if (!description.includes("Mach-O") || !description.includes(architecture)) {
      throw new Error(`${executable} has the wrong Mach-O architecture`);
    }
    const dependencies = execFileSync("otool", ["-L", binary], { encoding: "utf8" });
    if (unexpectedMacosDependencies(dependencies).length > 0) {
      throw new Error(`${executable} has a non-system dynamic dependency`);
    }
    const loadCommands = execFileSync("otool", ["-l", binary], { encoding: "utf8" });
    const minimum = macosMinimumVersion(loadCommands);
    if (minimum !== platform.minimumOsVersion) {
      throw new Error(`${executable} minimum macOS version is ${minimum ?? "unknown"}`);
    }
    execFileSync("xattr", ["-w", "com.apple.quarantine", "0081;00000000;AdocWeave;", binary]);
    const quarantine = execFileSync("xattr", ["-p", "com.apple.quarantine", binary], { encoding: "utf8" });
    if (!quarantine.includes("AdocWeave")) {
      throw new Error(`${executable} quarantine attribute was not applied`);
    }
    return;
  }
  if (bytes.readUInt16LE(0) !== 0x5a4d) throw new Error(`${executable} has no PE header`);
  const peOffset = bytes.readUInt32LE(0x3c);
  if (bytes.toString("ascii", peOffset, peOffset + 4) !== "PE\0\0" || bytes.readUInt16LE(peOffset + 4) !== 0x8664) {
    throw new Error(`${executable} has the wrong PE architecture`);
  }
  const optionalHeader = peOffset + 24;
  if (bytes.readUInt16LE(optionalHeader) !== 0x20b) {
    throw new Error(`${executable} is not a PE32+ executable`);
  }
  const dumpbin = runtime.platform.environment.ADOCWEAVE_DUMPBIN;
  if (!dumpbin) throw new Error("ADOCWEAVE_DUMPBIN is required for Windows dependency verification");
  const dependencies = execFileSync(dumpbin, ["/DEPENDENTS", binary], { encoding: "utf8" });
  const imported = importedWindowsDlls(dependencies);
  const unexpected = unexpectedWindowsDlls(imported);
  if (imported.length === 0 || unexpected.length > 0) {
    throw new Error(`${executable} has unexpected Windows dependencies: ${unexpected.join(", ") || "none detected"}`);
  }
}

function run(binary, args, options = {}) {
  return execFileSync(binary, args, { encoding: "utf8", ...options });
}

function version(binary) {
  const value = JSON.parse(run(binary, ["--version", "--json"]));
  if (value.packageVersion !== manifest.packageVersion) throw new Error(`${value.name} package version mismatch`);
}

async function smokeForcedProcessLifecycle(binary, deadline) {
  const lifecycle = join(scratch, `lifecycle${platform.executableSuffix}`);
  const replaced = `${lifecycle}.replaced`;
  copyFileSync(binary, lifecycle);
  if (platform.os !== "win32") execFileSync("chmod", ["755", lifecycle]);
  const child = spawn(lifecycle, [], { stdio: ["pipe", "pipe", "pipe"] });
  try {
    await deadline.run(new Promise((resolvePromise, reject) => {
      child.once("spawn", resolvePromise);
      child.once("error", reject);
    }), "forced lifecycle process startup", 5_000);
    const exited = new Promise((resolvePromise) => child.once("close", resolvePromise));
    child.kill();
    const exit = await deadline.run(
      exited,
      "forced Language Server stop",
      5_000,
    );
    if (exit === undefined && child.exitCode === null && child.signalCode === null) {
      throw new Error("forced Language Server stop did not report an exit");
    }
  } finally {
    if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
    child.stdin?.destroy?.();
    child.stdout?.destroy?.();
    child.stderr?.destroy?.();
  }
  renameSync(lifecycle, replaced);
  runtime.fileSystem.rmSync(replaced);
}

let smokeDeadline;
let operationError;
try {
  const cli = extract(archive("adocweave-cli"), `adocweave${platform.executableSuffix}`);
  const lsp = extract(archive("adocweave-lsp"), `adocweave-lsp${platform.executableSuffix}`);
  version(cli);
  version(lsp);
  const fixtureRoot = join(scratch, "space 日本語");
  mkdirSync(fixtureRoot);
  const fixture = join(fixtureRoot, "fixture.adoc");
  writeFileSync(fixture, "= Title\r\n\r\ntext\r\n");
  run(cli, ["check", fixture]);
  if (!run(cli, ["convert", fixture]).includes("<h1")) throw new Error("CLI convert produced no heading");
  run(cli, ["format", "--check", fixture]);
  smokeDeadline = createNativeSmokeDeadline();
  await smokeLsp(lsp, manifest.packageVersion, smokeDeadline);
  await smokeForcedProcessLifecycle(lsp, smokeDeadline);
} catch (error) {
  operationError = error;
} finally {
  const cleanupDeadline = smokeDeadline ?? createNativeSmokeDeadline();
  try {
    await removeNativeSmokeDirectory(scratch, cleanupDeadline, {
      platform: runtime.platform.os,
    });
  } catch (cleanupError) {
    if (operationError) {
      operationError.message = `${operationError.message}\n一時ディレクトリの削除にも失敗しました: ${cleanupError.message}`;
    } else {
      operationError = cleanupError;
    }
  } finally {
    cleanupDeadline.dispose();
  }
}
if (operationError) throw operationError;
process.stdout.write(`native release smoke passed: ${target}\n`);
