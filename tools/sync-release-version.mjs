import {
  existsSync,
  readFileSync,
  readdirSync,
  statSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { spawnSync } from "node:child_process";
import { resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const ROOT = new URL("../", import.meta.url);
const STABLE_VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const IGNORED_DIRECTORIES = new Set([
  ".agents",
  ".git",
  ".vscode-test",
  "node_modules",
  "target",
]);

function fail(message) {
  throw new Error(message);
}

function exactKeys(value, keys, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    fail(`${label}はobjectである必要があります`);
  }
  const actual = Object.keys(value).sort();
  const expected = [...keys].sort();
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    fail(`${label}に不足、余分または未知のfieldがあります`);
  }
}

function safePath(path, label) {
  if (
    typeof path !== "string" ||
    path.length === 0 ||
    path.startsWith("/") ||
    path.includes("\\") ||
    path.split("/").some((part) => part === "" || part === "." || part === "..")
  ) {
    fail(`${label}のpathが安全なrepository相対pathではありません`);
  }
}

function occurrences(source, literal) {
  if (literal.length === 0) fail("空文字列は検索できません");
  let count = 0;
  let offset = 0;
  while ((offset = source.indexOf(literal, offset)) !== -1) {
    count += 1;
    offset += literal.length;
  }
  return count;
}

function versionTemplate(template, label) {
  if (
    typeof template !== "string" ||
    occurrences(template, "{version}") !== 1
  ) {
    fail(`${label}のtemplateは{version}を1回だけ含める必要があります`);
  }
}

function positiveCount(count, label, allowZero = false) {
  if (
    !Number.isInteger(count) ||
    count < (allowZero ? 0 : 1)
  ) {
    fail(`${label}のcountが不正です`);
  }
}

export function validateRegistry(registry) {
  exactKeys(
    registry,
    ["schemaVersion", "authority", "targets", "generators", "preserved"],
    "version同期registry",
  );
  if (registry.schemaVersion !== 1) {
    fail("version同期registryのschemaVersionは1である必要があります");
  }

  exactKeys(
    registry.authority,
    ["type", "path", "template", "count"],
    "authority",
  );
  if (registry.authority.type !== "literal") {
    fail("authorityのtypeはliteralである必要があります");
  }
  safePath(registry.authority.path, "authority");
  versionTemplate(registry.authority.template, "authority");
  positiveCount(registry.authority.count, "authority");

  if (!Array.isArray(registry.targets) || registry.targets.length === 0) {
    fail("targetsは空でないarrayである必要があります");
  }
  const locators = new Set();
  for (const [index, target] of registry.targets.entries()) {
    const label = `targets[${index}]`;
    if (target?.type === "literal") {
      exactKeys(target, ["type", "path", "template", "count"], label);
      versionTemplate(target.template, label);
      positiveCount(target.count, label);
    } else if (target?.type === "cargo-lock") {
      exactKeys(target, ["type", "path", "packages"], label);
      if (
        !Array.isArray(target.packages) ||
        target.packages.length === 0 ||
        target.packages.some(
          (name) => typeof name !== "string" || !/^[a-z0-9-]+$/.test(name),
        ) ||
        new Set(target.packages).size !== target.packages.length
      ) {
        fail(`${label}.packagesは重複のないpackage名である必要があります`);
      }
    } else {
      fail(`${label}.typeはliteralまたはcargo-lockである必要があります`);
    }
    safePath(target.path, label);
    const identity = target.type === "literal"
      ? `${target.path}\0literal\0${target.template}`
      : `${target.path}\0cargo-lock\0${target.packages.join(",")}`;
    if (locators.has(identity)) fail(`${label}は重複しています`);
    locators.add(identity);
  }
  const authorityIdentity =
    `${registry.authority.path}\0literal\0${registry.authority.template}`;
  if (locators.has(authorityIdentity)) {
    fail("authorityをtargetsへ重複登録できません");
  }

  if (
    !Array.isArray(registry.generators) ||
    registry.generators.length === 0
  ) {
    fail("generatorsは空でないarrayである必要があります");
  }
  const generatorIds = new Set();
  const outputPaths = new Set();
  for (const [index, generator] of registry.generators.entries()) {
    const label = `generators[${index}]`;
    exactKeys(generator, ["id", "outputs"], label);
    if (!["protocol", "public-conformance"].includes(generator.id)) {
      fail(`${label}のgenerator IDは許可されていません`);
    }
    if (generatorIds.has(generator.id)) fail(`${label}のIDは重複しています`);
    generatorIds.add(generator.id);
    if (!Array.isArray(generator.outputs) || generator.outputs.length === 0) {
      fail(`${label}.outputsは空でないarrayである必要があります`);
    }
    for (const [outputIndex, output] of generator.outputs.entries()) {
      const outputLabel = `${label}.outputs[${outputIndex}]`;
      exactKeys(output, ["path", "versionOccurrences"], outputLabel);
      safePath(output.path, outputLabel);
      positiveCount(output.versionOccurrences, outputLabel, true);
      if (outputPaths.has(output.path)) {
        fail(`${outputLabel}のpathは重複しています`);
      }
      outputPaths.add(output.path);
    }
  }
  if (
    generatorIds.size !== 2 ||
    !generatorIds.has("protocol") ||
    !generatorIds.has("public-conformance")
  ) {
    fail("protocolとpublic-conformanceのgeneratorを1件ずつ登録してください");
  }

  if (!Array.isArray(registry.preserved)) {
    fail("preservedはarrayである必要があります");
  }
  const preservedLocators = new Set();
  for (const [index, preserved] of registry.preserved.entries()) {
    const label = `preserved[${index}]`;
    exactKeys(preserved, ["path", "literal", "count"], label);
    safePath(preserved.path, label);
    if (typeof preserved.literal !== "string" || preserved.literal.length === 0) {
      fail(`${label}.literalは空でない文字列である必要があります`);
    }
    positiveCount(preserved.count, label);
    const identity = `${preserved.path}\0${preserved.literal}`;
    if (preservedLocators.has(identity)) fail(`${label}は重複しています`);
    preservedLocators.add(identity);
  }
  return registry;
}

function absolute(root, path) {
  return resolve(fileURLToPath(root), path);
}

function read(root, path) {
  const url = absolute(root, path);
  if (!existsSync(url) || !statSync(url).isFile()) {
    fail(`管理対象fileがありません：${path}`);
  }
  return readFileSync(url, "utf8");
}

function render(template, version) {
  return template.replace("{version}", version);
}

function compareVersions(left, right) {
  const leftParts = left.split(".").map(BigInt);
  const rightParts = right.split(".").map(BigInt);
  for (let index = 0; index < leftParts.length; index += 1) {
    if (leftParts[index] < rightParts[index]) return -1;
    if (leftParts[index] > rightParts[index]) return 1;
  }
  return 0;
}

function validateLocator(root, locator, version, label) {
  if (locator.type === "cargo-lock") {
    cargoLockBlocks(read(root, locator.path), locator, version, label);
    return;
  }
  const literal = render(locator.template, version);
  const actual = occurrences(read(root, locator.path), literal);
  if (actual !== locator.count) {
    fail(
      `${label} ${locator.path} のversion記録数が不正です：期待${locator.count}件、実際${actual}件`,
    );
  }
}

function cargoLockBlocks(source, locator, version, label) {
  const blocks = source.split(/\n\n/);
  const selected = [];
  for (const packageName of locator.packages) {
    const matches = blocks.filter(
      (block) =>
        new RegExp(`^name = "${packageName}"$`, "m").test(block),
    );
    if (matches.length !== 1) {
      fail(
        `${label} ${locator.path} のpackage ${packageName}が一意ではありません`,
      );
    }
    const block = matches[0];
    if (/^source = /m.test(block)) {
      fail(`${label} ${locator.path} のpackage ${packageName}はlocal packageではありません`);
    }
    const expected = `version = "${version}"`;
    if (occurrences(block, expected) !== 1) {
      fail(
        `${label} ${locator.path} のpackage ${packageName}がversion ${version}ではありません`,
      );
    }
    selected.push(block);
  }
  return { blocks, selected };
}

function validatePreserved(root, preserved) {
  const actual = occurrences(read(root, preserved.path), preserved.literal);
  if (actual !== preserved.count) {
    fail(
      `保持対象 ${preserved.path} の記録数が不正です：期待${preserved.count}件、実際${actual}件`,
    );
  }
}

function walkedSourceFiles(root, directory = "", result = new Map()) {
  const url = absolute(root, directory || "./");
  for (const entry of readdirSync(url, { withFileTypes: true })) {
    if (IGNORED_DIRECTORIES.has(entry.name)) continue;
    const path = directory ? `${directory}/${entry.name}` : entry.name;
    if (entry.isDirectory()) {
      walkedSourceFiles(root, path, result);
    } else if (entry.isFile()) {
      const buffer = readFileSync(absolute(root, path));
      if (!buffer.includes(0)) result.set(path, buffer.toString("utf8"));
    }
  }
  return result;
}

function sourceFiles(root) {
  const tracked = spawnSync(
    "git",
    [
      "-C",
      fileURLToPath(root),
      "ls-files",
      "--cached",
      "--others",
      "--exclude-standard",
      "-z",
    ],
    { encoding: "utf8" },
  );
  if (tracked.status !== 0) return walkedSourceFiles(root);
  const result = new Map();
  for (const path of tracked.stdout.split("\0").filter(Boolean)) {
    if (IGNORED_DIRECTORIES.has(path.split("/", 1)[0])) continue;
    const file = absolute(root, path);
    if (!existsSync(file) || !statSync(file).isFile()) continue;
    const buffer = readFileSync(file);
    if (!buffer.includes(0)) result.set(path, buffer.toString("utf8"));
  }
  return result;
}

function expectedVersionOccurrences(registry, version) {
  const expected = new Map();
  const add = (path, count) =>
    expected.set(path, (expected.get(path) ?? 0) + count);
  add(registry.authority.path, registry.authority.count);
  for (const target of registry.targets) {
    add(
      target.path,
      target.type === "literal"
        ? target.count
        : target.type === "cargo-lock"
          ? target.packages.length
          : 1,
    );
  }
  for (const generator of registry.generators) {
    for (const output of generator.outputs) {
      add(output.path, output.versionOccurrences);
    }
  }
  for (const preserved of registry.preserved) {
    add(
      preserved.path,
      occurrences(preserved.literal, version) * preserved.count,
    );
  }
  return expected;
}

function validateInventory(root, registry, version) {
  const expected = expectedVersionOccurrences(registry, version);
  const actual = new Map();
  for (const [path, source] of sourceFiles(root)) {
    const count = occurrences(source, version);
    if (count > 0) actual.set(path, count);
  }
  const paths = new Set([...expected.keys(), ...actual.keys()]);
  for (const path of [...paths].sort()) {
    const expectedCount = expected.get(path) ?? 0;
    const actualCount = actual.get(path) ?? 0;
    if (expectedCount !== actualCount) {
      fail(
        `version inventoryが不一致です：${path} は期待${expectedCount}件、実際${actualCount}件`,
      );
    }
  }
}

function validateState(root, registry, version) {
  validateLocator(root, registry.authority, version, "authority");
  for (const [index, target] of registry.targets.entries()) {
    validateLocator(root, target, version, `targets[${index}]`);
  }
  for (const preserved of registry.preserved) {
    validatePreserved(root, preserved);
  }
  validateInventory(root, registry, version);
}

function run(command, args, root) {
  const result = spawnSync(command, args, {
    cwd: root,
    stdio: "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    fail(`${command} ${args.join(" ")} が終了code ${result.status}で失敗しました`);
  }
}

export function runRepositoryGenerator({ id, mode, root }) {
  if (id === "protocol") {
    run(
      process.execPath,
      [
        "tools/generate-protocol.mjs",
        ...(mode === "check" ? ["--check"] : []),
      ],
      root,
    );
    return;
  }
  if (id === "public-conformance") {
    if (mode === "check") {
      run(
        "cargo",
        [
          "test",
          "--locked",
          "-p",
          "adocweave",
          "--test",
          "public_conformance_fixture",
          "public_fixture_regeneration_is_clean",
          "--",
          "--exact",
        ],
        root,
      );
    } else {
      run(
        "cargo",
        [
          "test",
          "--locked",
          "-p",
          "adocweave",
          "--test",
          "public_conformance_fixture",
          "regenerate_public_fixture_products",
          "--",
          "--ignored",
          "--exact",
        ],
        root,
      );
    }
    return;
  }
  fail(`未知のgeneratorです：${id}`);
}

function changedPaths(before, after) {
  const paths = new Set([...before.keys(), ...after.keys()]);
  return [...paths].filter((path) => before.get(path) !== after.get(path));
}

function restore(root, before, after) {
  for (const path of changedPaths(before, after)) {
    if (before.has(path)) {
      writeFileSync(absolute(root, path), before.get(path));
    } else if (existsSync(absolute(root, path))) {
      unlinkSync(absolute(root, path));
    }
  }
}

function updateLocators(root, locators, current, next) {
  const updated = new Map();
  for (const locator of locators) {
    const source = updated.get(locator.path) ?? read(root, locator.path);
    if (locator.type === "cargo-lock") {
      const { blocks, selected } = cargoLockBlocks(
        source,
        locator,
        current,
        locator.path,
      );
      const selectedSet = new Set(selected);
      updated.set(
        locator.path,
        blocks
          .map((block) =>
            selectedSet.has(block)
              ? block.replace(
                  `version = "${current}"`,
                  `version = "${next}"`,
                )
              : block,
          )
          .join("\n\n"),
      );
      continue;
    }
    const from = render(locator.template, current);
    const to = render(locator.template, next);
    const actual = occurrences(source, from);
    if (actual !== locator.count) {
      fail(
        `${locator.path}を更新できません：期待${locator.count}件、実際${actual}件`,
      );
    }
    updated.set(locator.path, source.split(from).join(to));
  }
  for (const [path, source] of updated) {
    writeFileSync(absolute(root, path), source);
  }
}

export function syncReleaseVersion({
  root = ROOT,
  mode,
  version,
  registry,
  runGenerator = runRepositoryGenerator,
}) {
  if (!(root instanceof URL) || root.protocol !== "file:") {
    fail("rootはfile URLである必要があります");
  }
  validateRegistry(registry);
  const authoritySource = read(root, registry.authority.path);
  const authorityPattern = registry.authority.template
    .replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
    .replace("\\{version\\}", "([0-9]+\\.[0-9]+\\.[0-9]+)");
  const matches = [...authoritySource.matchAll(new RegExp(authorityPattern, "g"))];
  if (matches.length !== registry.authority.count) {
    fail("authorityから現在のversionを一意に取得できません");
  }
  const current = matches[0][1];
  if (!STABLE_VERSION.test(current)) fail(`authorityのversionが不正です：${current}`);
  if (mode !== "check" && mode !== "update") fail(`未知のmodeです：${mode}`);
  if (mode === "update" && !STABLE_VERSION.test(version ?? "")) {
    fail(`更新先versionがstable SemVerではありません：${version ?? "<missing>"}`);
  }
  if (
    mode === "update" &&
    version !== current &&
    compareVersions(version, current) <= 0
  ) {
    fail(`更新先versionは現在のversionより大きい必要があります：${version}`);
  }

  validateState(root, registry, current);
  const before = sourceFiles(root);
  if (mode === "check") {
    try {
      for (const generator of registry.generators) {
        runGenerator({ id: generator.id, mode, root, generator });
      }
      const after = sourceFiles(root);
      const changed = changedPaths(before, after);
      if (changed.length > 0) {
        fail(`--checkがfileを変更しました：${changed.join(", ")}`);
      }
    } catch (error) {
      restore(root, before, sourceFiles(root));
      throw error;
    }
    process.stdout.write(`release version同期を検査しました：${current}\n`);
    return { current, version: current, changed: [] };
  }

  if (version === current) {
    process.stdout.write(`release versionはすでに${current}です\n`);
    return { current, version, changed: [] };
  }

  try {
    updateLocators(
      root,
      [...registry.targets, registry.authority],
      current,
      version,
    );
    for (const generator of registry.generators) {
      runGenerator({
        id: generator.id,
        mode,
        root,
        generator,
        current,
        version,
      });
    }
    validateState(root, registry, version);
    const after = sourceFiles(root);
    const allowed = new Set([
      registry.authority.path,
      ...registry.targets.map(({ path }) => path),
      ...registry.generators.flatMap(({ outputs }) =>
        outputs.map(({ path }) => path)),
    ]);
    const unexpected = changedPaths(before, after).filter(
      (path) => !allowed.has(path),
    );
    if (unexpected.length > 0) {
      fail(`同期処理が管理対象外を変更しました：${unexpected.join(", ")}`);
    }
    process.stdout.write(`release versionを${current}から${version}へ同期しました\n`);
    return { current, version, changed: changedPaths(before, after) };
  } catch (error) {
    restore(root, before, sourceFiles(root));
    throw error;
  }
}

function parseArguments(args) {
  if (args.length === 1 && args[0] === "--check") {
    return { mode: "check", version: undefined };
  }
  if (args.length === 2 && args[0] === "--version") {
    return { mode: "update", version: args[1] };
  }
  fail(
    "使用方法：node tools/sync-release-version.mjs --check | --version X.Y.Z",
  );
}

export function main(args) {
  const options = parseArguments(args);
  const registry = JSON.parse(
    readFileSync(new URL("release/version-sync.json", ROOT), "utf8"),
  );
  syncReleaseVersion({ ...options, registry });
}

if (process.argv[1] && import.meta.url === new URL(process.argv[1], "file:").href) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
