import { readFileSync } from "node:fs";
import process from "node:process";
import { pathToFileURL } from "node:url";

const ROOT = new URL("../", import.meta.url);
export const CONTRACT_URL = new URL("release/textlint-plugin-package-contract.json", ROOT);

function fail(message) { throw new Error(`textlint plugin package contract: ${message}`); }
function exactKeys(value, keys, where) {
  if (!value || typeof value !== "object" || Array.isArray(value)) fail(`${where} must be an object`);
  const actual = Object.keys(value).sort();
  const expected = [...keys].sort();
  if (JSON.stringify(actual) !== JSON.stringify(expected)) fail(`${where} has unknown or missing fields`);
}
function string(value, where) { if (typeof value !== "string" || value.length === 0) fail(`${where} must be a non-empty string`); }
function integer(value, where) { if (!Number.isSafeInteger(value) || value <= 0) fail(`${where} must be a positive integer`); }
function safePath(value, where) {
  string(value, where);
  const parts = value.split("/");
  if (value.startsWith("/") || /^[A-Za-z]:/.test(value) || value.includes("\\") || value.includes("\0") ||
      parts.some((part) => part === "" || part === "." || part === "..") || value.normalize("NFC") !== value) {
    fail(`${where} is not a canonical relative path`);
  }
}

export function validateTextlintPluginPackageContract(contract) {
  exactKeys(contract, ["$schema", "schemaVersion", "identity", "extensions", "compatibility", "files", "wasm", "archive", "e2eMatrix", "oneShot"], "root");
  if (contract.$schema !== "./textlint-plugin-package-contract.schema.json" || contract.schemaVersion !== 1) fail("unsupported schema");
  exactKeys(contract.identity, ["packageName", "pluginName", "private"], "identity");
  string(contract.identity.packageName, "identity.packageName"); string(contract.identity.pluginName, "identity.pluginName");
  if (contract.identity.private !== true) fail("identity.private must be true");
  if (!Array.isArray(contract.extensions) || contract.extensions.length === 0 || new Set(contract.extensions).size !== contract.extensions.length) fail("extensions must be a non-empty unique array");
  for (const extension of contract.extensions) if (!/^\.[a-z0-9]+$/.test(extension)) fail(`invalid extension: ${extension}`);
  exactKeys(contract.compatibility, ["nodeEngine", "textlintVersion", "textlintTypesVersion"], "compatibility");
  for (const key of Object.keys(contract.compatibility)) string(contract.compatibility[key], `compatibility.${key}`);
  if (!Array.isArray(contract.files) || contract.files.length === 0) fail("files must be non-empty");
  const paths = new Set(); const portablePaths = new Set(); const generators = new Set();
  for (const [index, entry] of contract.files.entries()) {
    const generated = Object.hasOwn(entry ?? {}, "generator");
    exactKeys(entry, generated ? ["path", "generator"] : ["path", "source"], `files[${index}]`);
    safePath(entry.path, `files[${index}].path`);
    if (paths.has(entry.path)) fail(`duplicate file path: ${entry.path}`); paths.add(entry.path);
    const portablePath = entry.path.toLocaleLowerCase("en-US");
    if (portablePaths.has(portablePath)) fail(`portable file path collision: ${entry.path}`); portablePaths.add(portablePath);
    if (generated) {
      if (!["package-manifest", "third-party-notices", "wasm-wrapper", "wasm-binary"].includes(entry.generator)) fail(`unknown generator: ${entry.generator}`);
      if (generators.has(entry.generator)) fail(`duplicate generator: ${entry.generator}`); generators.add(entry.generator);
    } else safePath(entry.source, `files[${index}].source`);
  }
  if ([...paths].join("\n") !== [...paths].sort().join("\n")) fail("files must be sorted by path");
  exactKeys(contract.wasm, ["exportNames", "maximumMemoryBytes", "wrapperPath", "binaryPath"], "wasm");
  if (!Array.isArray(contract.wasm.exportNames) || contract.wasm.exportNames.length === 0 || contract.wasm.exportNames.some((v) => typeof v !== "string")) fail("wasm.exportNames must be non-empty strings");
  integer(contract.wasm.maximumMemoryBytes, "wasm.maximumMemoryBytes");
  if (contract.wasm.maximumMemoryBytes % 65536 !== 0) fail("wasm.maximumMemoryBytes must use WebAssembly pages");
  safePath(contract.wasm.wrapperPath, "wasm.wrapperPath"); safePath(contract.wasm.binaryPath, "wasm.binaryPath");
  if (!paths.has(contract.wasm.wrapperPath) || !paths.has(contract.wasm.binaryPath)) fail("WASM paths must be packaged files");
  for (const generator of ["package-manifest", "third-party-notices", "wasm-wrapper", "wasm-binary"]) if (!generators.has(generator)) fail(`missing generator: ${generator}`);
  exactKeys(contract.archive, ["fileCount", "maximumPackedBytes", "maximumUnpackedBytes"], "archive");
  for (const key of Object.keys(contract.archive)) integer(contract.archive[key], `archive.${key}`);
  if (contract.archive.fileCount !== contract.files.length) fail("archive.fileCount must equal files.length");
  if (!Array.isArray(contract.e2eMatrix) || contract.e2eMatrix.length === 0) fail("e2eMatrix must be non-empty");
  const matrix = new Set();
  for (const [index, entry] of contract.e2eMatrix.entries()) { exactKeys(entry, ["runner", "node"], `e2eMatrix[${index}]`); string(entry.runner, `e2eMatrix[${index}].runner`); string(entry.node, `e2eMatrix[${index}].node`); const key = `${entry.runner}\0${entry.node}`; if (matrix.has(key)) fail("e2eMatrix has a duplicate entry"); matrix.add(key); }
  exactKeys(contract.oneShot, ["rulePackage", "ruleVersion", "preset"], "oneShot");
  for (const key of Object.keys(contract.oneShot)) string(contract.oneShot[key], `oneShot.${key}`);
  return contract;
}

export function loadTextlintPluginPackageContract(path = CONTRACT_URL) {
  return validateTextlintPluginPackageContract(JSON.parse(readFileSync(path, "utf8")));
}

export function expectedManifestFiles(contract) {
  return contract.files.filter(({ path }) => path !== "package.json").map(({ path }) => path);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try { const contract = loadTextlintPluginPackageContract(process.argv[2]); process.stdout.write(`textlint plugin package contract verified: ${contract.files.length} files\n`); }
  catch (error) { process.stderr.write(`${error.message}\n`); process.exitCode = 1; }
}
