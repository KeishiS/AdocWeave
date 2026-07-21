import { readFileSync } from "node:fs";
import process from "node:process";

const [metadataPath] = process.argv.slice(2);
if (!metadataPath) {
  process.stderr.write("usage: node tools/verify-third-party-notices.mjs METADATA_JSON\n");
  process.exit(2);
}

try {
  const metadata = JSON.parse(readFileSync(metadataPath, "utf8"));
  const notices = readFileSync(new URL("../THIRD_PARTY_NOTICES.adoc", import.meta.url), "utf8");
  const workspace = new Set(metadata.workspace_members);
  for (const pkg of metadata.packages.filter((entry) => !workspace.has(entry.id))) {
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
