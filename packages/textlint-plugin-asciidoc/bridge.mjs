import { readFileSync } from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const manifest = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8"));

let bridge;

function loadBridge() {
  if (bridge) return bridge;
  try {
    bridge = require("./wasm/adocweave_textlint_wasm.cjs");
  } catch (cause) {
    const error = new Error("同梱されたAdocWeave WebAssemblyを読み込めませんでした。", {
      cause
    });
    error.code = "wasm-initialization-failed";
    throw error;
  }
  if (typeof bridge.projectText !== "function") {
    const error = new Error("同梱されたAdocWeave WebAssemblyにprojectTextがありません。");
    error.code = "wasm-initialization-failed";
    throw error;
  }
  return bridge;
}

function payloadFrom(error) {
  const encoded =
    typeof error === "string" ? error : error instanceof Error ? error.message : undefined;
  if (!encoded) return undefined;
  try {
    const payload = JSON.parse(encoded);
    return payload && typeof payload === "object" ? payload : undefined;
  } catch {
    return undefined;
  }
}

function normalizeError(cause) {
  if (cause instanceof Error && typeof cause.code === "string") return cause;
  const payload = payloadFrom(cause);
  const message =
    typeof payload?.message === "string" && payload.message.length > 0
      ? payload.message
      : "AdocWeaveでAsciiDocを解析できませんでした。";
  const error = new Error(message, { cause });
  error.code =
    typeof payload?.code === "string" && payload.code.length > 0
      ? payload.code
      : "adocweave-error";
  return error;
}

export function createProjectText(bridgeLoader) {
  return (source, sourceId) => {
    try {
      const loaded = bridgeLoader();
      if (typeof loaded?.projectText !== "function") {
        const error = new Error("同梱されたAdocWeave WebAssemblyにprojectTextがありません。");
        error.code = "wasm-initialization-failed";
        throw error;
      }
      return loaded.projectText({
        packageVersion: manifest.version,
        sourceId: sourceId ?? null,
        source
      });
    } catch (cause) {
      throw normalizeError(cause);
    }
  };
}

export const projectText = createProjectText(loadBridge);
