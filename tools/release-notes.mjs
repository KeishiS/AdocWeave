import { readFileSync } from "node:fs";
import process from "node:process";

import {
  PUBLIC_PROTOCOL_SCHEMA_VERSION,
  RELEASE_NOTES_VERSION,
} from "./release-policy.mjs";
import { loadBreakingRustApi } from "./breaking-rust-api.mjs";
import { loadTextlintPluginPackageContract } from "./textlint-plugin-package-contract.mjs";

const ROOT = new URL("../", import.meta.url);
const manifest = JSON.parse(readFileSync(new URL("release-manifest.json", ROOT), "utf8"));
const plan = JSON.parse(readFileSync(new URL("release/distribution-plan.json", ROOT), "utf8"));
const protocol = JSON.parse(readFileSync(new URL("protocol/public-api.json", ROOT), "utf8"));
const textlintContract = loadTextlintPluginPackageContract();
const breakingRustApi = loadBreakingRustApi();
export { RELEASE_NOTES_VERSION };
export const RELEASE_NOTES_PROTOCOL_SCHEMA_VERSION = PUBLIC_PROTOCOL_SCHEMA_VERSION;

const releaseVersionParts = RELEASE_NOTES_VERSION.split(".").map(Number);
if (releaseVersionParts.length !== 3 || releaseVersionParts.some((part) => !Number.isInteger(part))) {
  throw new Error(`Release NotesのversionがSemVerではありません：${RELEASE_NOTES_VERSION}`);
}
if (breakingRustApi.releaseVersion !== RELEASE_NOTES_VERSION) {
  throw new Error(
    `破壊的変更記録のreleaseVersionがRelease Notesと一致しません：${breakingRustApi.releaseVersion}`,
  );
}
export const PREVIOUS_RELEASE_VERSION = "0.33.0";

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
  "## 公開仕様と破壊的変更",
  `## v${RELEASE_NOTES_VERSION}への移行`,
  "## 既知の制約",
  "## 配布物の検証",
  "## 更新とロールバック",
];

const highlights = [
  "#492：filesystem resourceの不在を正常な取得結果として扱い、Language Serverの初期走査を複数sessionで共有するjob上限とdraftへ移行しました。",
  "#495：tableの``cols``属性にある空のcolumn specを既定列として保持し、明示した列数どおりに解析するよう修正しました。",
];

export function breakingContractNotes(changes) {
  if (changes.length === 0) return ["Rust APIの破壊的変更：ありません。"];
  return changes.map((change) => `Rust APIの破壊的変更：${change.description}`);
}

export function breakingMigrationNotes(changes) {
  return changes.map((change) => change.migration);
}

/// Public contracts this release states are unchanged since the previous stable tag.
///
/// The sentence in the notes is built from this list rather than written beside
/// it. v0.27.2 announced that the configuration schema had not changed while the
/// same release changed it: the claim was prose, so nothing compared it with the
/// diff. `tools/release-claims.mjs` reads this list and checks every entry that
/// has a single machine-readable source of truth.
export const UNCHANGED_CONTRACTS = [
  "CLI引数",
  "Language Server protocol",
  "設定schema",
];

/// The file that decides whether a named contract changed.
///
/// A contract without an entry here is stated but not checked: CLI arguments and
/// the Language Server protocol are spread across the sources that implement
/// them, and a file diff would report every unrelated edit. The tool names the
/// unchecked contracts in its output so the reader knows how far the check goes.
export const CONTRACT_SOURCES = {
  "WASM protocol": "protocol/public-api.json",
  設定schema: "config/adocweave.schema.json",
};

/// Fields that carry the release version rather than the contract's shape.
export const CONTRACT_VERSION_FIELDS = ["packageVersion"];

const contractNotes = [
  `統一package version：${RELEASE_NOTES_VERSION}`,
  `release manifest schema version：${manifest.schemaVersion}、distribution plan schema version：${plan.schemaVersion}、配布manifest schema version：2。`,
  `WASM protocol schema version：${RELEASE_NOTES_PROTOCOL_SCHEMA_VERSION}。Worker protocol versionは${protocol.workerProtocolVersion}で、どちらもv${PREVIOUS_RELEASE_VERSION}から変更していません。`,
  manifestSchemaNote,
  ...breakingContractNotes(breakingRustApi.changes),
  "Rust host APIへ``FilesystemReadOutcome``と``read_utf8_outcome``、``read_target_utf8_outcome``および``reread_utf8_outcome``を追加しました。sessionとdraftの両方で``Found``と``NotFound``を区別できます。",
  "従来の読込APIは維持し、対象fileの不在を引き続き``ResourceError::Missing``として返します。",
  "Language Serverの初期走査では、列挙後に消えたfileを走査全体の失敗にせず、その候補だけを省略します。",
  "Rust host APIへ``FilesystemJobCoordinator``を追加しました。一つのjobに参加する複数sessionで、resource取得、取得byte、directory列挙、entryおよび候補変更の上限を共有します。I/O前の予約により並行処理でも残量を重複利用せず、draftを破棄しても使用量を戻しません。",
  "``LocalFilesystemDraft``は作成時にcoordinatorを必須とし、file読込とdirectory走査を同じjobへ計上します。取消や上限超過の後にI/Oが完了しても、最初の終了理由を維持したまま実際の使用量を確定します。",
  "Language Serverの初期ワークスペース走査は、ディレクトリ列挙、project設定の読込および全project scopeの本文読込を一つのfilesystem jobへ計上します。各scopeの変更はdraftへ隔離し、候補Workspaceの構築後にだけ採用します。",
  "textlint Processorの公開API、TxtASTへの変換結果および自動修正を行わない保証は変更していません。",
  `${UNCHANGED_CONTRACTS.join("、")}は変更していません。`,
  "GitHub Release以外のregistryへpackageまたは拡張を公開しません。",
];

const migrationNotes = [
  `Browser向けWASMのJSON requestを直接構築している場合は、${RELEASE_NOTES_VERSION}のpackageとAPIへ更新し、requestの\`\`packageVersion\`\`も\`\`${RELEASE_NOTES_VERSION}\`\`にそろえてください。\`\`schemaVersion\`\`はrequestの項目ではありません。requestには追加しないでください。保存済みの結果やcacheは、packageが公開する\`\`PROTOCOL_SCHEMA_VERSION = ${RELEASE_NOTES_PROTOCOL_SCHEMA_VERSION}\`\`を使って区別してください。`,
  "filesystemの不在を正常な結果として扱うRust consumerは、従来の読込APIから対応する``*_outcome``メソッドへ移行し、``FilesystemReadOutcome::NotFound``を処理してください。従来どおり不在をerrorにするconsumerは変更不要です。",
  "``LocalFilesystemSession::draft``を使うRust consumerは、処理単位で``FilesystemJobCoordinator``を作成し、``session.draft(&job)``として渡してください。同じ処理の再試行と複数sessionでは同じjobを共有し、処理が完了した後の別eventでは新しいjobを作成します。",
  `CLI、LSP、browser、Zed、VS Codeおよびtextlint向け配布物のversionを${RELEASE_NOTES_VERSION}へそろえてください。バージョンの異なる配布物を混ぜて使えないため、更新する場合はすべてを入れ替えます。`,
  ...breakingMigrationNotes(breakingRustApi.changes),
];

const knownConstraints = [
  `対応Rust toolchain：${manifest.rustVersion}。このreleaseのflake.lockで固定しています。`,
  "native binaryは配布計画に定義したLinux、macOSおよびWindows環境へ提供します。macOSとWindowsのbinaryはOSのsystem libraryへ動的linkします。",
  "macOS binaryへDeveloper ID署名とnotarizationを行わず、Windows binaryへAuthenticode署名を行いません。OSの警告が表示された場合はchecksumとattestationを確認してください。",
  "Zed拡張はdevelopment extension、VS Code拡張はVSIXとして手動導入します。拡張registryへは公開しません。",
  "ZedがLanguage Serverの導入中に異常終了すると、安全のため導入ロックを自動削除しません。すべてのZedプロセスを終了してから、エラーに表示されたロックのpathを削除して再試行してください。",
  "公式Playgroundはこのreleaseに含みません。`adocweave preview`は利用者の端末で実行するローカル機能です。",
  "packageはcrates.io、npmまたはOS package registryへ公開しません。Nix packageはこのrepositoryのflakeから直接buildします。",
  `textlint用Processorの対応範囲はNode.js \`\`${textlintContract.compatibility.nodeEngine}\`\`、textlint \`\`${textlintContract.compatibility.textlintVersion}\`\`です。includeは展開せず、入力した一つの物理ファイルだけを検査します。`,
  "AdocWeaveはBibTeXの保存・解析やCSL相当の書誌の組版を行いません。citation keyの解決と引用表示の組み立ては利用側アプリの責務です。",
  "解決結果を渡さない引用の表示は`unresolved_references`の設定に従い、`hidden`では出力しません。ただし文書内の`[bibliography]`項目を指すkeyは、設定にかかわらずその項目へのlinkとして出力します。",
  "引用の解決結果は文書全体の並べ替えを行いません。番号付きの引用styleで通し番号を振る場合は、利用側アプリが出現順を見て文字列を決めてください。出現順は公開projectionの`citations`から取得できます。",
  "単一ファイルのworkspaceでは、同じディレクトリの別のAsciiDocファイルとinclude先を自動では読み込みません。複数ファイルの解析にはディレクトリのworkspace folderが必要です。",
  "Language Serverはworkspaceの走査を初期化の応答後に、要求へ応答するthreadの外で行います。走査中もほかの要求へ応答しますが、走査の完了前は、開いた文書の解析にworkspace内のほかの文書が反映されません。走査の完了後に再解析します。",
  "Linuxでfilesystemのhandle相対競合耐性を利用するには、``/proc/self/fd``を読み取れる実行環境が必要です。利用できない場合や、開いたfileのpathとidentityを確認できない場合は、安全性の低いpath検査へ切り替えず、``local-target-unverifiable``としてworkspaceの読込を拒否します。macOSとWindowsは、同時変更のない静的なfilesystem snapshotだけを前提とします。",
  "一つのfilesystem policyが保持できるrootは128件までです。読込対象を増やす場合は、設定のrootを必要な上位directoryへまとめてください。",
  "Language Serverの初期ワークスペース走査は、複数project scopeの合計で設定と本文の読込10,000回、取得内容50 MiB、directory entry 100,000件、候補変更10,000回、参加session 10,002件までです。project設定のsession単位の上限も別に適用します。",
  "``workspace.scan.exclude``はLanguage Serverの初期走査だけに適用します。CLI入力、明示的に開いた文書、file watcherの通知およびinclude先を拒否する設定ではありません。",
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
    "consumerは記載されたpackage versionを厳密に一致させてください。異なるversionのCLI、LSP、browser、Zed、VS Codeまたはtextlint向け配布物を混在させないでください。\n\n" +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[2]}\n\n${markdownList(migrationNotes)}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[3]}\n\n${markdownList(knownConstraints)}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[4]}\n\n` +
    "すべてのrelease assetをdownloadし、`sha256sum --check sha256.sum`を実行してください。その後、必要なassetを`gh attestation verify <asset> --repo KeishiS/adocweave`で検証してください。\n\n" +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[5]}\n\n` +
    `native archiveはversion別directoryへ展開し、\`--version --json\`が\`${RELEASE_NOTES_VERSION}\`を返すことを確認してから選択先を切り替えてください。\n\n` +
    "VS Codeでは検証済みVSIXを手動導入し、拡張とLanguage Serverのversion一致を確認してください。受入確認が成功するまで以前のVSIXとnative directoryを保持します。\n\n" +
    "Zedでは新versionのmanaged Language Server取得とeditor機能を確認するまで旧versionのZed directoryを保持します。rollback時は旧directoryをdev extensionとして選び直し、Zedを再起動してください。\n\n" +
    "textlint用Processorは新しいReleaseのtarball URLへ変更してlockfileを更新します。rollback時は以前の検証済みURLへ戻し、lockfileから依存を再導入してください。\n\n" +
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
