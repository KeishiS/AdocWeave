import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";

import { unzipSync } from "fflate";

const packageJson = JSON.parse(readFileSync("package.json", "utf8"));
const path = resolve("../../target/distrib", `adocweave-vscode-${packageJson.version}.vsix`);
if (!existsSync(path)) throw new Error("VSIXがありません。先にnpm run packageを実行してください");
const bytes = readFileSync(path);
const entries = unzipSync(bytes);
const manifest = JSON.parse(Buffer.from(entries["extension/package.json"]).toString("utf8"));
if (manifest.version !== packageJson.version || manifest.main !== "./dist/extension.cjs") {
  throw new Error("VSIX manifestがsource packageと一致しません");
}
if (!entries["extension/LICENSE-APACHE"] || !entries["extension/LICENSE-MIT"]) {
  throw new Error("VSIXにlicenseがありません");
}
if (Object.keys(entries).some((name) => name.includes("node_modules") || name.endsWith(".map"))) {
  throw new Error("VSIXに許可していない依存またはsource mapがあります");
}
process.stdout.write(
  `VSIX smokeに成功しました：${bytes.byteLength} bytes、SHA-256 ${createHash("sha256").update(bytes).digest("hex")}\n`,
);
