import { existsSync, readFileSync } from "node:fs";
import process from "node:process";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync } from "node:child_process";

const [mode] = process.argv.slice(2);
if (!["installation", "smoke"].includes(mode)) {
  process.stderr.write("usage: node tools/local-native-check.mjs installation|smoke\n");
  process.exit(2);
}

const distributionPlan = JSON.parse(
  readFileSync(new URL("../release/distribution-plan.json", import.meta.url), "utf8"),
);
const platform = distributionPlan.targets.find(
  ({ architecture, os }) => architecture === process.arch && os === process.platform,
);
if (!platform) {
  throw new Error(`local native checks do not support ${process.platform}/${process.arch}`);
}

const candidate = resolve(process.env.NATIVE_ARTIFACT_DIR ?? "target/distrib");
if (!existsSync(candidate)) {
  throw new Error(`native artifact directory does not exist: ${candidate}`);
}

const script = mode === "smoke" ? "native-release-smoke.mjs" : "release-installation-e2e.mjs";
const args = [fileURLToPath(new URL(script, import.meta.url)), candidate, platform.triple];
if (mode === "installation" && process.env.NATIVE_MANIFEST) {
  args.push(resolve(process.env.NATIVE_MANIFEST));
}
execFileSync(process.execPath, args, { stdio: "inherit" });
