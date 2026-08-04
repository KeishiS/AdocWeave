import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import test from "node:test";

import {
  assertDiagnostics,
  fixtureSource,
  runTextlintPluginReleaseSmoke,
} from "./textlint-plugin-release-smoke.mjs";

test("公開tgzを実CLI相当の経路で検査する", async () => {
  const { archive, root } = await createFixtureArchive();
  const invocations = [];
  try {
    await runTextlintPluginReleaseSmoke(archive, {
      installPackage: async ({ archive: installedArchive, cwd }) => {
        assert.equal(installedArchive, archive);
        await writeManifest(cwd, "@adocweave/textlint-plugin-asciidoc", "15.8.0");
      },
      invokeTextlint: async ({ args, cli, cwd, input }) => {
        assert.equal(cli, join(cwd, "node_modules", "textlint", "bin", "textlint.js"));
        assert.deepEqual(args.slice(0, 6), [
          "--config", join(cwd, ".textlintrc.json"),
          "--rulesdir", join(cwd, "rules"),
          "--format", "json",
        ]);
        const config = JSON.parse(await readFile(join(cwd, ".textlintrc.json"), "utf8"));
        assert.deepEqual(config.plugins, ["@adocweave/asciidoc"]);
        assert.deepEqual(config.rules, {});
        const paths = input === undefined
          ? args.filter((argument) => /\.(?:adoc|asciidoc|asc)$/.test(argument))
          : [args[args.indexOf("--stdin-filename") + 1]];
        invocations.push({ args, input, paths });
        if (args.includes("--fix")) {
          return { code: 0, stderr: "", stdout: fixedReports(paths) };
        }
        return { code: 1, stderr: "", stdout: diagnostics(paths) };
      },
    });
  } finally {
    await rm(root, { recursive: true, force: true });
  }

  assert.equal(invocations.length, 5);
  assert.deepEqual(invocations[0].paths.map((path) => basename(path)), [
    "sample.adoc",
    "sample.asciidoc",
    "sample.asc",
  ]);
  assert.deepEqual(invocations.slice(1, 4).map(({ paths }) => basename(paths[0])), [
    "stdin.adoc",
    "stdin.asciidoc",
    "stdin.asc",
  ]);
  assert.ok(invocations.slice(1, 4).every(({ args }) => args.includes("--stdin")));
  assert.equal(invocations[1].input, fixtureSource("\n"));
  assert.equal(invocations[2].input, fixtureSource("\r\n"));
  assert.ok(invocations[4].args.includes("--fix"));
});

test("--fixによる入力変更を検出する", async () => {
  const { archive, root } = await createFixtureArchive();
  try {
    await assert.rejects(
      runTextlintPluginReleaseSmoke(archive, {
        installPackage: async ({ cwd }) => writeManifest(
          cwd,
          "@adocweave/textlint-plugin-asciidoc",
          "15.8.0",
        ),
        invokeTextlint: async ({ args, cwd, input }) => {
          const paths = input === undefined
            ? args.filter((argument) => /\.(?:adoc|asciidoc|asc)$/.test(argument))
            : [args[args.indexOf("--stdin-filename") + 1]];
          if (args.includes("--fix")) {
            const original = await readFile(paths[0], "utf8");
            await writeFile(paths[0], original.replace("誤り", "修正"));
            return { code: 0, stderr: "", stdout: fixedReports(paths) };
          }
          return { code: 1, stderr: "", stdout: diagnostics(paths) };
        },
      }),
      /--fix changed input bytes: sample\.adoc/,
    );
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("診断位置のずれを検出する", () => {
  assert.throws(
    () => assertDiagnostics(JSON.stringify([{
      filePath: "/tmp/sample.adoc",
      messages: [{ ruleId: "probe", line: 3, column: 5 }],
    }]), ["/tmp/sample.adoc"]),
    /5 !== 6/,
  );
});

test("fixtureは日本語、emoji、結合文字、指定した改行を含む", () => {
  const lf = fixtureSource("\n");
  const crlf = fixtureSource("\r\n");
  assert.match(lf, /題名/);
  assert.match(lf, /😀/u);
  assert.match(lf, /e\u0301/u);
  assert.equal(lf.includes("\r"), false);
  assert.equal(crlf.split("\r\n").length, 4);
});

async function createFixtureArchive() {
  const root = await mkdtemp(join(tmpdir(), "adocweave-textlint-smoke-test-"));
  const archive = join(root, "plugin.tgz");
  await writeFile(archive, "fixture");
  return { archive, root };
}

async function writeManifest(cwd, packageName, textlintVersion) {
  const plugin = join(cwd, "node_modules", "@adocweave", "textlint-plugin-asciidoc");
  const textlint = join(cwd, "node_modules", "textlint");
  await mkdir(join(textlint, "bin"), { recursive: true });
  await mkdir(plugin, { recursive: true });
  await writeFile(join(plugin, "package.json"), JSON.stringify({ name: packageName }));
  await writeFile(join(textlint, "package.json"), JSON.stringify({ version: textlintVersion }));
}

function diagnostics(paths) {
  return JSON.stringify(paths.map((filePath) => ({
    filePath,
    messages: [{
      column: 6,
      line: 3,
      message: "検査用の指摘です。",
      ruleId: "probe",
      severity: 2,
    }],
  })));
}

function fixedReports(paths) {
  return JSON.stringify(paths.map((filePath) => ({
    applyingMessages: [],
    filePath,
    messages: [],
    output: fixtureSource(filePath.endsWith(".asciidoc") ? "\r\n" : "\n"),
  })));
}
