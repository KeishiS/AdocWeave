import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { basename, dirname } from "node:path";
import test from "node:test";
import { promisify } from "node:util";

import {
  hostExecutableEnvironment,
  resolveHostExecutable,
} from "./host-executable.mjs";

const run = promisify(execFile);
const loaderVariables = [
  "DYLD_FALLBACK_FRAMEWORK_PATH",
  "DYLD_FALLBACK_LIBRARY_PATH",
  "DYLD_FRAMEWORK_PATH",
  "DYLD_INSERT_LIBRARIES",
  "DYLD_LIBRARY_PATH",
  "GLIBC_TUNABLES",
  "LD_AUDIT",
  "LD_DEBUG",
  "LD_LIBRARY_PATH",
  "LD_PRELOAD",
  "NIX_LD",
  "NIX_LD_LIBRARY_PATH",
];

test("host executableへloader注入用の環境変数を渡しません", () => {
  const parent = {
    HOME: "/home/tester",
    PATH: "/usr/bin",
    TMPDIR: "/tmp/tester",
    XDG_RUNTIME_DIR: "/run/user/tester",
  };
  for (const name of loaderVariables) parent[name] = `/nix/store/injected-${name}`;

  const child = hostExecutableEnvironment(parent);

  for (const name of loaderVariables) {
    assert.equal(child[name], undefined, `${name} was inherited`);
    assert.match(parent[name], /^\/nix\/store\/injected-/);
  }
  assert.deepEqual(child, {
    HOME: "/home/tester",
    PATH: "/usr/bin",
    TMPDIR: "/tmp/tester",
    XDG_RUNTIME_DIR: "/run/user/tester",
  });
});

test("host executableをabsolute pathへ解決します", async () => {
  assert.equal(await resolveHostExecutable(process.execPath), process.execPath);
  assert.equal(
    await resolveHostExecutable(basename(process.execPath), {
      PATH: dirname(process.execPath),
      PATHEXT: process.env.PATHEXT,
    }),
    process.execPath,
  );
});

test("実processにも保持対象だけを渡します", async () => {
  const environment = hostExecutableEnvironment({
    ...process.env,
    ADOCWEAVE_TEST_KEEP: "kept",
    LD_LIBRARY_PATH: "/nix/store/injected-library",
    NIX_LD: "/nix/store/injected-loader",
  });
  const { stdout } = await run(
    process.execPath,
    [
      "--input-type=module",
      "-e",
      "process.stdout.write(JSON.stringify({keep:process.env.ADOCWEAVE_TEST_KEEP,ld:process.env.LD_LIBRARY_PATH,nixLd:process.env.NIX_LD}))",
    ],
    { env: environment },
  );
  assert.deepEqual(JSON.parse(stdout), {
    keep: "kept",
  });
});
