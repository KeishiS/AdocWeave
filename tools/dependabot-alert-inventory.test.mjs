import assert from "node:assert/strict";
import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";

const helper = new URL("./dependabot-alert-inventory.sh", import.meta.url);

async function runInventory(mode) {
  const directory = await mkdtemp(join(tmpdir(), "adocweave-alert-inventory-"));
  const gh = join(directory, "gh");
  await writeFile(gh, `#!/usr/bin/env bash
set -euo pipefail
test "$1" = api
test "$2" = --paginate
test "$3" = "repos/KeishiS/adocweave/dependabot/alerts?state=open&per_page=100"
case "$FAKE_GH_MODE" in
  pages) printf '%s\\n' '[{"number":1},{"number":2}]' '[{"number":3}]' ;;
  empty) printf '%s\\n' '[]' ;;
  no-output) ;;
  malformed) printf '%s\\n' '{"message":"unexpected"}' ;;
  failure) exit 22 ;;
  *) exit 64 ;;
esac
`);
  await chmod(gh, 0o755);
  try {
    return spawnSync("bash", [helper.pathname, "KeishiS/adocweave"], {
      encoding: "utf8",
      env: {
        ...process.env,
        FAKE_GH_MODE: mode,
        PATH: `${directory}${delimiter}${process.env.PATH}`,
      },
    });
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
}

test("alert inventory counts every paginated response", async () => {
  const result = await runInventory("pages");
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout.trim(), "3");
});

test("alert inventory accepts an empty alert array as zero", async () => {
  const result = await runInventory("empty");
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout.trim(), "0");
});

test("alert inventory rejects a successful API call without output", async () => {
  const result = await runInventory("no-output");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Dependabot alert response must contain one or more arrays/);
});

test("alert inventory rejects a malformed response", async () => {
  const result = await runInventory("malformed");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Dependabot alert response must contain one or more arrays/);
});

test("alert inventory propagates an API failure", async () => {
  const result = await runInventory("failure");
  assert.notEqual(result.status, 0);
});
