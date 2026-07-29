import assert from "node:assert/strict";
import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";

const helper = new URL("./dependabot-alert-snapshot.sh", import.meta.url);

async function runSnapshot(mode, dependencies = '["serde"]') {
  const directory = await mkdtemp(join(tmpdir(), "adocweave-alert-snapshot-"));
  const gh = join(directory, "gh");
  await writeFile(gh, `#!/usr/bin/env bash
set -euo pipefail
test "$1" = api
test "$2" = --paginate
state="\${3#*state=}"
state="\${state%%&*}"
if [[ "$FAKE_GH_MODE" == match-* ]]; then
  if [[ "$state" == "\${FAKE_GH_MODE#match-}" ]]; then
    jq -cn --arg state "$state" '[{
      state: $state,
      manifest_path: "Cargo.lock",
      dependency: {package: {name: "serde"}}
    }]'
  else
    printf '%s\\n' '[]'
  fi
  exit
fi
case "$FAKE_GH_MODE:$state" in
  pages:open)
    jq -cn '[range(0;100) | {
      number: .,
      state: "open",
      manifest_path: "other/Cargo.lock",
      dependency: {package: {name: "other"}}
    }]'
    printf '%s\\n' '[{"number":101,"state":"open","manifest_path":"other/Cargo.lock","dependency":{"package":{"name":"other"}}}]'
    ;;
  pages:fixed)
    printf '%s\\n' '[]'
    ;;
  pages:dismissed)
    printf '%s\\n' '[]' '[{"state":"dismissed","manifest_path":"Cargo.lock","dependency":{"package":{"name":"serde"}}}]'
    ;;
  pages:auto_dismissed)
    printf '%s\\n' '[]'
    ;;
  empty:*) printf '%s\\n' '[]' ;;
  no-output:fixed) ;;
  no-output:*) printf '%s\\n' '[]' ;;
  malformed:dismissed) printf '%s\\n' '{"message":"unexpected"}' ;;
  malformed:*) printf '%s\\n' '[]' ;;
  invalid-object:open) printf '%s\\n' '[null]' ;;
  invalid-object:*) printf '%s\\n' '[]' ;;
  missing-manifest:fixed)
    printf '%s\\n' '[{"state":"fixed","dependency":{"package":{"name":"serde"}}}]'
    ;;
  missing-manifest:*) printf '%s\\n' '[]' ;;
  invalid-dependency:dismissed)
    printf '%s\\n' '[{"state":"dismissed","manifest_path":"Cargo.lock","dependency":{"package":{"name":1}}}]'
    ;;
  invalid-dependency:*) printf '%s\\n' '[]' ;;
  mismatched-state:auto_dismissed)
    printf '%s\\n' '[{"state":"dismissed","manifest_path":"Cargo.lock","dependency":{"package":{"name":"serde"}}}]'
    ;;
  mismatched-state:*) printf '%s\\n' '[]' ;;
  failure:auto_dismissed) exit 22 ;;
  failure:*) printf '%s\\n' '[]' ;;
  *) exit 64 ;;
esac
`);
  await chmod(gh, 0o755);
  try {
    return spawnSync(
      "bash",
      [helper.pathname, "KeishiS/adocweave", dependencies, '["Cargo.toml","Cargo.lock"]'],
      {
        encoding: "utf8",
        env: {
          ...process.env,
          FAKE_GH_MODE: mode,
          PATH: `${directory}${delimiter}${process.env.PATH}`,
        },
      },
    );
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
}

test("alert snapshot reads every page and records every state", async () => {
  const result = await runSnapshot("pages");
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), {
    lookupCompleted: true,
    openCount: 101,
    securityUpdate: true,
    stateCounts: { open: 101, fixed: 0, dismissed: 1, autoDismissed: 0 },
  });
});

for (const state of ["open", "fixed", "dismissed", "auto_dismissed"]) {
  test(`alert snapshot detects a matching ${state} alert`, async () => {
    const result = await runSnapshot(`match-${state}`);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(JSON.parse(result.stdout).securityUpdate, true);
  });
}

test("alert snapshot may conservatively match every dependency in changed manifests", async () => {
  const result = await runSnapshot("pages", "[]");
  assert.equal(result.status, 0, result.stderr);
  assert.equal(JSON.parse(result.stdout).securityUpdate, true);
});

test("alert snapshot reports a completed empty lookup", async () => {
  const result = await runSnapshot("empty");
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), {
    lookupCompleted: true,
    openCount: 0,
    securityUpdate: false,
    stateCounts: { open: 0, fixed: 0, dismissed: 0, autoDismissed: 0 },
  });
});

for (const mode of ["no-output", "malformed", "failure"]) {
  test(`alert snapshot fails closed for ${mode}`, async () => {
    const result = await runSnapshot(mode);
    assert.notEqual(result.status, 0);
  });
}

for (const mode of [
  "invalid-object",
  "missing-manifest",
  "invalid-dependency",
  "mismatched-state",
]) {
  test(`alert snapshot rejects an invalid alert entry for ${mode}`, async () => {
    const result = await runSnapshot(mode);
    assert.notEqual(result.status, 0);
  });
}

for (const dependencies of ["", "null", '[""]', '["serde",1]']) {
  test(`alert snapshot rejects invalid dependency input ${dependencies}`, async () => {
    const result = await runSnapshot("empty", dependencies);
    assert.notEqual(result.status, 0);
  });
}
