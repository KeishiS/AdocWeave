import assert from "node:assert/strict";
import test from "node:test";

import {
  loadDependencyBoundaryInputs,
  validateDependabotConfig,
  validateDependencyBoundaries,
} from "./verify-dependency-boundaries.mjs";

test("repository dependency boundaries and exceptions are complete", () => {
  const inputs = loadDependencyBoundaryInputs();
  validateDependencyBoundaries(inputs);
  validateDependabotConfig(inputs.dependabot);
});

test("Dependabot keeps every managed boundary separate and bounded", () => {
  const { dependabot } = loadDependencyBoundaryInputs();
  assert.throws(() => validateDependabotConfig({
    ...dependabot,
    updates: dependabot.updates.filter((entry) => entry.directory !== "/fuzz"),
  }), /incomplete/);
  assert.throws(() => validateDependabotConfig({
    ...dependabot,
    updates: dependabot.updates.map((entry) => entry.directory === "/" && entry["package-ecosystem"] === "cargo"
      ? { ...entry, "open-pull-requests-limit": 3 }
      : entry),
  }), /limit open pull requests/);
});

test("runtime frontend dependencies require an approved lockfile boundary", () => {
  const inputs = loadDependencyBoundaryInputs();
  assert.throws(() => validateDependencyBoundaries({
    ...inputs,
    manifest: (path) => path === "web-worker/package.json" ? { dependencies: { unsafe: "1.0.0" } } : inputs.manifest(path),
  }), /without an approved lockfile/);
});

test("exceptions require a supported rule, owner, issue, and future expiry", () => {
  const inputs = loadDependencyBoundaryInputs();
  const base = {
    id: "example",
    kind: "rustsec",
    value: "RUSTSEC-2026-0001",
    owner: "repository-owner",
    reason: "affected API is unreachable",
    expires: "2026-01-01",
    issue: "https://github.com/KeishiS/AdocWeave/issues/999",
  };
  assert.throws(() => validateDependencyBoundaries({
    ...inputs,
    exceptions: { version: 1, exceptions: [base] },
  }), /expired/);
  assert.throws(() => validateDependencyBoundaries({
    ...inputs,
    exceptions: { version: 1, exceptions: [{ ...base, expires: "2099-01-01", owner: "" }] },
  }), /missing owner/);
});
