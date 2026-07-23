import { readFileSync } from "node:fs";
import process from "node:process";

import releaseManifest from "../release-manifest.json" with { type: "json" };

const [path, zedPath] = process.argv.slice(2);
if (!path || !zedPath) {
  process.stderr.write("usage: node tools/verify-cargo-release-metadata.mjs ROOT_METADATA_JSON ZED_METADATA_JSON\n");
  process.exit(2);
}

try {
  const metadata = JSON.parse(readFileSync(path, "utf8"));
  const zedMetadata = JSON.parse(readFileSync(zedPath, "utf8"));
  const expectedNames = ["adocweave", "adocweave-cli", "adocweave-host", "adocweave-lsp", "adocweave-wasm"];
  const packages = metadata.packages.filter((pkg) => expectedNames.includes(pkg.name));
  if (packages.length !== expectedNames.length) throw new Error("cargo metadata is missing a workspace package");
  for (const pkg of packages) {
    if (pkg.version !== releaseManifest.packageVersion) throw new Error(`${pkg.name}: cargo metadata version mismatch`);
    if (pkg.repository !== "https://github.com/KeishiS/AdocWeave" || pkg.homepage !== pkg.repository) {
      throw new Error(`${pkg.name}: cargo metadata repository mismatch`);
    }
    if (pkg.license !== "MIT OR Apache-2.0") throw new Error(`${pkg.name}: cargo metadata license mismatch`);
    if (!Array.isArray(pkg.publish) || pkg.publish.length !== 0) {
      throw new Error(`${pkg.name}: Cargo registry publication must be disabled`);
    }
    if (pkg.rust_version !== releaseManifest.rustVersion) {
      throw new Error(`${pkg.name}: cargo metadata Rust version mismatch`);
    }
  }
  const zed = zedMetadata.packages.find((pkg) => pkg.name === "adocweave-zed");
  if (!zed) throw new Error("cargo metadata is missing the Zed package");
  if (zed.rust_version !== releaseManifest.rustVersion) {
    throw new Error("adocweave-zed: cargo metadata Rust version mismatch");
  }
  process.stdout.write("cargo release metadata verified\n");
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 1;
}
