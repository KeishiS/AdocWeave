import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import {
  copyFile,
  lstat,
  mkdir,
  mkdtemp,
  readFile,
  readlink,
  realpath,
  rm,
  symlink,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

const DEFAULT_ROOT = fileURLToPath(new URL("../", import.meta.url));

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  await verifyTextlintPluginReproducibility();
}

export async function verifyTextlintPluginReproducibility({
  repositoryRoot = DEFAULT_ROOT,
  prepareSource = copyTrackedSource,
  buildPackage = buildTextlintPluginPackage,
  verifyPackage = verifyTextlintPluginPackage,
} = {}) {
  const root = await realpath(repositoryRoot);
  const scratch = await mkdtemp(join(tmpdir(), "adocweave-textlint-reproducibility-"));
  try {
    const builds = [];
    for (const name of ["first", "second"]) {
      const buildRoot = join(scratch, name);
      const sourceDirectory = join(buildRoot, "source");
      const outputDirectory = join(buildRoot, "distrib");
      const cargoTargetDirectory = join(buildRoot, "cargo-target");
      const npmCacheDirectory = join(buildRoot, "npm-cache");
      const wasmOutputDirectory = join(buildRoot, "wasm-output");
      await prepareSource(root, sourceDirectory);
      await buildPackage({
        cargoTargetDirectory,
        npmCacheDirectory,
        outputDirectory,
        sourceDirectory,
        wasmOutputDirectory,
      });
      const manifest = JSON.parse(
        await readFile(join(sourceDirectory, "release-manifest.json"), "utf8"),
      );
      const archive = join(
        outputDirectory,
        `adocweave-textlint-plugin-asciidoc-${manifest.packageVersion}.tgz`,
      );
      const bytes = await readFile(archive);
      await verifyPackage({ archive, sourceDirectory });
      builds.push({ archive, bytes, hash: sha256(bytes) });
    }

    if (!builds[0].bytes.equals(builds[1].bytes)) {
      throw new Error(
        `clean textlint plugin builds differ: ${builds[0].hash} != ${builds[1].hash}`,
      );
    }
    process.stdout.write(`textlint plugin clean build is reproducible: ${builds[0].hash}\n`);
    return builds[0].hash;
  } finally {
    await rm(scratch, { force: true, maxRetries: 5, recursive: true, retryDelay: 100 });
  }
}

async function copyTrackedSource(repositoryRoot, destination) {
  await mkdir(destination, { recursive: true });
  const result = await runProcess("git", ["ls-files", "-z"], { cwd: repositoryRoot });
  if (result.code !== 0) {
    throw new Error(`git ls-files failed:\n${result.stderr}`);
  }
  for (const relativePath of result.stdout.split("\0").filter(Boolean)) {
    const source = join(repositoryRoot, relativePath);
    const target = join(destination, relativePath);
    const metadata = await lstat(source);
    await mkdir(dirname(target), { recursive: true });
    if (metadata.isSymbolicLink()) {
      await symlink(await readlink(source), target);
    } else if (metadata.isFile()) {
      await copyFile(source, target);
    } else {
      throw new Error(`tracked path has an unsupported type: ${relativePath}`);
    }
  }
}

async function buildTextlintPluginPackage({
  cargoTargetDirectory,
  npmCacheDirectory,
  outputDirectory,
  sourceDirectory,
  wasmOutputDirectory,
}) {
  const result = await runProcess("bash", ["tools/package-textlint-plugin-release.sh"], {
    cwd: sourceDirectory,
    env: {
      ...process.env,
      ADOCWEAVE_SOURCE_ROOT: sourceDirectory,
      ADOCWEAVE_TEXTLINT_PLUGIN_CARGO_TARGET_DIRECTORY: cargoTargetDirectory,
      ADOCWEAVE_TEXTLINT_PLUGIN_NPM_CACHE: npmCacheDirectory,
      ADOCWEAVE_TEXTLINT_PLUGIN_OUTPUT_DIRECTORY: outputDirectory,
      ADOCWEAVE_TEXTLINT_PLUGIN_WASM_OUTPUT_DIRECTORY: wasmOutputDirectory,
    },
  });
  if (result.code !== 0) {
    throw new Error(
      `textlint plugin build failed with ${result.code}\nstdout:\n${result.stdout}\nstderr:\n${result.stderr}`,
    );
  }
}

async function verifyTextlintPluginPackage({ archive, sourceDirectory }) {
  const result = await runProcess(
    process.execPath,
    ["tools/verify-textlint-plugin-package.mjs", archive],
    { cwd: sourceDirectory },
  );
  if (result.code !== 0) {
    throw new Error(
      `independent textlint package verification failed with ${result.code}\n` +
      `stdout:\n${result.stdout}\nstderr:\n${result.stderr}`,
    );
  }
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function runProcess(command, arguments_, { cwd, env = process.env } = {}) {
  return new Promise((resolveProcess, rejectProcess) => {
    const child = spawn(command, arguments_, { cwd, env, stdio: ["ignore", "pipe", "pipe"] });
    const stdout = [];
    const stderr = [];
    child.stdout.on("data", (chunk) => stdout.push(chunk));
    child.stderr.on("data", (chunk) => stderr.push(chunk));
    child.once("error", rejectProcess);
    child.once("close", (code) => resolveProcess({
      code: code ?? 128,
      stderr: Buffer.concat(stderr).toString("utf8"),
      stdout: Buffer.concat(stdout).toString("utf8"),
    }));
  });
}
