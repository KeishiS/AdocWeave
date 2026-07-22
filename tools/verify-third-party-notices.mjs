import { readFileSync } from "node:fs";
import process from "node:process";

const metadataPaths = process.argv.slice(2);
if (metadataPaths.length === 0) {
  process.stderr.write("usage: node tools/verify-third-party-notices.mjs METADATA_JSON...\n");
  process.exit(2);
}

try {
  const metadataList = metadataPaths.map((path) => JSON.parse(readFileSync(path, "utf8")));
  const notices = readFileSync(new URL("../THIRD_PARTY_NOTICES.adoc", import.meta.url), "utf8");
  const packages = new Map();
  for (const metadata of metadataList) {
    const workspace = new Set(metadata.workspace_members);
    for (const entry of metadata.packages.filter((pkg) => !workspace.has(pkg.id))) packages.set(entry.id, entry);
  }
  for (const pkg of packages.values()) {
    if (!pkg.license) throw new Error(`${pkg.name} ${pkg.version} has no license metadata`);
    if (!notices.includes(`${pkg.name} ${pkg.version}`)) {
      throw new Error(`third-party notice is missing ${pkg.name} ${pkg.version}`);
    }
    if (!notices.includes(`|${pkg.license}\n`)) {
      throw new Error(`third-party notice is missing license expression: ${pkg.license}`);
    }
  }
  process.stdout.write("third-party notices verified\n");
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 1;
}
