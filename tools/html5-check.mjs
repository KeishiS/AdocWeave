import { spawnSync } from "node:child_process";
import {
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, isAbsolute, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const manifestPath = resolve(root, "fixtures/html/validation.json");
const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
const outputDirectory = resolve(root, "target/html5");
const cli = resolve(root, "target/debug/adocweave");
const conformanceNative = resolve(
  root,
  "target/debug/adocweave-conformance-native",
);
const conformanceDirectory = resolve(root, "fixtures/conformance");
const validator = process.env.ADOCWEAVE_HTML_VALIDATOR;
const conformanceManifest = JSON.parse(
  readFileSync(resolve(conformanceDirectory, "cases.json"), "utf8"),
);
const release = JSON.parse(
  readFileSync(resolve(root, "release-manifest.json"), "utf8"),
);

function fail(message) {
  throw new Error(message);
}

function repositoryPath(path, description) {
  if (typeof path !== "string" || path.length === 0) {
    fail(`${description} must be a non-empty repository-relative path`);
  }
  const absolute = resolve(root, path);
  const fromRoot = relative(root, absolute);
  if (fromRoot.startsWith("..") || isAbsolute(fromRoot)) {
    fail(`${description} escapes the repository: ${path}`);
  }
  return absolute;
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: root,
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
    ...options,
  });
  if (result.error) {
    fail(`${command} could not start: ${result.error.message}`);
  }
  return result;
}

function requireSuccess(result, description) {
  if (result.status !== 0) {
    fail(
      `${description} failed with status ${result.status}\n${result.stderr}${result.stdout}`,
    );
  }
}

function validateManifest() {
  if (manifest.schemaVersion !== 1) {
    fail(`unsupported HTML5 manifest schema: ${manifest.schemaVersion}`);
  }
  if (
    !manifest.validator ||
    manifest.validator.package !== "validator-nu" ||
    typeof manifest.validator.version !== "string" ||
    !Array.isArray(manifest.validator.options)
  ) {
    fail("invalid validator configuration");
  }
  if (!validator) {
    fail(
      "ADOCWEAVE_HTML_VALIDATOR is unset; run this check through `cargo make html5-check`",
    );
  }
  const names = new Set();
  for (const entry of manifest.cases ?? []) {
    if (
      typeof entry.name !== "string" ||
      !/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(entry.name) ||
      names.has(entry.name)
    ) {
      fail(`missing or duplicate HTML5 case name: ${entry.name}`);
    }
    names.add(entry.name);
    if (
      ![
        "cli-fragment",
        "cli-complete",
        "conformance-fragment",
        "conformance-complete",
      ].includes(entry.kind)
    ) {
      fail(`unsupported HTML5 case kind: ${entry.kind}`);
    }
    if (entry.kind.startsWith("cli-")) {
      repositoryPath(entry.source, `source for ${entry.name}`);
      if (
        entry.args !== undefined &&
        (!Array.isArray(entry.args) ||
          !entry.args.every((argument) => typeof argument === "string"))
      ) {
        fail(`args for ${entry.name} must be strings`);
      }
    } else if (typeof entry.case !== "string" || entry.case.length === 0) {
      fail(`missing conformance case for ${entry.name}`);
    }
  }
  if (!Array.isArray(manifest.negativeFixtures)) {
    fail("negativeFixtures must be an array");
  }
  for (const fixture of manifest.negativeFixtures) {
    repositoryPath(fixture.path, "negative fixture");
    if (
      fixture.type !== "error" ||
      typeof fixture.messagePattern !== "string" ||
      fixture.messagePattern.length === 0
    ) {
      fail(`invalid negative fixture expectation: ${fixture.path}`);
    }
  }
}

function validatorResult(paths) {
  const result = run(validator, [
    ...manifest.validator.options,
    ...paths.map((path) => repositoryPath(path, "validator input")),
  ]);
  let report;
  try {
    report = JSON.parse(result.stdout.trim() || result.stderr.trim());
  } catch {
    fail(`validator emitted invalid JSON\n${result.stderr}${result.stdout}`);
  }
  if (report.version !== manifest.validator.version) {
    fail(
      `validator version mismatch: expected ${manifest.validator.version}, got ${report.version}`,
    );
  }
  return { result, report };
}

function reportMessages(messages) {
  for (const message of messages) {
    const path = message.url?.startsWith("file:")
      ? relative(root, fileURLToPath(message.url))
      : (message.url ?? "<validator>");
    const line = message.lastLine ?? 0;
    const column = message.firstColumn ?? message.lastColumn ?? 0;
    const rule = [message.type, message.subType].filter(Boolean).join("/");
    process.stderr.write(
      `${path}:${line}:${column}: ${rule}: ${message.message}\n`,
    );
  }
}

function assertValidatorVersion() {
  const result = run(validator, ["--version"]);
  requireSuccess(result, "validator version query");
  const actual = result.stdout.trim();
  if (actual !== manifest.validator.version) {
    fail(
      `validator version mismatch: expected ${manifest.validator.version}, got ${actual}`,
    );
  }
}

function conformanceRequest(name, expectedMode) {
  const entry = conformanceManifest.cases.find(
    (candidate) => candidate.name === name,
  );
  if (!entry) {
    fail(`unknown conformance case: ${name}`);
  }
  const actualMode = entry.options?.documentMode ?? "fragment";
  if (actualMode !== expectedMode) {
    fail(
      `conformance case ${name} uses ${actualMode} output, expected ${expectedMode}`,
    );
  }
  const source = entry.sourceFile
    ? readFileSync(resolve(conformanceDirectory, entry.sourceFile), "utf8")
    : entry.source;
  return {
    packageVersion: release.packageVersion,
    sourceId: `html5:${name}`,
    version: 1,
    generation: 1,
    source,
    products: {
      syntax: false,
      canonicalAst: false,
      html: true,
      attributeOccurrences: false,
      resourceQueries: false,
      diagnostics: true,
      symbols: false,
      projection: false,
    },
    renderInputs: entry.renderInputs ?? {},
    options: entry.options ?? {},
  };
}

function renderCase(entry) {
  if (entry.kind === "cli-fragment" || entry.kind === "cli-complete") {
    const args = ["convert"];
    if (entry.kind === "cli-complete") {
      args.push("--complete");
    }
    args.push(...(entry.args ?? []));
    args.push(repositoryPath(entry.source, `source for ${entry.name}`));
    const result = run(cli, args);
    requireSuccess(result, `rendering ${entry.name}`);
    return result.stdout;
  }

  const result = run(conformanceNative, [], {
    input: `${JSON.stringify(
      conformanceRequest(
        entry.case,
        entry.kind === "conformance-complete" ? "complete" : "fragment",
      ),
    )}\n`,
  });
  requireSuccess(result, `rendering ${entry.name}`);
  const response = JSON.parse(result.stdout);
  if (!response.ok || typeof response.value?.html !== "string") {
    fail(`conformance case ${entry.case} did not return HTML`);
  }
  return response.value.html;
}

function generateDocuments() {
  const templatePath = repositoryPath(manifest.template.path, "HTML5 template");
  const template = readFileSync(templatePath, "utf8");
  const marker = manifest.template.marker;
  if (template.split(marker).length !== 2) {
    fail(`HTML5 template must contain exactly one marker: ${marker}`);
  }

  rmSync(outputDirectory, { recursive: true, force: true });
  mkdirSync(outputDirectory, { recursive: true });
  const generated = [];
  generated.push(templatePath);
  const templateProbe = resolve(outputDirectory, "template.html");
  writeFileSync(templateProbe, template.replace(marker, "<p>template probe</p>"));
  generated.push(templateProbe);

  for (const entry of manifest.cases) {
    const fragment = renderCase(entry);
    const document =
      entry.kind === "cli-complete" || entry.kind === "conformance-complete"
        ? fragment
        : template.replace(marker, fragment);
    const output = resolve(outputDirectory, `${entry.name}.html`);
    writeFileSync(output, document);
    generated.push(output);
  }
  return generated;
}

function assertNegativeFixturesFail() {
  for (const fixture of manifest.negativeFixtures) {
    const { result, report } = validatorResult([fixture.path]);
    if (result.status === 0 || report.messages.length === 0) {
      fail(`negative HTML5 fixture unexpectedly passed: ${fixture.path}`);
    }
    const expected = new RegExp(fixture.messagePattern, "u");
    if (
      !report.messages.some(
        (message) =>
          message.type === fixture.type && expected.test(message.message),
      )
    ) {
      reportMessages(report.messages);
      fail(
        `negative HTML5 fixture did not report its expected rule: ${fixture.path}`,
      );
    }
  }
}

try {
  validateManifest();
  assertValidatorVersion();
  assertNegativeFixturesFail();
  const generated = generateDocuments();
  const { result, report } = validatorResult(generated);
  if (result.status !== 0 || report.messages.length !== 0) {
    reportMessages(report.messages);
    fail(`HTML5 validation failed for ${report.messages.length} message(s)`);
  }
  process.stdout.write(
    `HTML5 validation passed: ${generated.length} documents, validator ${manifest.validator.version}\n`,
  );
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 1;
}
