import { readFileSync } from "node:fs";
import process from "node:process";

const ROOT = new URL("../", import.meta.url);
const STABLE_VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const STATES = new Set(["preparing", "ready"]);

function fail(message) {
  throw new Error(message);
}

function exactKeys(value, expected, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    fail(`${label}はobjectである必要があります`);
  }
  const actual = Object.keys(value).sort();
  if (JSON.stringify(actual) !== JSON.stringify([...expected].sort())) {
    fail(`${label}に不足、余分または未知のfieldがあります`);
  }
}

export function canonicalReleaseIntent(intent) {
  return `${JSON.stringify({
    schemaVersion: intent.schemaVersion,
    version: intent.version,
    state: intent.state,
    generation: intent.generation,
  }, null, 2)}\n`;
}

export function validateReleaseIntent(intent, packageVersion, { requireReady = false } = {}) {
  exactKeys(intent, ["schemaVersion", "version", "state", "generation"], "release intent");
  if (intent.schemaVersion !== 1) fail("release intentのschemaVersionは1である必要があります");
  if (typeof intent.version !== "string" || !STABLE_VERSION.test(intent.version)) {
    fail("release intentのversionはstable SemVerである必要があります");
  }
  if (intent.version !== packageVersion) {
    fail(`release intentのversion ${intent.version}がpackage version ${packageVersion}と一致しません`);
  }
  if (!STATES.has(intent.state)) {
    fail("release intentのstateはpreparingまたはreadyである必要があります");
  }
  if (!Number.isSafeInteger(intent.generation) || intent.generation < 1) {
    fail("release intentのgenerationは1以上の安全な整数である必要があります");
  }
  if (requireReady && intent.state !== "ready") {
    fail("release intentがreadyではありません");
  }
  return intent;
}

export function prepareReleaseIntent(intent, currentVersion, nextVersion) {
  validateReleaseIntent(intent, currentVersion);
  if (!STABLE_VERSION.test(nextVersion)) fail("更新先のrelease intent versionが不正です");
  if (intent.generation === Number.MAX_SAFE_INTEGER) {
    fail("release intentのgenerationが上限に達しています");
  }
  return {
    schemaVersion: 1,
    version: nextVersion,
    state: "preparing",
    generation: intent.generation + 1,
  };
}

export function loadReleaseIntent(root = ROOT) {
  return JSON.parse(readFileSync(new URL("release/intent.json", root), "utf8"));
}

export function verifyRepositoryReleaseIntent(root = ROOT, { requireReady = false } = {}) {
  const intent = loadReleaseIntent(root);
  const manifest = JSON.parse(readFileSync(new URL("release-manifest.json", root), "utf8"));
  validateReleaseIntent(intent, manifest.packageVersion, { requireReady });
  return intent;
}

if (process.argv[1] && import.meta.url === new URL(process.argv[1], "file:").href) {
  try {
    const requireReady = process.argv.slice(2).includes("--require-ready");
    if (process.argv.length > (requireReady ? 3 : 2)) {
      fail("使用方法：node tools/release-intent.mjs [--require-ready]");
    }
    const intent = verifyRepositoryReleaseIntent(ROOT, { requireReady });
    process.stdout.write(`release intentを検査しました：${intent.version} ${intent.state} generation ${intent.generation}\n`);
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
