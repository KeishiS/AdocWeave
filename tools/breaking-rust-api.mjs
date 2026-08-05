import { readFileSync } from "node:fs";

const ROOT = new URL("../", import.meta.url);
const CHANGE_KEYS = ["crate", "description", "item", "lint", "migration", "summary"];

const fail = (message) => {
  throw new Error(message);
};

export const breakingFailureKey = ({ crate, lint, item }) => `${crate}\u0000${lint}\u0000${item}`;

export function validateBreakingRustApi(record) {
  if (!record || typeof record !== "object" || Array.isArray(record)) {
    fail("公開Rust APIの破壊的変更記録がobjectではありません");
  }
  const keys = Object.keys(record).sort();
  if (JSON.stringify(keys) !== JSON.stringify(["changes", "releaseVersion", "schemaVersion"])) {
    fail(`公開Rust APIの破壊的変更記録に未知または不足した項目があります：${keys.join("、")}`);
  }
  if (record.schemaVersion !== 1) fail("公開Rust APIの破壊的変更記録のschemaVersionが1ではありません");
  if (typeof record.releaseVersion !== "string" || !/^\d+\.\d+\.\d+$/.test(record.releaseVersion)) {
    fail(`破壊的変更記録のreleaseVersionが X.Y.Z の形式ではありません：${record.releaseVersion}`);
  }
  if (!Array.isArray(record.changes)) fail("公開Rust APIの破壊的変更記録のchangesが配列ではありません");
  const seen = new Set();
  for (const change of record.changes) {
    const changeKeys = change && typeof change === "object" ? Object.keys(change).sort() : [];
    if (JSON.stringify(changeKeys) !== JSON.stringify(CHANGE_KEYS)) {
      fail(`破壊的変更に未知または不足した項目があります：${changeKeys.join("、")}`);
    }
    for (const key of CHANGE_KEYS) {
      if (typeof change[key] !== "string" || change[key].trim() === "") {
        fail(`破壊的変更の${key}が空です`);
      }
    }
    const key = breakingFailureKey(change);
    if (seen.has(key)) fail(`破壊的変更の記録が重複しています：${key}`);
    seen.add(key);
  }
  return record;
}

export function loadBreakingRustApi() {
  const record = JSON.parse(
    readFileSync(new URL("release/breaking-rust-api.json", ROOT), "utf8"),
  );
  return validateBreakingRustApi(record);
}
