import process from "node:process";
import { resolve } from "node:path";

import {
  installLatestCompatibleConsumer,
  runTextlintPluginConsumerE2E,
} from "./textlint-plugin-consumer-e2e.mjs";

const [archive] = process.argv.slice(2);
if (!archive) {
  process.stderr.write("usage: node tools/textlint-plugin-compatibility-probe.mjs PACKAGE_TGZ\n");
  process.exit(2);
}

await runTextlintPluginConsumerE2E(resolve(archive), {
  installPackage: installLatestCompatibleConsumer,
});
process.stdout.write("textlint plugin latest dependency compatibility probe passed\n");
