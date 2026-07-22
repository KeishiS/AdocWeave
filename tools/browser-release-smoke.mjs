import { execFile, spawn } from "node:child_process";
import { createServer } from "node:http";
import { mkdtemp, readFile, rm, stat } from "node:fs/promises";
import { once } from "node:events";
import { tmpdir } from "node:os";
import { extname, join, normalize, resolve, sep } from "node:path";
import { promisify } from "node:util";

const run = promisify(execFile);
const [archive, chromium = "chromium"] = process.argv.slice(2);
if (!archive) throw new Error("usage: browser-release-smoke.mjs ARCHIVE [CHROMIUM]");
const releaseManifest = JSON.parse(await readFile("release-manifest.json", "utf8"));

const root = await mkdtemp(join(tmpdir(), "adocweave-browser-smoke-"));
try {
  const { stdout: archiveList } = await run("tar", ["-tJf", resolve(archive)]);
  const members = archiveList.trimEnd().split("\n");
  const roots = new Set();
  for (const member of members) {
    if (member.startsWith("/") || member.split("/").includes("..")) {
      throw new Error(`unsafe archive member: ${member}`);
    }
    roots.add(member.split("/")[0]);
  }
  if (roots.size !== 1 || ![...roots][0].startsWith("adocweave-browser-")) {
    throw new Error(`unexpected archive roots: ${[...roots].join(", ")}`);
  }
  await run("tar", ["-xJf", resolve(archive), "-C", root]);
  const entries = await import("node:fs/promises").then(({ readdir }) => readdir(root));
  if (entries.length !== 1 || !entries[0].startsWith("adocweave-browser-")) {
    throw new Error(`unexpected archive root: ${entries.join(", ")}`);
  }
  const packageRoot = join(root, entries[0]);
  const archiveBytes = (await stat(archive)).size;
  const wasmBytes = (await stat(join(packageRoot, "wasm/adocweave_wasm_bg.wasm"))).size;
  if (archiveBytes > 2 * 1024 * 1024) throw new Error(`archive exceeds 2 MiB: ${archiveBytes}`);
  if (wasmBytes > 1024 * 1024) throw new Error(`WASM exceeds 1 MiB: ${wasmBytes}`);

  const requests = [];
  const server = createServer(async (request, response) => {
    try {
      const url = new URL(request.url, "http://localhost");
      requests.push(url.pathname);
      const requested = decodeURIComponent(url.pathname).replace(/^\/+/, "");
      const [context, ...segments] = requested.split("/");
      if (context !== "isolated" && context !== "fallback") throw new Error("missing browser context prefix");
      const relative = segments.join("/") || "example/index.html";
      const path = normalize(join(packageRoot, relative));
      if (!path.startsWith(`${normalize(packageRoot)}${sep}`)) throw new Error("unsafe path");
      const types = { ".html": "text/html", ".mjs": "text/javascript", ".js": "text/javascript", ".wasm": "application/wasm" };
      response.setHeader("Content-Type", types[extname(path)] ?? "application/octet-stream");
      response.setHeader("Content-Security-Policy", "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; worker-src 'self'; connect-src 'self'");
      if (context === "isolated") {
        response.setHeader("Cross-Origin-Opener-Policy", "same-origin");
        response.setHeader("Cross-Origin-Embedder-Policy", "require-corp");
      }
      response.end(await readFile(path));
    } catch (error) {
      response.statusCode = 404;
      response.end(String(error));
    }
  });
  await new Promise((resolveListen) => server.listen(0, "127.0.0.1", resolveListen));
  const { port } = server.address();
  try {
    for (const isolated of [false, true]) {
      const context = isolated ? "isolated" : "fallback";
      const url = `http://127.0.0.1:${port}/${context}/example/index.html?smoke=1`;
      console.log(`browser release smoke: starting ${context} context with ${chromium}`);
      const state = await inspectPage(chromium, url, root);
      if (state.status !== "ready:4:5" || !state.html.includes("Latest browser result") || state.isolated !== isolated) {
        throw new Error(`browser smoke failed (${isolated ? "isolated" : "fallback"}); requests=${requests.join(",")}: ${JSON.stringify(state)}`);
      }
      const contractsMatch = Object.keys(releaseManifest.contracts).every(
        (key) => state.contracts[key] === releaseManifest.contracts[key],
      ) && Object.keys(state.contracts).length === Object.keys(releaseManifest.contracts).length;
      if (state.packageVersion !== releaseManifest.packageVersion ||
          !contractsMatch ||
          state.wasmApi !== releaseManifest.contracts.wasmApi ||
          state.conformance !== releaseManifest.contracts.conformance ||
          state.coreProfile !== releaseManifest.contracts.coreProfile ||
          state.projection !== releaseManifest.contracts.projection) {
        throw new Error(`browser contract mismatch: ${JSON.stringify(state)}`);
      }
      console.log(`browser release smoke: passed ${context} context`);
    }
  } finally {
    await new Promise((resolveClose) => server.close(resolveClose));
  }
  console.log(`browser release smoke passed: archive=${archiveBytes} wasm=${wasmBytes}`);
} finally {
  await rm(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
}

async function inspectPage(chromium, url, temporaryRoot) {
  const port = await availablePort();
  const browser = spawn(chromium, [
    "--headless=new", "--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage",
    `--remote-debugging-port=${port}`, `--user-data-dir=${join(temporaryRoot, `profile-${port}`)}`,
    "about:blank",
  ], { stdio: "ignore" });
  try {
    const target = await poll(async () => {
      const response = await fetch(`http://127.0.0.1:${port}/json/list`, { signal: AbortSignal.timeout(1000) });
      return (await response.json()).find((candidate) => candidate.type === "page");
    });
    const socket = new WebSocket(target.webSocketDebuggerUrl);
    await withTimeout(once(socket, "open"), 5000, "DevTools WebSocket connection timeout");
    let id = 0;
    const replies = new Map();
    const eventWaiters = new Map();
    socket.addEventListener("message", ({ data }) => {
      const message = JSON.parse(data);
      if (message.id && replies.has(message.id)) {
        const reply = replies.get(message.id);
        replies.delete(message.id);
        message.error ? reply.reject(new Error(message.error.message)) : reply.resolve(message.result);
      } else if (message.method && eventWaiters.has(message.method)) {
        eventWaiters.get(message.method)(message.params);
        eventWaiters.delete(message.method);
      }
    });
    const call = (method, params = {}) => new Promise((resolveCall, rejectCall) => {
      const callId = ++id;
      replies.set(callId, { resolve: resolveCall, reject: rejectCall });
      socket.send(JSON.stringify({ id: callId, method, params }));
    });
    const event = (method) => new Promise((resolveEvent) => eventWaiters.set(method, resolveEvent));
    await withTimeout(call("Page.enable"), 5000, "Page.enable timeout");
    const loaded = event("Page.loadEventFired");
    await withTimeout(call("Page.navigate", { url }), 5000, "Page.navigate timeout");
    await withTimeout(loaded, 20000, "page load timeout");
    const evaluated = await withTimeout(call("Runtime.evaluate", {
      expression: `new Promise((resolve, reject) => {
        const deadline = Date.now() + 15000;
        const wait = () => {
          const status = document.querySelector('#status').value;
          if (status.startsWith('ready:') || status.startsWith('error:')) {
            const response = globalThis.adocweaveLastResult.result;
            resolve({
              status,
              html: document.querySelector('#preview').textContent,
              isolated: crossOriginIsolated,
              packageVersion: globalThis.adocweavePackageVersion,
              contracts: globalThis.adocweaveLastResult.contracts,
              wasmApi: response.apiVersion,
              conformance: response.conformanceContractVersion,
              coreProfile: response.parse.profileVersion,
              projection: response.projection.contractVersion,
            });
          } else if (Date.now() >= deadline) {
            reject(new Error('result timeout: ' + status));
          } else setTimeout(wait, 25);
        };
        wait();
      })`,
      awaitPromise: true,
      returnByValue: true,
    }), 20000, "Runtime.evaluate timeout");
    socket.close();
    return evaluated.result.value;
  } finally {
    browser.kill("SIGTERM");
    await waitForExit(browser, 2000);
    if (browser.exitCode === null) {
      browser.kill("SIGKILL");
      await withTimeout(once(browser, "exit"), 5000, "Chromium did not exit after SIGKILL");
    }
  }
}

async function availablePort() {
  const server = createServer();
  await new Promise((resolveListen) => server.listen(0, "127.0.0.1", resolveListen));
  const { port } = server.address();
  await new Promise((resolveClose) => server.close(resolveClose));
  return port;
}

async function poll(operation) {
  let error;
  for (let attempt = 0; attempt < 200; attempt += 1) {
    try {
      const value = await operation();
      if (value) return value;
    } catch (caught) {
      error = caught;
    }
    await new Promise((resolveWait) => setTimeout(resolveWait, 25));
  }
  throw error ?? new Error("Chromium did not start");
}

async function withTimeout(promise, milliseconds, message) {
  let timer;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(message)), milliseconds);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

function waitForExit(child, milliseconds) {
  if (child.exitCode !== null) return Promise.resolve(true);
  return new Promise((resolveWait) => {
    const exited = () => {
      clearTimeout(timer);
      resolveWait(true);
    };
    const timer = setTimeout(() => {
      child.off("exit", exited);
      resolveWait(false);
    }, milliseconds);
    child.once("exit", exited);
  });
}
