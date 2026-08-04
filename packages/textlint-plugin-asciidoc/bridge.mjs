import { readFileSync } from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const manifest = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8"));

let bundledBridge;

function loadBundledBridge() {
  if (bundledBridge) return bundledBridge;
  bundledBridge = require("./wasm/adocweave_textlint_wasm.cjs");
  return bundledBridge;
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

export function createParseText({ bridgeLoader, componentVersion }) {
  if (typeof bridgeLoader !== "function") {
    throw new TypeError("bridgeLoaderは関数で指定してください。");
  }
  if (typeof componentVersion !== "string" || componentVersion.length === 0) {
    throw new TypeError("componentVersionは空でない文字列で指定してください。");
  }

  return (source, sourceId) => {
    if (typeof source !== "string") {
      throw new TypeError("AsciiDocの入力は文字列で指定してください。");
    }
    if (sourceId !== undefined && sourceId !== null && typeof sourceId !== "string") {
      throw new TypeError("sourceIdは文字列またはnullで指定してください。");
    }
    try {
      let loaded;
      try {
        loaded = bridgeLoader();
      } catch (cause) {
        const error = new Error("AdocWeave WebAssemblyを読み込めませんでした。", { cause });
        error.code = "wasm-initialization-failed";
        throw error;
      }
      if (typeof loaded?.parseText !== "function") {
        const error = new Error("AdocWeave WebAssemblyにparseTextがありません。");
        error.code = "wasm-initialization-failed";
        throw error;
      }
      return loaded.parseText({
        componentVersion,
        sourceId: sourceId ?? null,
        source
      });
    } catch (cause) {
      throw normalizeError(cause);
    }
  };
}

export const parseText = createParseText({
  bridgeLoader: loadBundledBridge,
  componentVersion: manifest.version
});
