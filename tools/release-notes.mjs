import { readFileSync } from "node:fs";
import process from "node:process";

const ROOT = new URL("../", import.meta.url);
const manifest = JSON.parse(readFileSync(new URL("release-manifest.json", ROOT), "utf8"));
const plan = JSON.parse(readFileSync(new URL("release/distribution-plan.json", ROOT), "utf8"));
const protocol = JSON.parse(readFileSync(new URL("protocol/public-api.json", ROOT), "utf8"));

export const REQUIRED_RELEASE_NOTE_HEADINGS = [
  "## 対応環境",
  "## 公開契約と破壊的変更",
  "## 既知の制約",
  "## 配布物の検証",
  "## 更新とロールバック",
];

const highlights = [
  "Linux ARM64／x86-64、macOS ARM64／x86-64およびWindows x86-64向けに、CLIとLanguage Serverの検証済みZIPを提供します。",
  "VS Code拡張を追加しました。基本構文色付けと、Language Serverによる診断、補完、定義移動、整形などを利用できます。",
  "VS Code拡張とZed拡張は、配布manifestで対象platform、archive、byte数およびSHA-256を検証してmanaged Language Serverを導入します。",
  "VSIXをCLI、Language Server、browser packageおよびZed拡張と同じGitHub Releaseへ統合しました。",
  "配布物のchecksum、SBOM、Artifact Attestationおよびclean installation検査を全対象platformへ拡張しました。",
];

const contractNotes = [
  `統一package version：${manifest.packageVersion}`,
  `release manifest schemaをversion 2から${manifest.schemaVersion}、distribution plan schemaをversion 1から${plan.schemaVersion}、配布manifest schemaをversion 1から2へ更新しました。旧schemaを読むconsumerは、新しいVSIX asset、全platform共通ZIPおよびplatform選択情報へ対応してください。`,
  `WASM protocol schema version：${protocol.schemaVersion}、Worker protocol version：${protocol.workerProtocolVersion}。古いrequestとWorker envelopeは拒否されます。`,
  "native archive形式を全platformでflatなZIPへ統一しました。従来のLinux向け`.tar.xz`を参照する導入scriptは更新が必要です。",
  "VS Code拡張は、明示した絶対path、`PATH`、検証済みcache、managed downloadの順で同じversionのLanguage Serverを選択します。",
  "未信頼workspaceのLanguage Server path設定は使用しません。任意の引数、環境変数またはshell commandをworkspace設定から受け取りません。",
  "未対応platformではmanaged downloadを開始しません。同じversionの外部Language Serverを明示設定した場合だけ利用できます。",
  "VSIXは決定的にbuildし、許可file、license、size、source mapおよび機械固有pathの不在を検査します。",
  "GitHub Release以外のregistryへpackageまたは拡張を公開しません。",
];

const knownConstraints = [
  `対応Rust toolchain：${manifest.rustVersion}。このreleaseのflake.lockで固定しています。`,
  "native binaryは配布計画に定義したLinux、macOSおよびWindows環境へ提供します。macOSとWindowsのbinaryはOSのsystem libraryへ動的linkします。",
  "macOS binaryへDeveloper ID署名とnotarizationを行わず、Windows binaryへAuthenticode署名を行いません。OSの警告が表示された場合はchecksumとattestationを確認してください。",
  "Zed拡張はdevelopment extension、VS Code拡張はVSIXとして手動導入します。拡張registryへは公開しません。",
  "Zed extension APIではhost OS versionを取得できないため、macOSとWindowsの最小versionはdownload前ではなく、配布binaryのdeployment targetをOS loaderが強制します。",
  "packageはcrates.io、npmまたはOS package registryへ公開しません。Nix packageはこのrepositoryのflakeから直接buildします。",
];

function markdownList(items) {
  return items.map((item) => `- ${item}`).join("\n");
}

export function buildReleaseNotes(tag) {
  if (tag !== `v${manifest.packageVersion}`) throw new Error("Release Notesのtagがpackage versionと一致しません");
  const osNames = { darwin: "macOS", linux: "Linux", win32: "Windows" };
  const targets = plan.targets
    .map((target) => `- ${osNames[target.os]} ${target.architecture}（\`${target.triple}\`）`)
    .join("\n");
  const notes = `## 主な変更\n\n${markdownList(highlights)}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[0]}\n\n${targets}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[1]}\n\n${markdownList(contractNotes)}\n\n` +
    "consumerは記載されたpackage versionを厳密に一致させる必要があります。異なるversionのCLI、LSP、browser、ZedまたはVS Code向け配布物を混在させないでください。\n\n" +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[2]}\n\n${markdownList(knownConstraints)}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[3]}\n\n` +
    "すべてのrelease assetをdownloadし、`sha256sum --check sha256.sum`を実行してください。その後、必要なassetを`gh attestation verify <asset> --repo KeishiS/adocweave`で検証してください。\n\n" +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[4]}\n\n` +
    "Linuxの導入scriptではarchive拡張子を`.zip`へ変更し、flatなarchive rootから実行fileを配置してください。新versionは既存directoryへ上書きせず、検証後にだけ選択先を切り替えます。\n\n" +
    "VS Codeでは検証済みVSIXを手動導入し、拡張とLanguage Serverのversion一致を確認してください。受入確認が成功するまで以前のVSIXとnative directoryを保持します。\n\n" +
    "rollback時は以前のversion別directoryまたはVSIXへ戻します。詳細は`docs/user-guide/release-installation.adoc`を参照してください。\n";
  return notes;
}

export function validateReleaseNotes(body) {
  for (const heading of REQUIRED_RELEASE_NOTE_HEADINGS) {
    if (!body.includes(heading)) throw new Error(`Release Notesに必須見出しがありません：${heading}`);
  }
}

if (process.argv[1] && import.meta.url === new URL(process.argv[1], "file:").href) {
  const tag = process.argv[2];
  // cargo-distの自動生成本文は英語であるため読み捨て、単一の日本語本文を生成します。
  for await (const _chunk of process.stdin) {
    // 標準入力を最後まで読み、呼出側のpipeを正常に終了させます。
  }
  const output = buildReleaseNotes(tag);
  validateReleaseNotes(output);
  process.stdout.write(output);
}
