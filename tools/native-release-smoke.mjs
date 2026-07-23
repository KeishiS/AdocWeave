import { execFileSync, spawn } from "node:child_process";
import { mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join, resolve, sep } from "node:path";
import process from "node:process";

const [artifactDirectory, target] = process.argv.slice(2);
if (!artifactDirectory || !target) {
  process.stderr.write("usage: node tools/native-release-smoke.mjs ARTIFACT_DIRECTORY TARGET\n");
  process.exit(2);
}

const expectedArchitecture = {
  "aarch64-unknown-linux-musl": { node: "arm64", elf: "AArch64" },
  "x86_64-unknown-linux-musl": { node: "x64", elf: "Advanced Micro Devices X86-64" },
}[target];
if (!expectedArchitecture) throw new Error(`unsupported smoke target: ${target}`);
if (process.arch !== expectedArchitecture.node) {
  throw new Error(`smoke host ${process.arch} does not match ${target}`);
}

const manifest = JSON.parse(readFileSync(new URL("../release-manifest.json", import.meta.url), "utf8"));
const workspaceRoot = realpathSync(new URL("../", import.meta.url).pathname);
const scratch = mkdtempSync(join(tmpdir(), "adocweave-native-smoke-"));

function filesRecursively(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    return entry.isDirectory() ? filesRecursively(path) : [path];
  });
}

function archive(name) {
  const expected = `${name}-${target}.tar.xz`;
  const matches = filesRecursively(resolve(artifactDirectory)).filter((path) => basename(path) === expected);
  if (matches.length !== 1) throw new Error(`expected exactly one ${expected}, found ${matches.length}`);
  return matches[0];
}

function extract(archivePath, executable) {
  const root = basename(archivePath, ".tar.xz");
  const entries = execFileSync("tar", ["-tJf", archivePath], { encoding: "utf8" }).trim().split("\n");
  const expectedEntries = [
    `${root}/`,
    `${root}/LICENSE-APACHE`,
    `${root}/LICENSE-MIT`,
    `${root}/README.adoc`,
    `${root}/THIRD_PARTY_NOTICES.adoc`,
    `${root}/${executable}`,
  ].sort();
  if (JSON.stringify(entries.sort()) !== JSON.stringify(expectedEntries)) {
    throw new Error(`${basename(archivePath)} has an unexpected archive layout:\n${entries.join("\n")}`);
  }
  if (entries.some((entry) => entry.startsWith("/") || entry.split("/").includes(".."))) {
    throw new Error(`${basename(archivePath)} contains an unsafe path`);
  }
  execFileSync("tar", ["-xJf", archivePath, "-C", scratch]);
  const binary = realpathSync(join(scratch, root, executable));
  if (!binary.startsWith(`${realpathSync(scratch)}${sep}`) || binary.startsWith(`${workspaceRoot}${sep}`)) {
    throw new Error(`smoke test selected a binary outside the extracted archive: ${binary}`);
  }
  execFileSync("test", ["-x", binary]);
  const header = execFileSync("readelf", ["-h", binary], { encoding: "utf8" });
  if (!header.includes(`Machine:                           ${expectedArchitecture.elf}`)) {
    throw new Error(`${executable} has the wrong ELF architecture`);
  }
  const dynamic = execFileSync("readelf", ["-d", binary], { encoding: "utf8" });
  if (/\(NEEDED\)/.test(dynamic) && process.env.ADOCWEAVE_SMOKE_ALLOW_DYNAMIC !== "1") {
    throw new Error(`${executable} has an unexpected dynamic dependency`);
  }
  return binary;
}

function run(binary, args, options = {}) {
  return execFileSync(binary, args, { encoding: "utf8", ...options });
}

function version(binary) {
  const value = JSON.parse(run(binary, ["--version", "--json"]));
  if (value.packageVersion !== manifest.packageVersion) throw new Error(`${value.name} package version mismatch`);
  if (value.contractVersion !== manifest.contractVersion) {
    throw new Error(`${value.name} contract version mismatch`);
  }
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
  const exitCode = await new Promise((resolvePromise) => child.once("exit", resolvePromise));
  if (exitCode !== 0) throw new Error(`LSP exited with ${exitCode}`);
}

try {
  const cli = extract(archive("adocweave-cli"), "adocweave");
  const lsp = extract(archive("adocweave-lsp"), "adocweave-lsp");
  version(cli);
  version(lsp);
  const fixture = join(scratch, "fixture.adoc");
  writeFileSync(fixture, "= Title\n\ntext\n");
  run(cli, ["check", fixture]);
  if (!run(cli, ["convert", fixture]).includes("<h1")) throw new Error("CLI convert produced no heading");
  run(cli, ["format", "--check", fixture]);
  await smokeLsp(lsp);
  process.stdout.write(`native release smoke passed: ${target}\n`);
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
