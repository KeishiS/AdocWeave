import { mkdirSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

const LOCAL_TARGET_EXCLUSIONS = [
  "editors/vscode/test/fixtures/",
  "fixtures/",
  "fuzz/",
];

function parseNullSeparatedPaths(output) {
  const text = new TextDecoder("utf-8", { fatal: true }).decode(output);
  const paths = text.split("\0");
  if (paths.at(-1) === "") paths.pop();
  return paths;
}

export function trackedAdocPlan(paths) {
  const sorted = [...paths].sort((left, right) => left.localeCompare(right, "en"));
  const unique = new Set(sorted);
  if (unique.size !== sorted.length) throw new Error("追跡対象のAsciiDoc pathが重複しています");
  for (const path of sorted) {
    if (!path.endsWith(".adoc")) throw new Error(`AsciiDoc以外のpathが含まれています: ${path}`);
  }
  return sorted.map((path) => ({
    path,
    failOn: LOCAL_TARGET_EXCLUSIONS.some((prefix) => path.startsWith(prefix)) ? "error" : "warning",
    localTargets: !LOCAL_TARGET_EXCLUSIONS.some((prefix) => path.startsWith(prefix)),
  }));
}

export function trackedAdocPaths(git = spawnSync) {
  const result = git("git", ["ls-files", "-z", "--", "*.adoc"], { encoding: null });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`追跡対象の取得に失敗しました\n${result.stderr?.toString("utf8") ?? ""}`);
  }
  return parseNullSeparatedPaths(result.stdout);
}

function run(command, args) {
  return spawnSync(command, args, { encoding: "utf8" });
}

function showFailure(path, result, suffix = "") {
  process.stderr.write(`[${path}]${suffix}\n`);
  if (result.stdout) process.stderr.write(result.stdout);
  if (result.stderr) process.stderr.write(result.stderr);
}

export function main() {
  const executable = "target/debug/adocweave";
  const diagnosticsLog = "target/adoc-check-diagnostics.log";
  const plan = trackedAdocPlan(trackedAdocPaths());
  mkdirSync(dirname(diagnosticsLog), { recursive: true });
  writeFileSync(diagnosticsLog, "");

  for (const config of [".adocweave.toml", "fixtures/.adocweave.toml", "fuzz/.adocweave.toml"]) {
    const result = run(executable, ["config", "show", "--config", config]);
    if (result.status !== 0) {
      showFailure(config, result, " 設定の検証に失敗しました");
      return 1;
    }
  }

  let diagnostics = 0;
  let failed = false;
  for (const entry of plan) {
    const result = run(executable, ["check", "--fail-on", entry.failOn, entry.path]);
    const output = `${result.stdout}${result.stderr}`;
    if (result.status !== 0) {
      showFailure(entry.path, result);
      failed = true;
    } else if (output) {
      writeFileSync(diagnosticsLog, `[${entry.path}]\n${output}`, { flag: "a" });
      diagnostics += output.trimEnd().split(/\r?\n/).length;
    }
  }
  if (failed) {
    process.stderr.write(`adoc-checkに失敗しました: ${plan.length}文書\n`);
    return 1;
  }

  const localEntries = plan.filter((entry) => entry.localTargets);
  for (const entry of localEntries) {
    const result = run(executable, [
      "check",
      "--fail-on",
      "warning",
      "--local-targets",
      "--project-root",
      ".",
      entry.path,
    ]);
    if (result.status !== 0) {
      showFailure(entry.path, result, " local target検査に失敗しました");
      failed = true;
    }
  }
  if (failed) {
    process.stderr.write(`adoc-checkに失敗しました: local target検査 ${localEntries.length}文書\n`);
    return 1;
  }

  process.stdout.write(
    `adoc-checkに成功しました: ${plan.length}文書、記録した想定内の診断 ${diagnostics}件（${diagnosticsLog}）\n`,
  );
  return 0;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) process.exitCode = main();
