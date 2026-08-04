// Compares the candidate's public Rust API against the previous stable tag.
//
// The check exists because compiling and passing tests says nothing about
// whether a release removed or changed an item that a consumer depends on.
// cargo-semver-checks derives the allowed change set from the two versions, so
// a patch release fails on any breaking difference while a 0.y minor release
// accepts one that the Release Notes record.
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import process from "node:process";

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
  "adocweave-governance":
    "リポジトリ規約の検査だけを持ち、libraryが空で公開する型がないため",
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

/// Reads back the breaking differences cargo-semver-checks reported.
export function reportedFailures(output) {
  return [...output.matchAll(/^--- failure ([\w-]+): (.+?) ---$/gm)].map((match) => ({
    lint: match[1],
    summary: match[2],
  }));
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
    status: result.status ?? 1,
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
  const { status, output } = run("cargo", [
    "semver-checks",
    "--baseline-rev",
    baseline.tag,
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

  const failures = reportedFailures(output);
  if (status !== 0) {
    if (step === "patch") {
      fail(
        `patch releaseに破壊的変更が ${failures.length} 件あります。` +
          "破壊的変更はminor版へ載せてください。",
      );
    }
    fail(`公開Rust APIの検査が失敗しました（${failures.length} 件）`);
  }

  if (failures.length > 0 && step !== "patch") {
    requireRecordedBreakingChanges(candidate, failures);
  }
  process.stdout.write(
    `公開Rust APIを検査しました：${checked.length} crate、破壊的変更 ${failures.length} 件\n`,
  );
}

/// Requires the Release Notes to describe every accepted breaking change.
///
/// A breaking change that only the tool knows about leaves consumers to
/// discover it at compile time, so an accepted difference must arrive with a
/// migration step written for a reader.
function requireRecordedBreakingChanges(candidate, failures) {
  const notes = read("tools/release-notes.mjs");
  if (!notes.includes("破壊的変更：")) {
    fail(
      `公開Rust APIに破壊的変更が ${failures.length} 件ありますが、` +
        "Release Notesに「破壊的変更：」の記載がありません。",
    );
  }
  process.stdout.write(
    `v${format(candidate)}の破壊的変更 ${failures.length} 件はRelease Notesに記載があります\n`,
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
