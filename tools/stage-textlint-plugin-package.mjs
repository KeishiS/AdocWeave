import { cp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

import { expectedManifestFiles, loadTextlintPluginPackageContract } from "./textlint-plugin-package-contract.mjs";

const ROOT = fileURLToPath(new URL("../", import.meta.url));

export async function stageTextlintPluginPackage(stageDirectory, wasmDirectory, noticeFile) {
  const contract = loadTextlintPluginPackageContract();
  const release = JSON.parse(await readFile(join(ROOT, "release-manifest.json"), "utf8"));
  const stage = resolve(stageDirectory);
  await rm(stage, { recursive: true, force: true });
  await mkdir(stage, { recursive: true });
  for (const entry of contract.files) {
    const destination = join(stage, entry.path);
    await mkdir(dirname(destination), { recursive: true });
    if (entry.source) {
      await cp(join(ROOT, entry.source), destination, { errorOnExist: true });
      continue;
    }
    if (entry.generator === "third-party-notices") await cp(resolve(noticeFile), destination, { errorOnExist: true });
    else if (entry.generator === "wasm-wrapper") await cp(join(resolve(wasmDirectory), "adocweave_textlint_wasm.js"), destination, { errorOnExist: true });
    else if (entry.generator === "wasm-binary") await cp(join(resolve(wasmDirectory), "adocweave_textlint_wasm_bg.wasm"), destination, { errorOnExist: true });
    else if (entry.generator === "package-manifest") {
      const manifest = {
        name: contract.identity.packageName,
        version: release.packageVersion,
        description: "AsciiDoc Processor Plugin for textlint powered by AdocWeave",
        private: contract.identity.private,
        type: "module",
        main: "./index.mjs",
        types: "./index.d.mts",
        exports: { ".": { types: "./index.d.mts", import: "./index.mjs", default: "./index.mjs" } },
        files: expectedManifestFiles(contract),
        engines: { node: contract.compatibility.nodeEngine },
        peerDependencies: {
          "@textlint/types": contract.compatibility.textlintTypesVersion,
          textlint: contract.compatibility.textlintVersion,
        },
        keywords: ["asciidoc", "textlint", "textlintplugin"],
        license: "MIT OR Apache-2.0",
        homepage: "https://github.com/KeishiS/adocweave",
        bugs: "https://github.com/KeishiS/adocweave/issues",
        repository: { type: "git", url: "https://github.com/KeishiS/adocweave.git", directory: "packages/textlint-plugin-asciidoc" },
      };
      await writeFile(destination, `${JSON.stringify(manifest, null, 2)}\n`);
    } else throw new Error(`未対応の生成方法です：${entry.generator}`);
  }
  return { contract, stage };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [stage, wasm, notice] = process.argv.slice(2);
  if (!stage || !wasm || !notice) {
    process.stderr.write("usage: node tools/stage-textlint-plugin-package.mjs STAGE_DIR WASM_DIR NOTICE_FILE\n");
    process.exit(2);
  }
  try { const result = await stageTextlintPluginPackage(stage, wasm, notice); process.stdout.write(`textlint plugin package staged: ${result.stage}\n`); }
  catch (error) { process.stderr.write(`${error.message}\n`); process.exitCode = 1; }
}
