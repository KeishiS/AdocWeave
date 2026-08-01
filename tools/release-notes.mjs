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

// The release manifest schema version the previous stable release shipped.
//
// An earlier release shipped notes announcing a manifest change that had not
// been made: the same body stated one schema version in one line and a
// different transition in another. Deriving the sentence from this value and
// the manifest keeps a claim from outliving the change it describes, and keeps
// a release that changes nothing from announcing a transition.
export const PREVIOUS_RELEASE_MANIFEST_SCHEMA_VERSION = 4;

const manifestSchemaNote =
  manifest.schemaVersion === PREVIOUS_RELEASE_MANIFEST_SCHEMA_VERSION
    ? `release manifestのschema versionは${manifest.schemaVersion}のままで、項目を追加も削除もしていません。`
    : `release manifestのschema versionを${PREVIOUS_RELEASE_MANIFEST_SCHEMA_VERSION}から${manifest.schemaVersion}へ更新しました。`;

export const REQUIRED_RELEASE_NOTE_HEADINGS = [
  "## 対応環境",
  "## 公開契約と破壊的変更",
  `## v${RELEASE_NOTES_VERSION}への移行`,
  "## 既知の制約",
  "## 配布物の検証",
  "## 更新とロールバック",
];

const highlights = [
  "アンカーの表示テキストをIDと分けて読むようにしました。AsciiDocは``[[[smith2024,1]]]``のようにカンマの後ろへ表示テキストを書きます。これまではカンマ以降もIDの一部として読んでいたため、仕様どおりに書いた文書は``<<smith2024>>``が解決できず、参照先が無いという診断が出ていました。インラインアンカー``[[id,表示テキスト]]``も同じです。",
  "参考文献の項目に表示テキストを持たせ、公開projectionへ出します。番号を付ける引用styleでは、番号を利用側アプリが決めたあと、表示と相互linkをAdocWeaveへ任せられます。",
  "条件分岐とincludeのdirectiveを字句として認識するようにしました。前処理を行わない解析では``ifeval::[\"a\" == \"b\"]``が名前付きマクロとして読まれ、先頭の``ifeval:``がURL schemeに見えるため、条件分岐を書いた利用者は書いていないURLを拒否したと告げられていました。HTMLも属性展開まで適用された壊れた段落になっていました。",
  "利用側アプリ用の文書属性の接頭辞を設定で予約する案は、実施しない判断としました。理由はロードマップに記録しています。",
];

const contractNotes = [
  `統一package version：${RELEASE_NOTES_VERSION}`,
  `release manifest schema version：${manifest.schemaVersion}、distribution plan schema version：${plan.schemaVersion}、配布manifest schema version：2。`,
  `WASM protocol schema version：${RELEASE_NOTES_PROTOCOL_SCHEMA_VERSION}、Worker protocol version：${protocol.workerProtocolVersion}。v0.23.0から変更していません。`,
  manifestSchemaNote,
  "破壊的変更：``BibliographyEntry``へ``label``を追加しました。``[[[id,表示テキスト]]]``のカンマ以降を保持します。表示テキストが無い場合は``None``です。この構造体を自分で構築している利用側は、``label``の指定を加えてください。",
  "破壊的変更：``Unsupported``へ``kind``を追加し、``UnsupportedKind``を公開しました。このバージョンが読めない構文（``Syntax``）と、対応済みだが今回の解析では評価していないpreprocessor directive（``UnprocessedDirective``）を区別します。``UnsupportedKind``の既定は``Syntax``です。",
  "破壊的変更：``SyntaxIssueClass``へ``UnprocessedDirective``を追加しました。この列挙を網羅的に``match``している利用側は、分岐の追加が必要です。",
  "公開projectionの``catalogs.bibliography``の各項目へ``label``が加わります。値は文字列またはnullです。",
  "WASM protocolのschema versionとWorker protocol versionは変更していません。追加した``label``は既存の項目を変えません。",
  "挙動の変更：条件分岐とincludeのdirectiveを含む行の診断codeが``invalid-url-scheme``から``unprocessed-directive``へ変わります。HTML出力では、これらの行が1つの段落へ吸収されず、書いたままの文字列として残ります。",
  "挙動の変更：``[[[id,表示テキスト]]]``と``[[id,表示テキスト]]``のIDがカンマの手前までになります。カンマを含むIDを書いていた文書では、参照先のIDが変わります。",
  "CLI引数、Language Server protocolおよび設定schemaはv0.26.1から変更していません。",
  "GitHub Release以外のregistryへpackageまたは拡張を公開しません。",
];

const migrationNotes = [
  "設定の移行は不要です。終了コードとAsciiDocのそのほかの解釈は変わりません。",
  "Rust APIを使う利用側は、``BibliographyEntry``と``Unsupported``を自分で構築している箇所へ新しい項目を加えてください。``SyntaxIssueClass``を網羅的に``match``している箇所には``UnprocessedDirective``の分岐を加えてください。読み取りだけの利用側に変更は要りません。",
  "公開projectionを保存している利用側は、``catalogs.bibliography``へ``label``が加わるため、保存済みの結果を作り直してください。",
  "条件分岐またはincludeのdirectiveを含む文書を、前処理を行わずに解析している利用側は、``invalid-url-scheme``で拒否していた判定を``unprocessed-directive``へ変えてください。",
  "カンマを含むアンカーIDを書いていた文書は、IDがカンマの手前までになります。該当する文書では、``<<ID>>``の書き方を確認してください。",
  `release manifestを機械的に読んでいる場合も追随は不要です。\`\`schemaVersion\`\`は${manifest.schemaVersion}のままです。`,
  `CLI、LSP、browser、ZedおよびVS Code向け配布物のversionを${RELEASE_NOTES_VERSION}へそろえてください。バージョンの異なる配布物を混ぜて使えないため、更新する場合はすべてを入れ替えます。`,
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
  "Language Serverはworkspaceの走査を初期化の応答後に、要求へ応答するthreadの外で行います。走査中もほかの要求へ応答しますが、走査の完了前は、開いた文書の解析にworkspace内のほかの文書が反映されません。走査の完了後に再解析します。",
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
