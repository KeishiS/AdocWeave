import { readFileSync } from "node:fs";
import process from "node:process";

import {
  PUBLIC_PROTOCOL_SCHEMA_VERSION,
  RELEASE_NOTES_VERSION,
} from "./release-policy.mjs";

const ROOT = new URL("../", import.meta.url);
const manifest = JSON.parse(readFileSync(new URL("release-manifest.json", ROOT), "utf8"));
const plan = JSON.parse(readFileSync(new URL("release/distribution-plan.json", ROOT), "utf8"));
const protocol = JSON.parse(readFileSync(new URL("protocol/public-api.json", ROOT), "utf8"));
export { RELEASE_NOTES_VERSION };
export const RELEASE_NOTES_PROTOCOL_SCHEMA_VERSION = PUBLIC_PROTOCOL_SCHEMA_VERSION;

export const REQUIRED_RELEASE_NOTE_HEADINGS = [
  "## 対応環境",
  "## 公開契約と破壊的変更",
  `## v${RELEASE_NOTES_VERSION}への移行`,
  "## 既知の制約",
  "## 配布物の検証",
  "## 更新とロールバック",
];

const highlights = [
  "外部の書誌ライブラリーを参照する`cite:[key]`を引用として解析し、公開APIへ構造化して公開しました。citation key、macro全体と各keyのsource range、名前付き属性および文書全体での出現順を取得できます。",
  "`cite:[a, b]`のように複数のkeyを指定できます。`locator`などの名前付き属性は引用の補足として保持します。",
  "keyを一つも持たない`cite:[]`を`invalid-catalog`として診断します。従来は出力から黙って消えていました。",
  "解析の入口を`Engine::analyze`と`Engine::analyze_with`の2つへ整理しました。渡す入力の組み合わせごとにmethodを用意しません。",
  "既存の標準bibliography（`[[[key]]]`と`<<key>>`）は変更していません。",
];

const contractNotes = [
  `統一package version：${RELEASE_NOTES_VERSION}`,
  `release manifest schema version：${manifest.schemaVersion}、distribution plan schema version：${plan.schemaVersion}、配布manifest schema version：2。`,
  `WASM protocol schema version：${RELEASE_NOTES_PROTOCOL_SCHEMA_VERSION}、Worker protocol version：${protocol.workerProtocolVersion}。v0.21.0から変更していません。`,
  "WASM protocol、CLI引数、Language Server protocolおよび設定schemaはv0.21.0から変更していません。",
  "破壊的変更：`Engine::analyze_with_source_id`、`analyze_cancellable`および`analyze_cancellable_with_source_id`を削除し、`Engine::analyze_with`へ統合しました。`AnalysisInputs`で`SourceId`と協調キャンセルを渡します。",
  "破壊的変更：`adocweave::ProductSet`と`adocweave::DocumentProducts`のcrate直下の再公開を外しました。`adocweave::output::conformance`から参照します。",
  "対応するAsciiDocの範囲を広げました。`cite:`を含む既存文書は、従来テキストとして出力していた箇所が引用として解析されます。",
  "`adocweave::AnalysisCacheKey`を新たに参照できます。",
  "HTMLの許可classへ`citation`を追加しました。エスケープと許可リスト方式は変更していません。",
  "GitHub Release以外のregistryへpackageまたは拡張を公開しません。",
];

const migrationNotes = [
  "設定の移行は不要です。既存の診断code、HTML出力および終了状態は`cite:`を含まない文書で変わりません。",
  "`Engine::analyze_with_source_id`を使っていた場合は`Engine::analyze_with(source, AnalysisInputs { source_id: Some(&id), ..Default::default() })`へ置き換えてください。",
  "`Engine::analyze_cancellable`は`AnalysisInputs { cancellation: Some(&token), ..Default::default() }`、両方を渡す場合は両fieldを指定します。引数の順序がsourceを先にする形へそろいます。",
  "`adocweave::ProductSet`と`adocweave::DocumentProducts`のimportを`adocweave::output::conformance`へ変更してください。",
  "`cite:`をこれまで通常のテキストとして出力していた文書は、出力が変わります。引用として扱わない場合は記述を変更してください。",
  "citation keyの解決は利用側アプリが行います。現在の版では解決結果をHTMLへ渡す経路がないため、解決前の表示は`unresolved_references`の設定に従います。",
  `CLI、LSP、browser、ZedおよびVS Code向け配布物のversionを${RELEASE_NOTES_VERSION}へそろえてください。`,
];

const knownConstraints = [
  `対応Rust toolchain：${manifest.rustVersion}。このreleaseのflake.lockで固定しています。`,
  "native binaryは配布計画に定義したLinux、macOSおよびWindows環境へ提供します。macOSとWindowsのbinaryはOSのsystem libraryへ動的linkします。",
  "macOS binaryへDeveloper ID署名とnotarizationを行わず、Windows binaryへAuthenticode署名を行いません。OSの警告が表示された場合はchecksumとattestationを確認してください。",
  "Zed拡張はdevelopment extension、VS Code拡張はVSIXとして手動導入します。拡張registryへは公開しません。",
  "公式Playgroundはこのreleaseに含みません。`adocweave preview`は利用者の端末で実行するローカル機能です。",
  "packageはcrates.io、npmまたはOS package registryへ公開しません。Nix packageはこのrepositoryのflakeから直接buildします。",
  "AdocWeaveはBibTeXの保存・解析やCSL相当の書誌の組版を行いません。citation keyの解決は利用側アプリの責務です。",
  "citationの引用情報はまだprojectionとWASM protocolへ公開していません。現在はRust APIの`Analysis::citations()`から取得します。",
  "ホストが解決した引用表示をHTMLへ渡す経路はまだありません。解決前の表示は`unresolved_references`の設定に従い、`hidden`では出力しません。",
  "単一ファイルのworkspaceでは、同じディレクトリの別のAsciiDocファイルとinclude先を自動では読み込みません。複数ファイルの解析にはディレクトリのworkspace folderが必要です。",
];

function markdownList(items) {
  return items.map((item) => `- ${item}`).join("\n");
}

const MINIMUM_OS_DESCRIPTIONS = {
  "darwin:14.0": "macOS 14.0以降",
  "win32:10.0.17763": "Windows 10 version 1809（build 10.0.17763）以降",
};

function minimumOsDescription(target) {
  if (target.minimumOsVersion === null) return "";
  const description = MINIMUM_OS_DESCRIPTIONS[`${target.os}:${target.minimumOsVersion}`];
  if (!description) {
    throw new Error(`最小対応OS版の説明がありません：${target.os} ${target.minimumOsVersion}`);
  }
  return `、${description}`;
}

export function buildReleaseNotes(tag) {
  if (tag !== `v${RELEASE_NOTES_VERSION}`) {
    throw new Error(`Release Notesはv${RELEASE_NOTES_VERSION}専用です`);
  }
  const osNames = { darwin: "macOS", linux: "Linux", win32: "Windows" };
  const targets = plan.targets
    .map(
      (target) =>
        `- ${osNames[target.os]} ${target.architecture}（\`${target.triple}\`${minimumOsDescription(target)}）`,
    )
    .join("\n");
  const notes = `## 主な変更\n\n${markdownList(highlights)}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[0]}\n\n${targets}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[1]}\n\n${markdownList(contractNotes)}\n\n` +
    "consumerは記載されたpackage versionを厳密に一致させてください。異なるversionのCLI、LSP、browser、ZedまたはVS Code向け配布物を混在させないでください。\n\n" +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[2]}\n\n${markdownList(migrationNotes)}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[3]}\n\n${markdownList(knownConstraints)}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[4]}\n\n` +
    "すべてのrelease assetをdownloadし、`sha256sum --check sha256.sum`を実行してください。その後、必要なassetを`gh attestation verify <asset> --repo KeishiS/adocweave`で検証してください。\n\n" +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[5]}\n\n` +
    `native archiveはversion別directoryへ展開し、\`--version --json\`が\`${RELEASE_NOTES_VERSION}\`を返すことを確認してから選択先を切り替えてください。\n\n` +
    "VS Codeでは検証済みVSIXを手動導入し、拡張とLanguage Serverのversion一致を確認してください。受入確認が成功するまで以前のVSIXとnative directoryを保持します。\n\n" +
    "Zedでは新versionのmanaged Language Server取得とeditor機能を確認するまで旧versionのZed directoryを保持します。rollback時は旧directoryをdev extensionとして選び直し、Zedを再起動してください。\n\n" +
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
