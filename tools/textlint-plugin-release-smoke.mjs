import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import {
  mkdtemp,
  mkdir,
  readFile,
  realpath,
  rm,
  stat,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

const TEXTLINT_VERSION = "15.8.0";
const PACKAGE_NAME = "@adocweave/textlint-plugin-asciidoc";
const PLUGIN_NAME = "@adocweave/asciidoc";
const EXPECTED_LINE = 3;
const EXPECTED_COLUMN = 6;

const fileCases = [
  { name: "sample.adoc", newline: "\n" },
  { name: "sample.asciidoc", newline: "\r\n" },
  { name: "sample.asc", newline: "\n" },
];

const stdinCases = [
  { name: "stdin.adoc", newline: "\n" },
  { name: "stdin.asciidoc", newline: "\r\n" },
  { name: "stdin.asc", newline: "\n" },
];

const probeRule = String.raw`"use strict";

module.exports = function probeRule(context) {
  const { Syntax, RuleError, fixer, getSource, report } = context;
  return {
    [Syntax.Str](node) {
      const source = getSource(node);
      const marker = "誤り";
      const index = source.indexOf(marker);
      if (index === -1) return;
      report(node, new RuleError("検査用の指摘です。", {
        index,
        fix: fixer.replaceTextRange(
          [node.range[0] + index, node.range[0] + index + marker.length],
          "修正",
        ),
      }));
    },
  };
};
`;

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  const [archive] = process.argv.slice(2);
  if (!archive) {
    process.stderr.write("usage: node tools/textlint-plugin-release-smoke.mjs PACKAGE_TGZ\n");
    process.exit(2);
  }
  await runTextlintPluginReleaseSmoke(archive);
  process.stdout.write(`textlint plugin release smoke passed: ${basename(archive)}\n`);
}

export async function runTextlintPluginReleaseSmoke(
  archive,
  {
    installPackage = installTextlintAndPlugin,
    invokeTextlint = invokeTextlintCli,
  } = {},
) {
  const archivePath = await realpath(resolve(archive));
  const archiveMetadata = await stat(archivePath);
  if (!archiveMetadata.isFile()) throw new Error(`package archive is not a file: ${archivePath}`);

  const root = await mkdtemp(join(tmpdir(), "adocweave-textlint-plugin-smoke-"));
  try {
    await writeFile(join(root, "package.json"), `${JSON.stringify({ private: true }, null, 2)}\n`);
    await installPackage({ archive: archivePath, cwd: root });
    await assertInstalledPackage(root);

    const rulesDirectory = join(root, "rules");
    await mkdir(rulesDirectory);
    await writeFile(join(rulesDirectory, "probe.js"), probeRule);
    const config = join(root, ".textlintrc.json");
    await writeFile(config, `${JSON.stringify({
      plugins: [PLUGIN_NAME],
      rules: {},
    }, null, 2)}\n`);

    const fixtures = join(root, "fixtures");
    await mkdir(fixtures);
    const inputByPath = new Map();
    for (const fixture of fileCases) {
      const path = join(fixtures, fixture.name);
      const input = fixtureSource(fixture.newline);
      await writeFile(path, input);
      inputByPath.set(path, Buffer.from(input));
    }

    const cli = join(root, "node_modules", "textlint", "bin", "textlint.js");
    const commonArguments = ["--config", config, "--rulesdir", rulesDirectory, "--format", "json"];
    const lintResult = await invokeTextlint({
      args: [...commonArguments, ...inputByPath.keys()],
      cli,
      cwd: root,
    });
    assert.equal(lintResult.code, 1, diagnosticForUnexpectedExit("file lint", lintResult));
    assertDiagnostics(lintResult.stdout, [...inputByPath.keys()]);

    for (const fixture of stdinCases) {
      const filename = join(fixtures, fixture.name);
      const stdinResult = await invokeTextlint({
        args: [...commonArguments, "--stdin", "--stdin-filename", filename],
        cli,
        cwd: root,
        input: fixtureSource(fixture.newline),
      });
      assert.equal(stdinResult.code, 1, diagnosticForUnexpectedExit(`stdin lint (${fixture.name})`, stdinResult));
      assertDiagnostics(stdinResult.stdout, [filename]);
    }

    const fixResult = await invokeTextlint({
      args: [...commonArguments, "--fix", ...inputByPath.keys()],
      cli,
      cwd: root,
    });
    assert.ok([0, 1].includes(fixResult.code), diagnosticForUnexpectedExit("--fix lint", fixResult));
    assertNoAppliedFixes(fixResult.stdout, inputByPath);
    for (const [path, expected] of inputByPath) {
      assert.deepEqual(await readFile(path), expected, `--fix changed input bytes: ${basename(path)}`);
    }
  } finally {
    await rm(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
}

export function assertNoAppliedFixes(stdout, inputByPath) {
  const reports = JSON.parse(stdout);
  assert.equal(reports.length, inputByPath.size, "--fix returned an unexpected number of file reports");
  for (const [path, expected] of inputByPath) {
    const report = reports.find(({ filePath }) => resolve(filePath) === resolve(path));
    assert.ok(report, `--fix returned no report for ${path}`);
    assert.equal(report.output, expected.toString("utf8"), `--fix returned changed output: ${basename(path)}`);
    assert.deepEqual(report.applyingMessages ?? [], [], `--fix applied a change: ${basename(path)}`);
  }
}

export function fixtureSource(newline) {
  return ["= 題名", "", "前😀e\u0301誤り後です。", ""].join(newline);
}

export function assertDiagnostics(stdout, expectedPaths) {
  let reports;
  try {
    reports = JSON.parse(stdout);
  } catch (error) {
    throw new Error(`textlint did not return JSON: ${error.message}\n${stdout}`);
  }
  assert.equal(reports.length, expectedPaths.length, "textlint returned an unexpected number of file reports");
  for (const expectedPath of expectedPaths) {
    const report = reports.find(({ filePath }) => resolve(filePath) === resolve(expectedPath));
    assert.ok(report, `textlint returned no report for ${expectedPath}`);
    assert.equal(report.messages.length, 1, `${basename(expectedPath)} has an unexpected number of diagnostics`);
    const [message] = report.messages;
    assert.equal(message.ruleId, "probe");
    assert.equal(message.line, EXPECTED_LINE);
    assert.equal(message.column, EXPECTED_COLUMN);
  }
}

async function assertInstalledPackage(root) {
  const manifestPath = join(root, "node_modules", "@adocweave", "textlint-plugin-asciidoc", "package.json");
  const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
  assert.equal(manifest.name, PACKAGE_NAME, `installed package name must be ${PACKAGE_NAME}`);
  const textlintManifest = JSON.parse(
    await readFile(join(root, "node_modules", "textlint", "package.json"), "utf8"),
  );
  assert.equal(textlintManifest.version, TEXTLINT_VERSION, `textlint must be pinned to ${TEXTLINT_VERSION}`);
}

async function installTextlintAndPlugin({ archive, cwd }) {
  const npm = process.platform === "win32" ? "npm.cmd" : "npm";
  const result = await runProcess(npm, [
    "install",
    "--ignore-scripts",
    "--no-audit",
    "--no-fund",
    "--save-exact",
    `textlint@${TEXTLINT_VERSION}`,
    archive,
  ], {
    cwd,
    env: {
      ...process.env,
      npm_config_cache: join(cwd, ".npm-cache"),
    },
  });
  if (result.code !== 0) throw new Error(diagnosticForUnexpectedExit("npm install", result));
}

async function invokeTextlintCli({ args, cli, cwd, input }) {
  return runProcess(process.execPath, [cli, ...args], {
    cwd,
    env: { ...process.env, npm_config_offline: "true" },
    input,
  });
}

function diagnosticForUnexpectedExit(operation, result) {
  return `${operation} exited with ${result.code}\nstdout:\n${result.stdout}\nstderr:\n${result.stderr}`;
}

function runProcess(command, args, { cwd, env = process.env, input } = {}) {
  return new Promise((resolveProcess, rejectProcess) => {
    const child = spawn(command, args, { cwd, env, stdio: ["pipe", "pipe", "pipe"] });
    const stdout = [];
    const stderr = [];
    child.stdout.on("data", (chunk) => stdout.push(chunk));
    child.stderr.on("data", (chunk) => stderr.push(chunk));
    child.once("error", rejectProcess);
    child.once("close", (code, signal) => resolveProcess({
      code: code ?? 128,
      signal,
      stderr: Buffer.concat(stderr).toString("utf8"),
      stdout: Buffer.concat(stdout).toString("utf8"),
    }));
    if (input === undefined) child.stdin.end();
    else child.stdin.end(input);
  });
}
