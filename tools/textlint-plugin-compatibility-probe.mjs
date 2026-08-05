import process from "node:process";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

import {
  installLatestCompatibleConsumer,
  runTextlintPluginConsumerE2E,
} from "./textlint-plugin-consumer-e2e.mjs";
import { loadTextlintPluginPackageContract } from "./textlint-plugin-package-contract.mjs";

export async function runTextlintPluginCompatibilityProbe(
  archive,
  {
    contract = loadTextlintPluginPackageContract(),
    runConsumer = runTextlintPluginConsumerE2E,
  } = {},
) {
  await runConsumer(resolve(archive), {
    contract,
    installPackage: installLatestCompatibleConsumer,
  });
  return {
    packageName: contract.identity.packageName,
    textlintVersion: contract.compatibility.textlintVersion,
  };
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  const [archive] = process.argv.slice(2);
  if (!archive) {
    process.stderr.write("usage: node tools/textlint-plugin-compatibility-probe.mjs PACKAGE_TGZ\n");
    process.exit(2);
  } else {
    const result = await runTextlintPluginCompatibilityProbe(archive);
    process.stdout.write(
      `textlint plugin latest dependency compatibility probe passed: ` +
      `${result.packageName} with textlint@${result.textlintVersion}\n`,
    );
  }
}
