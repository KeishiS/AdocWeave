import process from "node:process";
import { pathToFileURL } from "node:url";
import { resolve } from "node:path";

import {
  runTextlintPluginConsumerE2E,
} from "./textlint-plugin-consumer-e2e.mjs";

export * from "./textlint-plugin-consumer-e2e.mjs";
export const runTextlintPluginReleaseSmoke = runTextlintPluginConsumerE2E;

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  const [archive] = process.argv.slice(2);
  if (!archive) {
    process.stderr.write("usage: node tools/textlint-plugin-release-smoke.mjs PACKAGE_TGZ\n");
    process.exit(2);
  }
  await runTextlintPluginConsumerE2E(archive);
  process.stdout.write("textlint plugin fixed consumer release smoke passed\n");
}
