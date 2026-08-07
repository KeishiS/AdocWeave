// Compares the candidate's public Rust API against the previous stable tag.
//
// The check exists because compiling and passing tests says nothing about
// whether a release removed or changed an item that a consumer depends on.
// The release step selects the comparison policy. Patch releases reject every
// breaking difference; minor and major releases accept only differences that
// exactly match the machine-readable record used to build the Release Notes.
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import process from "node:process";

import {
  breakingFailureKey,
  loadBreakingRustApi,
  validateBreakingRustApi,
} from "./breaking-rust-api.mjs";

const ROOT = new URL("../", import.meta.url);
const read = (path) => readFileSync(new URL(path, ROOT), "utf8");
const json = (path) => JSON.parse(read(path));

/// Crates whose public Rust API this gate compares.
///
/// The list is written out rather than derived so that adding a workspace
/// member is a reviewed decision. `cargo semver-checks --workspace` checks
/// nothing here because every crate sets `publish = false`, so naming each
/// crate is also what makes the check run at all.
export const CHECKED_CRATES = [
  "adocweave",
  "adocweave-config",
  "adocweave-host",
  "adocweave-lsp",
  "adocweave-textlint",
  "adocweave-textlint-wasm",
  "adocweave-wasm",
  "adocweave-workspace",
];

/// Versions in which public library crates first joined the release train.
///
/// A crate cannot be compared with a baseline from before it existed. Keeping
/// the introduction version explicit makes the exemption expire
/// automatically once the selected baseline contains the crate.
export const CRATE_INTRODUCTIONS = {
  "adocweave-textlint": { major: 0, minor: 30, patch: 0 },
  "adocweave-textlint-wasm": { major: 0, minor: 29, patch: 0 },
};

/// Workspace members this gate does not compare, and why.
///
/// Only a library target carries a Rust API that another crate can name, so a
/// crate that ships a binary alone has nothing for this gate to compare. The
/// reason is written down, and the tests require every member to appear either
/// here or in [`CHECKED_CRATES`], so a crate cannot fall out of the check by
/// being forgotten.
export const UNCHECKED_CRATES = {
  "adocweave-cli": "binaryだけを提供し、library targetを持たないため",
};

export const STABLE_TAG = /^v(\d+)\.(\d+)\.(\d+)$/;

const fail = (message) => {
  throw new Error(message);
};

/// Parses `X.Y.Z`, rejecting anything a release train must not carry.
export function parseVersion(value, label) {
  const match = /^(\d+)\.(\d+)\.(\d+)$/.exec(value);
  if (!match) fail(`${label}が X.Y.Z の形式ではありません：${value}`);
  return { major: Number(match[1]), minor: Number(match[2]), patch: Number(match[3]) };
}

/// Names the release step from the baseline and candidate versions.
///
/// Under 0.y the minor position carries breaking changes, so a step that only
/// raises the minor number is a `minor` release here and the slot in which
/// cargo-semver-checks permits a breaking difference.
export function releaseStep(baseline, candidate) {
  if (candidate.major !== baseline.major) {
    if (candidate.major < baseline.major) fail("候補versionがbaselineより古いmajor版です");
    return "major";
  }
  if (candidate.minor !== baseline.minor) {
    if (candidate.minor < baseline.minor) fail("候補versionがbaselineより古いminor版です");
    return "minor";
  }
  if (candidate.patch <= baseline.patch) {
    fail(`候補versionがbaseline以下です：${format(baseline)} -> ${format(candidate)}`);
  }
  return "patch";
}

/// Selects the cargo-semver-checks policy used to enumerate differences.
///
/// A minor comparison permits additive minor changes but still reports major
/// differences. We intentionally use it for a major release as well because a
/// release gate must enumerate those differences before accepting them.
export function comparisonReleaseType(step) {
  return step === "patch" ? "patch" : "minor";
}

const format = ({ major, minor, patch }) => `${major}.${minor}.${patch}`;

/// Picks the newest stable tag below the candidate version.
///
/// Tags at or above the candidate are ignored so re-running the gate after a
/// tag exists compares against the release before it, not against itself.
export function baselineTag(tags, candidate) {
  const ordered = tags
    .map((tag) => ({ tag, version: STABLE_TAG.exec(tag) }))
    .filter(({ version }) => version !== null)
    .map(({ tag, version }) => ({
      tag,
      version: { major: Number(version[1]), minor: Number(version[2]), patch: Number(version[3]) },
    }))
    .filter(({ version }) => compare(version, candidate) < 0)
    .sort((left, right) => compare(left.version, right.version));
  const newest = ordered.at(-1);
  if (!newest) {
    fail(
      `候補version ${format(candidate)} より前のstable tagがありません。` +
        "初回導入時はbaselineとするtagを先に公開してください。",
    );
  }
  return newest;
}

/// Selects public crates that exist in the chosen baseline.
export function cratesForBaseline(baseline) {
  return CHECKED_CRATES.filter((name) => {
    const introduced = CRATE_INTRODUCTIONS[name];
    return (
      introduced === undefined ||
      compare(introduced, baseline) <= 0
    );
  });
}

function compare(left, right) {
  return (
    left.major - right.major || left.minor - right.minor || left.patch - right.patch
  );
}

/// Reads back which crates cargo-semver-checks actually compared.
///
/// A crate the tool skips reports nothing and leaves the exit status at zero,
/// so the gate confirms every named crate appears before trusting the result.
export function checkedCrates(output) {
  return [...output.matchAll(/^\s*Checking (\S+) v/gm)].map((match) => match[1]);
}

/// Parses every cargo-semver-checks failure block without discarding unknown data.
export function reportedFailureBlocks(output) {
  const blocks = [];
  let block = null;
  const finishBlock = () => {
    if (!block) return;
    if (!block.failedIn) {
      fail(`cargo-semver-checksのfailure ${block.lint}にFailed inがありません`);
    }
    if (block.items.length === 0) {
      fail(`cargo-semver-checksのfailure ${block.lint}に解析できる対象がありません`);
    }
    blocks.push(block);
  };
  for (const line of output.split("\n")) {
    const heading = /^--- failure ([\w-]+): (.+?) ---$/.exec(line);
    if (heading) {
      finishBlock();
      block = { lint: heading[1], summary: heading[2], failedIn: false, items: [] };
      continue;
    }
    if (/^--- failure\b/.test(line)) {
      fail(`cargo-semver-checksのfailure見出しを解析できません：${line}`);
    }
    if (!block) continue;
    if (/^\s*Failed in:\s*$/.test(line)) {
      if (block.failedIn) fail(`cargo-semver-checksのfailure ${block.lint}にFailed inが重複しています`);
      block.failedIn = true;
      continue;
    }
    if (!block.failedIn) continue;
    if (line.trim() === "") continue;
    const item = /^\s{2,}(.+?)\s+in\s+(.+):\d+(?::\d+)?\s*$/.exec(line);
    if (!item) {
      fail(`cargo-semver-checksのfailure ${block.lint}に解析できない対象があります：${line}`);
    }
    const crate = /[\\/]crates[\\/]([\w-]+)[\\/]/.exec(item[2])?.[1];
    if (!crate) fail(`cargo-semver-checksのfailure ${block.lint}のcrateをpathから特定できません：${line}`);
    block.items.push({ crate, item: item[1] });
  }
  finishBlock();
  return blocks;
}

/// Reads back every concrete breaking difference in validated failure blocks.
export function reportedFailures(output) {
  return reportedFailureBlocks(output).flatMap(({ lint, summary, items }) =>
    items.map(({ crate, item }) => ({ crate, lint, summary, item })),
  );
}

/// Reads back which crate comparisons reached cargo-semver-checks' terminal line.
export function finishedCrates(output) {
  return [...output.matchAll(/^\s*Finished \[[^\]]+\]\s+(\S+)\s*$/gm)].map((match) => match[1]);
}

/// Distinguishes reported API differences from an abnormal tool termination.
export function verifySemverResult({
  candidate,
  step,
  status,
  failures,
  signal,
  record,
  expectedCrates,
  completedCrates,
}) {
  validateBreakingRustApi(record);
  if (record.releaseVersion !== format(candidate)) {
    fail(
      `破壊的変更記録のreleaseVersionが候補と一致しません：` +
        `${record.releaseVersion} != ${format(candidate)}`,
    );
  }
  if (signal !== null) {
    fail(`cargo-semver-checksがsignal ${signal}で異常終了しました`);
  }
  const expectedStatus = failures.length > 0 ? 1 : 0;
  if (status !== expectedStatus) {
    fail(
      `cargo-semver-checksの終了ステータスが不正です：` +
        `${String(status)}（期待値 ${expectedStatus}）`,
    );
  }
  const incomplete = expectedCrates.filter((name) => !completedCrates.includes(name));
  if (incomplete.length > 0) {
    fail(`cargo-semver-checksが次のcrateの比較を完了せず、異常終了しました：${incomplete.join("、")}`);
  }
  if (failures.length === 0) {
    requireRecordedBreakingChanges(candidate, failures, record);
    return;
  }
  if (step === "patch") {
    fail(
      `patch releaseに破壊的変更が ${failures.length} 件あります。` +
        "破壊的変更はminor版へ載せてください。",
    );
  }
  requireRecordedBreakingChanges(candidate, failures, record);
}

/// Runs a command and returns both streams.
///
/// cargo-semver-checks reports its progress and findings on standard error, so
/// reading only standard output would leave the gate unable to tell a crate it
/// compared from one it never reached.
function run(command, args) {
  const result = spawnSync(command, args, { cwd: ROOT, encoding: "utf8" });
  if (result.error) throw result.error;
  return {
    status: result.status,
    signal: result.signal,
    stdout: result.stdout ?? "",
    stderr: result.stderr ?? "",
    output: `${result.stdout ?? ""}${result.stderr ?? ""}`,
  };
}

function main() {
  const candidate = parseVersion(json("release-manifest.json").packageVersion, "候補version");
  const tags = run("git", ["tag", "--list", "v*"]).output.split("\n").map((tag) => tag.trim());
  const baseline = baselineTag(tags, candidate);
  const step = releaseStep(baseline.version, candidate);
  process.stdout.write(
    `公開Rust APIを ${baseline.tag} と比較します：` +
      `${format(baseline.version)} -> ${format(candidate)}（${step} release）\n`,
  );

  const known = run("git", ["rev-parse", "--verify", `${baseline.tag}^{commit}`]);
  if (known.status !== 0) fail(`baseline tag ${baseline.tag} がこのcloneにありません`);

  const expectedCrates = cratesForBaseline(baseline.version);
  const newCrates = CHECKED_CRATES.filter((name) => !expectedCrates.includes(name));
  if (newCrates.length > 0) {
    process.stdout.write(
      `baselineより後に追加されたcrateは初回比較を省略します：${newCrates.join("、")}\n`,
    );
  }
  const packages = expectedCrates.flatMap((name) => ["--package", name]);
  const { status, signal, stdout, output } = run("cargo", [
    "semver-checks",
    "--baseline-rev",
    baseline.tag,
    "--release-type",
    comparisonReleaseType(step),
    ...packages,
  ]);
  process.stdout.write(output);

  const checked = checkedCrates(output);
  const missing = expectedCrates.filter((name) => !checked.includes(name));
  if (missing.length > 0) {
    fail(
      `次のcrateが比較されませんでした：${missing.join("、")}。` +
        "対象から外れたまま検査に成功しないよう、失敗として扱います。",
    );
  }

  const failures = reportedFailures(stdout);
  verifySemverResult({
    candidate,
    step,
    status,
    signal,
    failures,
    record: loadBreakingRustApi(),
    expectedCrates,
    completedCrates: finishedCrates(output),
  });
  process.stdout.write(
    `公開Rust APIを検査しました：${checked.length} crate、破壊的変更 ${failures.length} 件\n`,
  );
}

/// Requires the Release Notes to describe every accepted breaking change.
///
/// A breaking change that only the tool knows about leaves consumers to
/// discover it at compile time, so an accepted difference must arrive with a
/// migration step written for a reader.
export function requireRecordedBreakingChanges(candidate, failures, record) {
  const detected = new Map(failures.map((failure) => [breakingFailureKey(failure), failure]));
  if (detected.size !== failures.length) fail("cargo-semver-checksが同じ破壊的変更を重複して報告しました");
  const recorded = new Map(record.changes.map((change) => [breakingFailureKey(change), change]));
  const missing = [...detected.keys()].filter((key) => !recorded.has(key));
  const extra = [...recorded.keys()].filter((key) => !detected.has(key));
  if (missing.length > 0 || extra.length > 0) {
    fail(
      "公開Rust APIの破壊的変更記録が検出結果と一致しません。" +
        `未記録 ${missing.length} 件、余分 ${extra.length} 件`,
    );
  }
  for (const [key, failure] of detected) {
    if (recorded.get(key).summary !== failure.summary) {
      fail(`破壊的変更のsummaryが検出結果と一致しません：${failure.lint}`);
    }
  }
  if (failures.length > 0) {
    process.stdout.write(
      `v${format(candidate)}の破壊的変更 ${failures.length} 件はRelease Notesに記載があります\n`,
    );
  }
}

if (process.argv[1] && import.meta.url === new URL(process.argv[1], "file:").href) {
  try {
    main();
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exit(1);
  }
}
