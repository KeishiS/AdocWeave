import { existsSync, readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const ROOT = new URL("../", import.meta.url);
const readJson = (path) => JSON.parse(readFileSync(new URL(path, ROOT), "utf8"));

function fail(message) {
  throw new Error(message);
}

export function validateDependencyBoundaries({ inventory, exceptions, exists, manifest, today }) {
  if (inventory.version !== 1 || !Array.isArray(inventory.boundaries)) fail("invalid dependency boundary inventory");
  if (exceptions.version !== 1 || !Array.isArray(exceptions.exceptions)) fail("invalid dependency exception registry");

  const ids = new Set();
  for (const boundary of inventory.boundaries) {
    if (!boundary.id || ids.has(boundary.id)) fail(`duplicate or empty dependency boundary: ${boundary.id}`);
    ids.add(boundary.id);
    for (const field of ["manifest", "lockfile"]) {
      if (boundary[field] && !exists(boundary[field])) {
        fail(`${boundary.id} references missing ${field}: ${boundary[field]}`);
      }
    }
    if (boundary.kind === "npm") {
      const runtime = Object.keys(manifest(boundary.manifest).dependencies ?? {});
      if (runtime.length !== 0 && (!boundary.runtimeDependenciesAllowed || !boundary.lockfile)) {
        fail(`${boundary.id} has runtime dependencies without an approved lockfile boundary`);
      }
    }
  }

  const required = new Set(["native-and-wasm", "zed-extension", "browser-worker", "ci-and-development-tools", "fuzz-harness"]);
  for (const id of required) if (!ids.has(id)) fail(`missing dependency boundary: ${id}`);

  for (const exception of exceptions.exceptions) {
    for (const field of ["id", "kind", "value", "owner", "reason", "expires", "issue"]) {
      if (typeof exception[field] !== "string" || exception[field].trim() === "") {
        fail(`dependency exception is missing ${field}`);
      }
    }
    if (exception.kind !== "rustsec" || !/^RUSTSEC-\d{4}-\d{4}$/.test(exception.value)) {
      fail(`dependency exception ${exception.id} has an unsupported kind or value`);
    }
    if (!/^\d{4}-\d{2}-\d{2}$/.test(exception.expires) || exception.expires < today) {
      fail(`dependency exception ${exception.id} is expired or has an invalid expiry`);
    }
    if (!/^https:\/\/github\.com\/KeishiS\/AdocWeave\/issues\/\d+$/.test(exception.issue)) {
      fail(`dependency exception ${exception.id} must reference a repository issue`);
    }
  }
}

export function validateDependabotConfig(config) {
  if (config?.version !== 2 || !Array.isArray(config.updates)) fail("invalid Dependabot configuration");
  const expected = new Set([
    "cargo\u0000/",
    "cargo\u0000/editors/zed",
    "cargo\u0000/fuzz",
    "github-actions\u0000/",
  ]);
  const actual = new Set();
  for (const update of config.updates) {
    const ecosystem = update?.["package-ecosystem"];
    const directory = update?.directory;
    const key = `${ecosystem}\u0000${directory}`;
    if (!expected.has(key) || actual.has(key)) fail(`unexpected or duplicate Dependabot update boundary: ${ecosystem} ${directory}`);
    actual.add(key);
    if (update?.schedule?.interval !== "weekly" || update.schedule?.day !== "monday") {
      fail(`Dependabot update boundary must run weekly on Monday: ${ecosystem} ${directory}`);
    }
    if (update?.["open-pull-requests-limit"] !== 2) {
      fail(`Dependabot update boundary must limit open pull requests: ${ecosystem} ${directory}`);
    }
  }
  if (actual.size !== expected.size || [...expected].some((key) => !actual.has(key))) {
    fail("Dependabot update boundaries are incomplete");
  }
}

function readYaml(path) {
  const result = spawnSync("yq", ["-o=json", ".", fileURLToPath(new URL(path, ROOT))], { encoding: "utf8" });
  if (result.status !== 0) fail(`cannot parse ${path}: ${result.stderr.trim() || result.error?.message}`);
  return JSON.parse(result.stdout);
}

export function loadDependencyBoundaryInputs() {
  return {
    inventory: readJson("security/dependency-boundaries.json"),
    exceptions: readJson("security/dependency-exceptions.json"),
    exists: (path) => existsSync(new URL(path, ROOT)),
    manifest: readJson,
    dependabot: readYaml(".github/dependabot.yml"),
    today: new Date().toISOString().slice(0, 10),
  };
}

export function main() {
  const inputs = loadDependencyBoundaryInputs();
  validateDependencyBoundaries(inputs);
  validateDependabotConfig(inputs.dependabot);
  if (process.argv.includes("--audit-ignores")) {
    for (const exception of inputs.exceptions.exceptions) process.stdout.write(`${exception.value}\n`);
  } else {
    process.stdout.write("dependency boundaries and exceptions verified\n");
  }
}

if (process.argv[1] && import.meta.url === new URL(process.argv[1], "file:").href) {
  try {
    main();
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
