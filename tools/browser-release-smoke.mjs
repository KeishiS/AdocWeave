import { execFile, spawn } from "node:child_process";
import { createServer } from "node:http";
import { mkdtemp, readFile, rm, stat } from "node:fs/promises";
import { once } from "node:events";
import { tmpdir } from "node:os";
import { extname, join, normalize, resolve, sep } from "node:path";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { assertBrowserArtifactSizes } from "./browser-release-budget.mjs";
import {
  BROWSER_STARTUP_ATTEMPTS,
  BROWSER_STARTUP_ATTEMPT_TIMEOUT_MS,
  BROWSER_STARTUP_TOTAL_TIMEOUT_MS,
  retryBrowserStartup,
} from "./browser-startup.mjs";
import {
  hostExecutableEnvironment,
  resolveHostExecutable,
} from "./host-executable.mjs";
import { hasExited, waitForExit } from "./process-lifecycle.mjs";

const run = promisify(execFile);
const POLL_INTERVAL_MS = 25;

if (
  process.argv[1]
  && pathToFileURL(resolve(process.argv[1])).href === import.meta.url
) {
  await main();
}

async function main() {
  const [archive, chromiumCommand = "chromium"] = process.argv.slice(2);
  if (!archive) throw new Error("usage: browser-release-smoke.mjs ARCHIVE [CHROMIUM]");
  const chromium = await resolveHostExecutable(chromiumCommand);
  const releaseManifest = JSON.parse(await readFile("release-manifest.json", "utf8"));
  const root = await mkdtemp(join(tmpdir(), "adocweave-browser-smoke-"));
  try {
    await runArchiveSmoke(archive, chromium, releaseManifest, root);
  } finally {
    await rm(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
}

async function runArchiveSmoke(archive, chromium, releaseManifest, root) {
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
  assertBrowserArtifactSizes(archiveBytes, wasmBytes);

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
      if (state.packageVersion !== releaseManifest.packageVersion ||
          state.resultPackageVersion !== releaseManifest.packageVersion ||
          state.wasmPackageVersion !== releaseManifest.packageVersion ||
          state.analysisPackageVersion !== releaseManifest.packageVersion ||
          state.projectionPackageVersion !== releaseManifest.packageVersion) {
        throw new Error(`browser package version mismatch: ${JSON.stringify(state)}`);
      }
      console.log(`browser release smoke: passed ${context} context`);
    }
  } finally {
    await new Promise((resolveClose) => server.close(resolveClose));
  }
  console.log(`browser release smoke passed: archive=${archiveBytes} wasm=${wasmBytes}`);
}

export async function inspectPage(
  chromium,
  url,
  temporaryRoot,
  {
    attempts = BROWSER_STARTUP_ATTEMPTS,
    attemptTimeoutMs = BROWSER_STARTUP_ATTEMPT_TIMEOUT_MS,
    totalTimeoutMs = BROWSER_STARTUP_TOTAL_TIMEOUT_MS,
    dependencies,
  } = {},
) {
  return retryBrowserStartup(
    ({ remainingMs, signal }) => inspectPageAttempt(
      chromium,
      url,
      temporaryRoot,
      Math.min(attemptTimeoutMs, remainingMs),
      signal,
      dependencies,
    ),
    {
      attempts,
      totalTimeoutMs,
      onFailure: ({ attempt, attempts, elapsedMs, error, willRetry }) => {
        console.error(
          `browser release smoke: startup attempt ${attempt}/${attempts} failed after ${elapsedMs} ms; ${willRetry ? "retrying with a fresh profile" : "no retries remain"}:\n${error.message}`,
        );
      },
    },
  );
}

export async function inspectPageAttempt(
  chromium,
  url,
  temporaryRoot,
  startupTimeoutMs,
  signal,
  {
    randomUUID = () => crypto.randomUUID(),
    spawnBrowser = spawn,
    readText = readFile,
    fetchTarget = fetch,
    WebSocketImplementation = WebSocket,
    waitForBrowserExit = waitForExit,
  } = {},
) {
  const profile = join(temporaryRoot, `profile-${randomUUID()}`);
  const browser = spawnBrowser(chromium, [
    "--headless=new", "--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage",
    "--disable-background-networking", "--no-first-run", "--no-default-browser-check",
    "--remote-debugging-port=0", `--user-data-dir=${profile}`,
    "about:blank",
  ], {
    env: hostExecutableEnvironment(process.env),
    stdio: ["ignore", "ignore", "pipe"],
  });
  let spawnError;
  let stderr = "";
  let socket;
  browser.once("error", (error) => { spawnError = error; });
  browser.stderr.setEncoding("utf8");
  browser.stderr.on("data", (chunk) => { stderr = `${stderr}${chunk}`.slice(-8192); });
  try {
    const attemptController = new AbortController();
    let startupPhase = "DevToolsActivePort";
    const attemptTimer = setTimeout(() => {
      const error = new Error(
        `Chromium startup attempt exceeded ${startupTimeoutMs} ms during ${startupPhase}`,
      );
      error.retryBrowserStartup = true;
      attemptController.abort(error);
    }, startupTimeoutMs);
    const startupSignal = AbortSignal.any([signal, attemptController.signal]);
    let port;
    let call;
    let event;
    try {
      port = await poll(async () => {
        const contents = await readText(
          join(profile, "DevToolsActivePort"),
          { encoding: "utf8", signal: startupSignal },
        );
        const candidate = Number.parseInt(contents.split("\n", 1)[0], 10);
        return Number.isInteger(candidate) && candidate > 0 ? candidate : undefined;
      }, () => browserFailure(browser, spawnError, stderr), startupTimeoutMs, startupSignal);
      startupPhase = "DevTools page target discovery";
      const target = await poll(async () => {
        const requestSignal = AbortSignal.any([
          startupSignal,
          AbortSignal.timeout(1000),
        ]);
        const response = await fetchTarget(
          `http://127.0.0.1:${port}/json/list`,
          { signal: requestSignal },
        );
        return (await response.json()).find((candidate) => candidate.type === "page");
      }, () => browserFailure(browser, spawnError, stderr), startupTimeoutMs, startupSignal);
      startupPhase = "DevTools WebSocket connection";
      socket = new WebSocketImplementation(target.webSocketDebuggerUrl);
      await once(socket, "open", { signal: startupSignal });
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
      call = (method, params = {}) => new Promise((resolveCall, rejectCall) => {
        const callId = ++id;
        replies.set(callId, { resolve: resolveCall, reject: rejectCall });
        socket.send(JSON.stringify({ id: callId, method, params }));
      });
      event = (method) => new Promise((resolveEvent) => eventWaiters.set(method, resolveEvent));
      startupPhase = "Page.enable";
      await withAbortSignal(call("Page.enable"), startupSignal);
    } catch (error) {
      const fatal = browserFailure(browser, spawnError, stderr);
      const failure = fatal ?? new Error(
        `browser did not complete the DevTools startup handshake: ${error.message}${stderr ? `\n${stderr}` : ""}`,
      );
      failure.retryBrowserStartup = fatal === undefined;
      throw failure;
    } finally {
      clearTimeout(attemptTimer);
    }

    const loaded = event("Page.loadEventFired");
    await withTimeout(call("Page.navigate", { url }), 5000, "Page.navigate timeout");
    await withTimeout(loaded, 20000, "page load timeout");
    const evaluated = await withTimeout(call("Runtime.evaluate", {
      expression: `new Promise((resolve, reject) => {
        const deadline = Date.now() + 15000;
        const wait = () => {
          const status = document.querySelector('#status').value;
          if (status.startsWith('ready:') || status.startsWith('error:')) {
            const response = globalThis.adocweaveLastResult;
            resolve({
              status,
              html: document.querySelector('#preview').textContent,
              isolated: crossOriginIsolated,
              packageVersion: globalThis.adocweavePackageVersion,
              resultPackageVersion: response.packageVersion,
              wasmPackageVersion: response.packageVersion,
              analysisPackageVersion: response.parse.packageVersion,
              projectionPackageVersion: response.projection.packageVersion,
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
    if (socket && socket.readyState < WebSocketImplementation.CLOSING) socket.close();
    browser.kill("SIGTERM");
    if (!await waitForBrowserExit(browser, 2000)) {
      browser.kill("SIGKILL");
      if (!await waitForBrowserExit(browser, 5000)) throw new Error("browser did not exit after SIGKILL");
    }
  }
}

async function withAbortSignal(promise, signal) {
  if (!signal) return promise;
  signal.throwIfAborted();
  let onAbort;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        onAbort = () => reject(signal.reason);
        signal.addEventListener("abort", onAbort, { once: true });
      }),
    ]);
  } finally {
    signal.removeEventListener("abort", onAbort);
  }
}

async function poll(operation, failure, timeoutMs, signal) {
  let error;
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    signal?.throwIfAborted();
    const fatal = failure?.();
    if (fatal) throw fatal;
    try {
      const value = await withAbortSignal(operation(), signal);
      if (value) return value;
    } catch (caught) {
      signal?.throwIfAborted();
      error = caught;
    }
    await abortableDelay(POLL_INTERVAL_MS, signal);
  }
  signal?.throwIfAborted();
  throw error ?? new Error(
    `Chromium did not become ready within ${timeoutMs} ms`,
  );
}

async function abortableDelay(milliseconds, signal) {
  signal?.throwIfAborted();
  let timer;
  let onAbort;
  try {
    await new Promise((resolveDelay, rejectDelay) => {
      timer = setTimeout(resolveDelay, milliseconds);
      if (signal) {
        onAbort = () => rejectDelay(signal.reason);
        signal.addEventListener("abort", onAbort, { once: true });
      }
    });
  } finally {
    clearTimeout(timer);
    if (onAbort) signal.removeEventListener("abort", onAbort);
  }
}

function browserFailure(browser, spawnError, stderr) {
  if (spawnError) return new Error(`browser failed to start: ${spawnError.message}`);
  if (!hasExited(browser)) return undefined;
  const status = browser.signalCode ?? browser.exitCode;
  return new Error(`browser exited before DevTools became ready (${status})${stderr ? `:\n${stderr}` : ""}`);
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
