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

const releaseVersionParts = RELEASE_NOTES_VERSION.split(".").map(Number);
if (releaseVersionParts.length !== 3 || releaseVersionParts.some((part) => !Number.isInteger(part))) {
  throw new Error(`Release NotesのversionがSemVerではありません：${RELEASE_NOTES_VERSION}`);
}
const [releaseMajor, releaseMinor, releasePatch] = releaseVersionParts;
if (releasePatch < 1) {
  throw new Error("Release Notesの訂正対象となる直前のpatch版がありません");
}
export const PREVIOUS_RELEASE_VERSION = `${releaseMajor}.${releaseMinor}.${releasePatch - 1}`;

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
  `v${PREVIOUS_RELEASE_VERSION}のRelease Notesで設定schemaに変更がないと案内していた誤りを訂正しました。実際には\`\`resources.roots\`\`と\`\`local-targets.project-root\`\`へ相対パスの制約を加え、\`\`local-targets.enabled\`\`が\`\`true\`\`の場合は\`\`project-root\`\`を必須にしていました。実行時の設定検査に変更はありません。`,
  "前処理のdirectiveが属性名の大文字と小文字を区別しなくなりました。AsciiDocの属性名は大文字と小文字を区別しませんが、``ifdef``、``ifndef``、``ifeval``および``include``は書かれた名前をそのまま探しており、小文字の綴りだけが一致していました。``Web``という属性を渡した利用者が``ifdef::Web[]``と書いても成立せず、同じ文書の本文にある``{WEB}``は解決するという食い違いが起きていました。",
  "存在しないファイルを繰り返し参照しても、調べられるパス数の上限を重複して消費しなくなりました。読み取りの失敗を記録していなかったため、同じ欠落パスを二回読むと上限を二回消費し、次の実在するファイルが上限超過で読めませんでした。",
  "Browser packageの型定義に``wasm-trapped``を追加しました。実行時の判定にはこの符号が含まれており、型定義だけが欠けていたため、型で絞り込んだあとに網羅的な分岐を書けませんでした。同梱するREADMEが古いschema versionを案内していた点も直しました。",
  "VS Code拡張が、Content-Lengthを返さない配信からもLanguage Serverを導入できるようになりました。ヘッダーが無い応答をサイズ不一致として拒否していました。",
  "Zed拡張の導入ロックを、作成した処理だけが削除するようにしました。取得に15分以上かかると後続の処理がロックを引き継ぎますが、先行する処理の終了時に後続のロックまで削除しており、三つ目の導入が同時に始まれました。",
];

const contractNotes = [
  `統一package version：${RELEASE_NOTES_VERSION}`,
  `release manifest schema version：${manifest.schemaVersion}、distribution plan schema version：${plan.schemaVersion}、配布manifest schema version：2。`,
  `WASM protocol schema version：${RELEASE_NOTES_PROTOCOL_SCHEMA_VERSION}、Worker protocol version：${protocol.workerProtocolVersion}。v0.23.0から変更していません。`,
  manifestSchemaNote,
  `公開契約に破壊的変更はありません。v${PREVIOUS_RELEASE_VERSION}では、設定schemaの\`\`resources.roots\`\`と\`\`local-targets.project-root\`\`へ相対パスの制約を加え、\`\`local-targets.enabled\`\`が\`\`true\`\`の場合は\`\`project-root\`\`を必須にしていました。これらは以前から実行時に拒否していた設定をschemaでも拒否する変更です。`,
  "Browser packageの``AdocWeaveClientLifecycleErrorCode``へ``wasm-trapped``を加えました。この符号は以前から実行時に返っており、型定義だけが実態を表していませんでした。この列挙を網羅的に扱っているTypeScriptの利用側は、分岐の追加が必要です。",
  "挙動の変更：``ifdef``、``ifndef``、``ifeval``および``include``が、属性名の大文字と小文字を区別しなくなります。これまで成立しなかった大文字を含む綴りが成立します。小文字の綴りの結果は変わりません。",
  "挙動の変更：存在しないパスを繰り返し参照しても、調べられるパス数の上限を一度しか消費しません。異なる欠落パスは、これまでどおりそれぞれ消費します。",
  "GitHub Release以外のregistryへpackageまたは拡張を公開しません。",
];

const migrationNotes = [
  `実行時に受理されていた設定の移行は不要です。絶対パス、親ディレクトリへ移動する\`\`..\`\`、または\`\`project-root\`\`を省略した有効な\`\`local-targets\`\`は以前から実行時に拒否していました。v${PREVIOUS_RELEASE_VERSION}からはエディターなどのschema検査でも拒否します。`,
  "大文字を含む綴りでdirectiveの属性名を書いた文書は、これまで条件が成立しませんでした。このreleaseからは成立するため、結果が変わります。小文字の綴りで書いた文書の結果は変わりません。",
  "存在しないパスを繰り返し参照する文書では、上限超過にならず解析が進みます。上限超過を前提にしていた利用側は、期待する結果を確認してください。",
  "TypeScriptで``AdocWeaveClientLifecycleErrorCode``を網羅的に扱っている場合は、``wasm-trapped``の分岐を加えてください。実行時にはこれまでもこの符号が返っており、分岐の欠落は型では現れていませんでした。",
  `release manifestを機械的に読んでいる場合も追随は不要です。\`\`schemaVersion\`\`は${manifest.schemaVersion}のままです。`,
  `CLI、LSP、browser、ZedおよびVS Code向け配布物のversionを${RELEASE_NOTES_VERSION}へそろえてください。バージョンの異なる配布物を混ぜて使えないため、更新する場合はすべてを入れ替えます。`,
  "VS Code拡張とZed拡張は、導入の不具合を直しているため更新を推奨します。",
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
