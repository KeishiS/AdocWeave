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
  "CLIとLanguage Serverの終了コードを、失敗の理由ごとに0から4へ分けました。使用法の誤り、入出力の失敗、診断による失敗および上限超過を呼び出し側が区別できます。",
  "公開Rust APIの差分を、直前のstable tagと比較する検査をrelease gateへ追加しました。patch版に破壊的変更があれば公開前に失敗します。",
  "設定fileの読み込みに上限を設けました。ほかのすべての読み込みが有界であるのに対し、ここだけがsizeを見ずに全体をメモリへ読んでいました。",
  "Language Serverが、読み込んだ設定をdirectoryごとに保持します。これまでは打鍵のたびに設定fileのpath解決、読み込み、digest計算およびTOML解析を、ほかの要求へ応答するthreadの上で繰り返していました。",
  "共有conformance fixtureに、現在の実装から成果物を書き出すコマンドと、fileがその結果と一致することを検査するtestを用意しました。",
  "受入検査に使うChromiumをflakeで固定しました。これまでCIは実行環境に同梱されたbrowserを使っており、そのversionは記録に残りませんでした。",
  "変更したpathが到達できるsource検査だけを実行するようにしました。文書だけの変更で、coreの再compileやfuzzを行いません。",
];

const contractNotes = [
  `統一package version：${RELEASE_NOTES_VERSION}`,
  `release manifest schema version：${manifest.schemaVersion}、distribution plan schema version：${plan.schemaVersion}、配布manifest schema version：2。`,
  `WASM protocol schema version：${RELEASE_NOTES_PROTOCOL_SCHEMA_VERSION}、Worker protocol version：${protocol.workerProtocolVersion}。v0.23.0から変更していません。`,
  "破壊的変更：CLIとLanguage Serverの終了コードを、失敗の理由ごとに分けました。0は成功、1は診断がしきい値に達した場合、2は指定した引数またはオプションを実行できない場合、3はファイル、ストリームまたはリソースの読み書きに失敗した場合、4は入力サイズまたはリソースの上限を超えた場合です。",
  "Language Serverは、shutdownを受け取る前にexitを受け取った場合だけ1を返します。これはLanguage Server Protocolが定める値であり、上の規約より優先します。",
  "公開Rust API、WASM protocol、CLI引数、Language Server protocolおよび設定schemaは、終了コードを除いてv0.23.0から変更していません。",
  "設定fileの読み込みに1 MiBの上限を設けました。上限を超えるfileは読み込まずerrorとします。",
  "文書の記述を実装に合わせました。local targetの欠落、root外および権限不足は、ほかの診断と同じくerror件数へ加算されるため、--fail-on neverでは終了コードが0になります。以前の文書は無条件に「0以外」と記載していました。",
  "設定の探索境界を明記しました。Language Serverはworkspace folder、CLIは作業directoryで探索を停止し、処理対象がその外にある場合は設定fileを適用しません。",
  "HTML出力、診断code、公開projectionおよび許可リスト方式は変更していません。",
  "GitHub Release以外のregistryへpackageまたは拡張を公開しません。",
];

const migrationNotes = [
  "設定の移行は不要です。HTML出力、診断codeおよび公開projectionは変わりません。",
  "終了コードが0かどうかだけを判定している場合、変更は不要です。これまでどおり0とそれ以外で判定できます。",
  "「失敗は必ず1」を前提にしている場合は、判定を見直してください。使用法の誤りは2、入出力の失敗は3、上限超過は4を返します。",
  "1 MiBを超える設定fileを使っている場合は、読み込まずerrorになります。設定fileが記述するのは根、上限および規則の設定であり、この大きさになることは想定していません。",
  "--fail-on neverでlocal targetの失敗を終了コードとして受け取っていた場合、その動作は以前から0です。文書の記載を実装に合わせました。",
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
  "Language Serverの初期化は、workspace全体の走査を終えてから応答します。大きなworkspaceでは、その間ほかの要求へ応答できません。打鍵中の応答性はこのreleaseで改善しています。",
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
