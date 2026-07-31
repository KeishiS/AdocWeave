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
  "`cite:`の引用情報を公開projectionとWASM protocolへ公開しました。citation key、macro全体と各keyのsource range、名前付き属性および文書全体での出現順を、解析結果を再度字句解析せずに取得できます。",
  "利用側アプリが解決した引用表示をHTMLへ渡せるようにしました。`RenderInputs`へ引用の解決結果を与えると、その文字列で描画します。",
  "解決結果は表示順に並べた文字列の断片で、断片ごとに参照先anchorを指定できます。`(Smith 2024; Tanaka 2025)`のように、括弧や区切り文字を素の文字列のまま残し、著者名の部分だけをlinkにできます。",
  "`cite:[key]`のkeyが同じ文書の`[bibliography]`項目を指す場合、利用側アプリの解決なしでもその項目へlinkし、項目側の戻りlinkにも並びます。戻りlinkの番号は原文の位置の順に付きます。",
  "`RenderInputs`を、空の値から必要な種類だけを与えて組み立てる形へ変更しました。",
  "Linuxで、検査中にほかのプロセスがworkspaceを変更した場合に、解決できるはずのlocal targetを検査不能として報告することがあった問題を修正しました。",
  "前処理の入口を6変種から整理し、任意の入力を`PreprocessInputs`へまとめました。",
];

const contractNotes = [
  `統一package version：${RELEASE_NOTES_VERSION}`,
  `release manifest schema version：${manifest.schemaVersion}、distribution plan schema version：${plan.schemaVersion}、配布manifest schema version：2。`,
  `WASM protocol schema version：${RELEASE_NOTES_PROTOCOL_SCHEMA_VERSION}、Worker protocol version：${protocol.workerProtocolVersion}。schema versionをv0.22.0の7から9へ更新しました。`,
  "公開projectionへ`citations`を追加しました。既存のfieldは変更していません。",
  "WASM protocolへ`ResolvedCitation`、`CitationSegment`および`ResolvedCitationOutcome`を追加し、`renderInputs`へ`citations`を追加しました。省略した場合は解決結果が一つもない状態として扱うため、既存の要求はそのまま受け付けます。",
  "破壊的変更：前処理の入口から`preprocess_cancellable`、`preprocess_and_analyze_cancellable`、`preprocess_and_analyze_with_options`および`preprocess_and_analyze_cancellable_with_options`を削除しました。`preprocess_with`、`preprocess_and_analyze_with`および`EffectiveProcessingOptions::preprocess_and_analyze`へ統合しています。",
  "破壊的変更：`RenderInputs::new`を削除しました。`RenderInputs::default()`から`with_references`、`with_resources`および`with_citations`で必要な種類だけを与えます。",
  "CLI引数、Language Server protocolおよび設定schemaはv0.21.0から変更していません。",
  "利用側アプリが解決した引用の断片は、公開可能なプレーンテキストとして扱います。AsciiDoc、属性参照またはHTMLとして再解析しません。anchorは出力直前に検査し、その文書が定義している対象を指す場合だけlinkを生成します。",
  "診断code`unknown-citation-anchor`を追加しました。解決済みの引用が、文書の定義しないanchorを指した場合に返します。",
  "HTMLの許可class、エスケープおよび許可リスト方式は変更していません。",
  "GitHub Release以外のregistryへpackageまたは拡張を公開しません。",
];

const migrationNotes = [
  "設定の移行は不要です。既存の診断code、HTML出力および終了状態は、`cite:`を含まず引用の解決結果を渡さない文書で変わりません。",
  "`RenderInputs::new(references, resources)`を使っていた場合は`RenderInputs::default().with_references(references).with_resources(resources)`へ置き換えてください。空の`Vec`しか渡していなかった種類は、対応する呼び出しを省略できます。",
  "`preprocess_cancellable(source, snapshot, options, &token)`は`preprocess_with(source, snapshot, options, PreprocessInputs { cancellation: Some(&token) })`へ置き換えてください。`preprocess_and_analyze_cancellable`も同じ形です。キャンセルを渡していなかった場合は`PreprocessInputs::default()`を使います。",
  "`preprocess_and_analyze_with_options(source, snapshot, &effective)`は`effective.preprocess_and_analyze(source, snapshot, PreprocessInputs::default())`へ置き換えてください。",
  "WASM APIの`renderInputs`は変更不要です。`citations`を省略した場合の動作はv0.22.0と同じです。",
  "公開projectionをJSON schemaや型定義で検証している場合は、`citations`の追加に合わせて更新してください。",
  "`cite:[key]`のkeyが文書内の`[bibliography]`項目と同じ名前の場合、v0.22.0ではkeyをそのまま表示していましたが、この版からその項目へのlinkになります。項目側にも戻りlinkが増えます。",
  "利用側アプリが引用を解決する場合は、`cite:`から閉じ括弧までのmacro全体のrangeで解決結果を渡してください。個々のkeyのrangeでは照合しません。",
  `CLI、LSP、browser、ZedおよびVS Code向け配布物のversionを${RELEASE_NOTES_VERSION}へそろえてください。`,
];

const knownConstraints = [
  `対応Rust toolchain：${manifest.rustVersion}。このreleaseのflake.lockで固定しています。`,
  "native binaryは配布計画に定義したLinux、macOSおよびWindows環境へ提供します。macOSとWindowsのbinaryはOSのsystem libraryへ動的linkします。",
  "macOS binaryへDeveloper ID署名とnotarizationを行わず、Windows binaryへAuthenticode署名を行いません。OSの警告が表示された場合はchecksumとattestationを確認してください。",
  "Zed拡張はdevelopment extension、VS Code拡張はVSIXとして手動導入します。拡張registryへは公開しません。",
  "公式Playgroundはこのreleaseに含みません。`adocweave preview`は利用者の端末で実行するローカル機能です。",
  "packageはcrates.io、npmまたはOS package registryへ公開しません。Nix packageはこのrepositoryのflakeから直接buildします。",
  "AdocWeaveはBibTeXの保存・解析やCSL相当の書誌の組版を行いません。citation keyの解決と引用表示の組み立ては利用側アプリの責務です。",
  "解決結果を渡さない引用の表示は`unresolved_references`の設定に従い、`hidden`では出力しません。ただし文書内の`[bibliography]`項目を指すkeyは、設定にかかわらずその項目へのlinkとして出力します。",
  "引用の解決結果は文書全体の並べ替えを行いません。番号付きの引用styleで通し番号を振る場合は、利用側アプリが出現順を見て文字列を決めてください。出現順は公開projectionの`citations`から取得できます。",
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
