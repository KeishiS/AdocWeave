import assert from "node:assert/strict";
import test from "node:test";

import {
  loadDependencyBoundaryInputs,
  validateDependencyBoundaries,
} from "./verify-dependency-boundaries.mjs";

test("repository dependency boundaries and exceptions are complete", () => {
  validateDependencyBoundaries(loadDependencyBoundaryInputs());
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
