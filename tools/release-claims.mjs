import { readFileSync } from "node:fs";
import process from "node:process";
import { spawnSync } from "node:child_process";

import {
  CONTRACT_SOURCES,
  CONTRACT_VERSION_FIELDS,
  RELEASE_NOTES_VERSION,
  UNCHANGED_CONTRACTS,
} from "./release-notes.mjs";
import { baselineTag } from "./semver-gate.mjs";

function fail(message) {
  throw new Error(message);
}

function run(command, args) {
  const result = spawnSync(command, args, { encoding: "utf8" });
  return { status: result.status, output: `${result.stdout ?? ""}${result.stderr ?? ""}` };
}

function parseVersion(value) {
  const match = /^(\d+)\.(\d+)\.(\d+)$/.exec(value);
  if (!match) fail(`候補versionがSemVerではありません：${value}`);
  return { major: Number(match[1]), minor: Number(match[2]), patch: Number(match[3]) };
}

/// Reads one contract source as it stood at a commit.
function contractAt(revision, path) {
  const { status, output } = run("git", ["show", `${revision}:${path}`]);
  if (status !== 0) fail(`${revision} の ${path} を読めません：${output.trim()}`);
  return output;
}

/// Compares a contract while ignoring the fields that carry the release version.
///
/// Every release changes `packageVersion`, so a byte comparison would report a
/// difference for a contract whose shape nobody touched.
export function contractShape(source, path) {
  if (!path.endsWith(".json")) return source;
  const value = JSON.parse(source);
  for (const field of CONTRACT_VERSION_FIELDS) delete value[field];
  return JSON.stringify(value, null, 2);
}

export function changedContracts(baseline, sources, read) {
  return Object.entries(sources)
    .filter(([, path]) => {
      const before = contractShape(read(baseline, path), path);
      const after = contractShape(read("HEAD", path), path);
      return before !== after;
    })
    .map(([name]) => name);
}

function main() {
  const candidate = parseVersion(JSON.parse(readFileSync("release-manifest.json", "utf8")).packageVersion);
  if (`${candidate.major}.${candidate.minor}.${candidate.patch}` !== RELEASE_NOTES_VERSION) {
    fail("release manifestとRelease Notesのversionが一致しません");
  }
  const tags = run("git", ["tag", "--list", "v*"]).output.split("\n").map((tag) => tag.trim());
  const baseline = baselineTag(tags, candidate);
  const known = run("git", ["rev-parse", "--verify", `${baseline.tag}^{commit}`]);
  if (known.status !== 0) fail(`baseline tag ${baseline.tag} がこのcloneにありません`);

  const declared = UNCHANGED_CONTRACTS.filter((name) => name in CONTRACT_SOURCES);
  const unchecked = UNCHANGED_CONTRACTS.filter((name) => !(name in CONTRACT_SOURCES));
  const sources = Object.fromEntries(declared.map((name) => [name, CONTRACT_SOURCES[name]]));
  const changed = changedContracts(baseline.tag, sources, contractAt);

  if (changed.length > 0) {
    fail(
      `Release Notesは変更していないと述べていますが、${baseline.tag}から実際に変わっています：` +
        `${changed.map((name) => `${name}（${CONTRACT_SOURCES[name]}）`).join("、")}`,
    );
  }

  process.stdout.write(
    `Release Notesの「変更していません」を${baseline.tag}と照合しました：` +
      `検査した契約 ${declared.length > 0 ? declared.join("、") : "なし"}` +
      `${unchecked.length > 0 ? `、正本が一つに定まらないため未検査 ${unchecked.join("、")}` : ""}\n`,
  );
}

if (process.argv[1] && import.meta.url === new URL(process.argv[1], "file:").href) {
  try {
    main();
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exit(1);
  }
}
