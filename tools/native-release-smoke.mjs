import { execFileSync, spawn } from "node:child_process";
import {
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  realpathSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { basename, join, resolve, sep } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const [artifactDirectory, target] = process.argv.slice(2);
if (!artifactDirectory || !target) {
  process.stderr.write("usage: node tools/native-release-smoke.mjs ARTIFACT_DIRECTORY TARGET\n");
  process.exit(2);
}

const plan = JSON.parse(readFileSync(new URL("../release/distribution-plan.json", import.meta.url), "utf8"));
const platform = plan.targets.find(({ triple }) => triple === target);
if (!platform) throw new Error(`unsupported smoke target: ${target}`);
if (process.platform !== platform.os || process.arch !== platform.architecture) {
  throw new Error(`smoke host ${process.arch} does not match ${target}`);
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
  const entries = platform.archive === "zip"
    ? execFileSync("unzip", ["-Z1", archivePath], { encoding: "utf8" }).trim().split("\n")
    : execFileSync("tar", ["-tJf", archivePath], { encoding: "utf8" }).trim().split("\n");
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
  if (entries.some((entry) => entry.startsWith("/") || entry.split("/").includes(".."))) {
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
    if (/\(NEEDED\)/.test(dynamic) && process.env.ADOCWEAVE_SMOKE_ALLOW_DYNAMIC !== "1") {
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
    if (dependencies.split("\n").slice(1).some((line) => line.trim() && !line.trim().startsWith("/usr/lib/") &&
      !line.trim().startsWith("/System/Library/"))) {
      throw new Error(`${executable} has a non-system dynamic dependency`);
    }
    const loadCommands = execFileSync("otool", ["-l", binary], { encoding: "utf8" });
    const minimum = /cmd LC_BUILD_VERSION[\s\S]*?minos ([0-9.]+)/.exec(loadCommands)?.[1];
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
  const dumpbin = process.env.ADOCWEAVE_DUMPBIN;
  if (!dumpbin) throw new Error("ADOCWEAVE_DUMPBIN is required for Windows dependency verification");
  const dependencies = execFileSync(dumpbin, ["/DEPENDENTS", binary], { encoding: "utf8" });
  const imported = [...dependencies.matchAll(/^\s+([A-Za-z0-9_.-]+\.dll)\s*$/gim)]
    .map((match) => match[1].toLowerCase());
  const allowed = new Set([
    "advapi32.dll",
    "bcrypt.dll",
    "bcryptprimitives.dll",
    "crypt32.dll",
    "iphlpapi.dll",
    "kernel32.dll",
    "normaliz.dll",
    "ntdll.dll",
    "ole32.dll",
    "secur32.dll",
    "shell32.dll",
    "user32.dll",
    "userenv.dll",
    "ws2_32.dll",
  ]);
  const unexpected = imported.filter((name) =>
    !allowed.has(name) && !name.startsWith("api-ms-win-") && !name.startsWith("ext-ms-win-"));
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

function send(child, message) {
  const body = JSON.stringify(message);
  child.stdin.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
}

async function smokeLsp(binary) {
  const child = spawn(binary, [], { stdio: ["pipe", "pipe", "pipe"] });
  let buffer = Buffer.alloc(0);
  const messages = [];
  const waiters = [];
  const publish = (message) => {
    messages.push(message);
    for (const waiter of [...waiters]) {
      if (waiter.predicate(message)) {
        clearTimeout(waiter.timer);
        waiter.resolve(message);
        waiters.splice(waiters.indexOf(waiter), 1);
      }
    }
  };
  child.stdout.on("data", (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    while (true) {
      const boundary = buffer.indexOf("\r\n\r\n");
      if (boundary < 0) return;
      const header = buffer.subarray(0, boundary).toString("ascii");
      const match = /(?:^|\r\n)Content-Length: (\d+)(?:\r\n|$)/i.exec(header);
      if (!match) throw new Error("LSP response has no Content-Length");
      const length = Number(match[1]);
      const end = boundary + 4 + length;
      if (buffer.length < end) return;
      publish(JSON.parse(buffer.subarray(boundary + 4, end).toString("utf8")));
      buffer = buffer.subarray(end);
    }
  });
  const waitFor = (predicate) => new Promise((resolvePromise, reject) => {
    const found = messages.find(predicate);
    if (found) return resolvePromise(found);
    const waiter = { predicate, resolve: resolvePromise };
    waiters.push(waiter);
    waiter.timer = setTimeout(() => {
      const index = waiters.indexOf(waiter);
      if (index >= 0) waiters.splice(index, 1);
      reject(new Error("timed out waiting for LSP response"));
    }, 10_000);
  });

  send(child, { jsonrpc: "2.0", id: 1, method: "initialize", params: { processId: null, rootUri: null, capabilities: {} } });
  const initialized = await waitFor((message) => message.id === 1);
  if (initialized.result?.serverInfo?.version !== manifest.packageVersion) throw new Error("LSP serverInfo version mismatch");
  send(child, { jsonrpc: "2.0", method: "initialized", params: {} });
  send(child, {
    jsonrpc: "2.0",
    method: "textDocument/didOpen",
    params: { textDocument: { uri: "file:///tmp/adocweave-smoke.adoc", languageId: "asciidoc", version: 1, text: "=Bad\n" } },
  });
  const diagnostics = await waitFor((message) => message.method === "textDocument/publishDiagnostics");
  if (!Array.isArray(diagnostics.params?.diagnostics) || diagnostics.params.diagnostics.length === 0) {
    throw new Error("LSP smoke fixture produced no diagnostics");
  }
  send(child, { jsonrpc: "2.0", id: 2, method: "shutdown", params: null });
  await waitFor((message) => message.id === 2);
  send(child, { jsonrpc: "2.0", method: "exit", params: null });
  child.stdin.end();
  const exitCode = await new Promise((resolvePromise) => child.once("close", resolvePromise));
  if (exitCode !== 0) throw new Error(`LSP exited with ${exitCode}`);
}

async function smokeForcedProcessLifecycle(binary) {
  const lifecycle = join(scratch, `lifecycle${platform.executableSuffix}`);
  const replaced = `${lifecycle}.replaced`;
  copyFileSync(binary, lifecycle);
  if (platform.os !== "win32") execFileSync("chmod", ["755", lifecycle]);
  const child = spawn(lifecycle, [], { stdio: ["pipe", "pipe", "pipe"] });
  await new Promise((resolvePromise, reject) => {
    child.once("spawn", resolvePromise);
    child.once("error", reject);
  });
  if (platform.os === "win32") {
    let rejected = false;
    try {
      renameSync(lifecycle, replaced);
    } catch {
      rejected = true;
    }
    if (!rejected) throw new Error("Windows allowed replacement of a running Language Server");
  }
  const exited = new Promise((resolvePromise) => child.once("close", resolvePromise));
  child.kill();
  const exit = await Promise.race([
    exited,
    new Promise((_, reject) =>
      setTimeout(() => reject(new Error("forced Language Server stop timed out")), 5_000)),
  ]);
  if (exit === undefined && child.exitCode === null && child.signalCode === null) {
    throw new Error("forced Language Server stop did not report an exit");
  }
  renameSync(lifecycle, replaced);
  rmSync(replaced);
}

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
  await smokeLsp(lsp);
  await smokeForcedProcessLifecycle(lsp);
  process.stdout.write(`native release smoke passed: ${target}\n`);
} finally {
  rmSync(scratch, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
}
