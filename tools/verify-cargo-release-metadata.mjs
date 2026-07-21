import { readFileSync } from "node:fs";
import process from "node:process";

const [path] = process.argv.slice(2);
if (!path) {
  process.stderr.write("usage: node tools/verify-cargo-release-metadata.mjs METADATA_JSON\n");
  process.exit(2);
}

try {
  const metadata = JSON.parse(readFileSync(path, "utf8"));
  const expectedNames = ["adocweave", "adocweave-cli", "adocweave-host", "adocweave-lsp", "adocweave-wasm"];
  const packages = metadata.packages.filter((pkg) => expectedNames.includes(pkg.name));
  if (packages.length !== expectedNames.length) throw new Error("cargo metadata is missing a workspace package");
  for (const pkg of packages) {
    if (pkg.version !== "0.1.0") throw new Error(`${pkg.name}: cargo metadata version mismatch`);
    if (pkg.repository !== "https://github.com/KeishiS/AdocWeave" || pkg.homepage !== pkg.repository) {
      throw new Error(`${pkg.name}: cargo metadata repository mismatch`);
    }
    if (pkg.license !== "MIT OR Apache-2.0") throw new Error(`${pkg.name}: cargo metadata license mismatch`);
    if (!Array.isArray(pkg.publish) || pkg.publish.length !== 0) {
      throw new Error(`${pkg.name}: Cargo registry publication must be disabled`);
    }
  }
  process.stdout.write("cargo release metadata verified\n");
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 1;
}
