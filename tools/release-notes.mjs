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
  "local target検査でpermission不足と検査件数上限の診断、終了codeおよびJSON出力を共通fixtureへ固定しました。",
  "Linuxではlocal targetを検証済みdirectory handleから読み込み、検査後のsymlink差し替えでroot外を読まないようにしました。",
  "include処理の論理source ID、検証対象pathおよび読込済みsourceを別の型へ分離し、診断とsource mapへOS pathを漏らしません。",
  "追跡するAsciiDoc文書を自動的に検査対象へ含め、追加文書の検査漏れと重複した列挙を拒否します。",
  "native成果物へ影響するPull RequestでWindows・macOS smokeを実行し、OS依存のpath、DLL、archiveおよび導入契約を共通fixtureで検査します。",
];

const contractNotes = [
  `統一package version：${manifest.packageVersion}`,
  `release manifest schema version：${manifest.schemaVersion}、distribution plan schema version：${plan.schemaVersion}、配布manifest schema version：2。v0.14.0からschema形状を変更していません。`,
  `WASM protocol schema version：${protocol.schemaVersion}、Worker protocol version：${protocol.workerProtocolVersion}。v0.14.0のrequestとWorker envelopeの形状を維持します。`,
  "CLI option、診断codeおよびJSON schemaに破壊的変更はありません。",
  "Linuxのlocal target検査は静的なworkspace snapshotを前提とせず、同じdirectory handle系列から検査と読込みを行います。macOS、Windowsおよび汎用Rustのportable adapterは、静的snapshot向けのbest effortであり、敵対的な同時変更への耐性を宣言しません。",
  "includeのsource identityは利用者が指定した論理IDを維持します。権限検査に使用したcanonical filesystem pathは公開診断とsource mapへ含めません。",
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
    "native archiveは従来どおりflatなZIPです。新versionは既存directoryへ上書きせず、検証後にだけ選択先を切り替えます。\n\n" +
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
