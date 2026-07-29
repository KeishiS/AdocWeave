import assert from "node:assert/strict";
import { chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";

const helper = new URL("./dependabot-alert-snapshot.sh", import.meta.url);

async function runSnapshot(mode, dependencies = '["serde"]') {
  const directory = await mkdtemp(join(tmpdir(), "adocweave-alert-snapshot-"));
  const gh = join(directory, "gh");
  const calls = join(directory, "calls");
  await writeFile(gh, `#!/usr/bin/env bash
set -euo pipefail
test "$1" = api
test "$2" = --paginate
state="\${3#*state=}"
state="\${state%%&*}"
printf '%s\\n' "$state" >> "$FAKE_GH_CALLS"
if [[ "$FAKE_GH_MODE" == match-* ]]; then
  if [[ "$state" == "\${FAKE_GH_MODE#match-}" ]]; then
    jq -cn --arg state "$state" '[{
      state: $state,
      dependency: {
        manifest_path: "Cargo.lock",
        package: {name: "serde"}
      }
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
      dependency: {
        manifest_path: "other/Cargo.lock",
        package: {name: "other"}
      }
    }]'
    printf '%s\\n' '[{"number":101,"state":"open","dependency":{"manifest_path":"other/Cargo.lock","package":{"name":"other"}}}]'
    ;;
  pages:fixed)
    printf '%s\\n' '[]'
    ;;
  pages:dismissed)
    printf '%s\\n' '[]' '[{"state":"dismissed","dependency":{"manifest_path":"Cargo.lock","package":{"name":"serde"}}}]'
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
  top-level-manifest:fixed)
    printf '%s\\n' '[{"state":"fixed","manifest_path":"Cargo.lock","dependency":{"package":{"name":"serde"}}}]'
    ;;
  top-level-manifest:*) printf '%s\\n' '[]' ;;
  invalid-dependency:dismissed)
    printf '%s\\n' '[{"state":"dismissed","dependency":{"manifest_path":"Cargo.lock","package":{"name":1}}}]'
    ;;
  invalid-dependency:*) printf '%s\\n' '[]' ;;
  mismatched-state:auto_dismissed)
    printf '%s\\n' '[{"state":"dismissed","dependency":{"manifest_path":"Cargo.lock","package":{"name":"serde"}}}]'
    ;;
  mismatched-state:*) printf '%s\\n' '[]' ;;
  failure:auto_dismissed) exit 22 ;;
  failure:*) printf '%s\\n' '[]' ;;
  *) exit 64 ;;
esac
`);
  await chmod(gh, 0o755);
  try {
    const result = spawnSync(
      "bash",
      [helper.pathname, "KeishiS/adocweave", dependencies, '["Cargo.toml","Cargo.lock"]'],
      {
        encoding: "utf8",
        env: {
          ...process.env,
          FAKE_GH_MODE: mode,
          FAKE_GH_CALLS: calls,
          PATH: `${directory}${delimiter}${process.env.PATH}`,
        },
      },
    );
    const callOrder = await readFile(calls, "utf8")
      .then((value) => value.trim().split("\n"))
      .catch(() => []);
    return { ...result, callOrder };
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
}

test("alert snapshot reads every page and records every state", async () => {
  const result = await runSnapshot("pages");
  assert.equal(result.status, 0, result.stderr);
  const snapshot = JSON.parse(result.stdout);
  assert.deepEqual(result.callOrder, ["fixed", "dismissed", "auto_dismissed", "open"]);
  assert.match(snapshot.observedAt, /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/);
  delete snapshot.observedAt;
  assert.deepEqual(snapshot, {
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
  const snapshot = JSON.parse(result.stdout);
  assert.match(snapshot.observedAt, /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/);
  delete snapshot.observedAt;
  assert.deepEqual(snapshot, {
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
  "top-level-manifest",
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
