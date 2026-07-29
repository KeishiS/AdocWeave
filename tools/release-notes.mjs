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
  "WASMの要求、前処理、応答およびエラーのwire型を公開schemaから生成し、wire値の検査・変換とコア処理の境界を明確にしました。",
  "filesystemの列挙と読込をhostへ集約し、Workspaceは検証済みの論理ID、本文、snapshotおよび依存関係だけを扱うようにしました。",
  "project設定のresource上限を、filesystem読込、Workspaceが保持するdisk・overlay、および解析snapshotへ適用する一つの解決済みplanへ統合しました。",
  "前処理と解析で共有する文書外属性と属性展開上限を一つの検証済み処理契約へ統合しました。",
  "前処理、解析、Lintおよび生成元への位置投影へ協調キャンセルを伝播し、取り消し後の部分結果を返さない入口を追加しました。",
  "公開Rust APIとWASM protocol schemaに互換性へ影響する変更があります。次の移行手順を確認してください。",
];

const contractNotes = [
  `統一package version：${RELEASE_NOTES_VERSION}`,
  `release manifest schema version：${manifest.schemaVersion}、distribution plan schema version：${plan.schemaVersion}、配布manifest schema version：2。`,
  `WASM protocol schema version：${RELEASE_NOTES_PROTOCOL_SCHEMA_VERSION}、Worker protocol version：${protocol.workerProtocolVersion}。前処理設定へdefault付きの属性展開上限を追加しました。worker envelopeは変更していません。`,
  "`WasmPreprocessResponse::package_version`と`WasmSourceMapSegment::mapping`のRust型、前処理とWorkspaceの公開設定・エラー型、filesystem読込の公開境界、およびresource上限の公開型を変更しました。",
  "`RetainedResourceBudget`と`Workspace::try_snapshot_resources`を追加しました。host adapterは前者でproject単位のdisk・overlay課金をtransactionalに検査し、後者でhost固有の上限を検査しながら許可したresourceとrootだけを複製して解析snapshotを構築できます。",
  "CLI引数およびHTML契約はv0.18.0から変更していません。",
  "GitHub Release以外のregistryへpackageまたは拡張を公開しません。",
];

const migrationNotes = [
  "`WasmPreprocessResponse::package_version`は`&'static str`から所有値の`String`へ変わりました。文字列として借用する場合は`response.package_version.as_str()`を使用し、値を直接構築する場合は`VERSION.to_owned()`などの所有値を渡してください。",
  "`WasmSourceMapSegment::mapping`は`String`から`WasmSourceMapping`へ変わりました。`\"identity\".to_owned()`と`\"whole-origin\".to_owned()`は、それぞれ`WasmSourceMapping::Identity`と`WasmSourceMapping::WholeOrigin`へ置き換えてください。",
  "`PreprocessOptions`と`WasmPreprocessOptions`へ`max_attribute_expansion_depth`と`max_attribute_expansion_bytes`を追加しました。Rustの構造体を直接構築する場合は両fieldを指定するか、型に応じて`..PreprocessOptions::default()`または`..WasmPreprocessOptions::default()`を使用してください。",
  "前処理と解析を一度に行うRustコードは、同じ文書外属性と属性展開上限を持つ`AnalysisOptions`と`PreprocessOptions`から`EffectiveProcessingOptions::new`を呼び、`preprocess_and_analyze_with_options`または`WorkspaceSnapshot::analyze_with_options`へ渡してください。従来の入口は不一致を`PreprocessedAnalysisError::Options`または`WorkspaceErrorCode::InvalidOptions`として拒否します。",
  "`adocweave-workspace`の`scan_filesystem`、`scan_filesystem_with_session`、`FilesystemResource`および`WorkspaceErrorCode::Filesystem`を削除しました。`LocalFilesystemSession::scan_utf8`でhost側から読込み、検証済みの論理IDと本文を`Workspace::upsert_disk`へ渡してください。読込エラーはWorkspaceへ渡す前にhostの`ResourceError`として処理してください。",
  "filesystemの再読込とWorkspace更新を一つのtransactionとして扱うhost adapterでは、`LocalFilesystemSession::reread_utf8_with_rollback`が返す`FilesystemReadRollback`を、後続更新が失敗した場合だけ同じsessionの`rollback_reread`へ渡してください。tokenは発行元sessionと直後のgenerationにだけ有効です。`FilesystemReadRollbackResult::Stale`の場合は別の課金を変更していないため、処理全体を失敗として扱い、古いtokenを再利用しないでください。削除を反映する場合は`release`で読込課金を解放してください。",
  "`adocweave_host::DependencyGraph`の公開を終了しました。依存関係はWorkspaceが所有するため、解析後は`WorkspaceAnalysis::dependencies`を参照してください。",
  "`adocweave_host::ResourceLimits`はfilesystem読込専用の`FilesystemReadLimits`へ、`adocweave_workspace::ResourceLimits`はdisk・overlay保持専用の`RetainedResourceLimits`へ置き換えました。以前の型名をimportするコードは、上限を適用する段階に応じた型名へ変更してください。",
  "`ResourceSettings::limits`は`ResourceSettings::limit_plan`へ変わりました。host adapterは`ResolvedResourceLimitPlan::filesystem_reads`、Workspaceへの取込みは`retained_layers`、解析対象の選択は`analysis_snapshot`を使用してください。`.adocweave.toml`の`resources.max-files`、`max-total-bytes`および`max-resource-bytes`の項目名は変更していません。",
  "resource上限は段階ごとに独立して検査します。特に、保持上限は同じresourceのdiskとoverlayを別々に加算し、解析snapshot上限は一つのrootから参照できる有効resourceだけを数えます。以前は通過した入力でも新しい保持上限またはsnapshot上限を超える場合があるため、上限エラー時は値を無条件に引き上げず、開いているoverlayとinclude範囲を確認してください。",
  "WASM protocol schema 7では`preprocess.options`へ`maxAttributeExpansionDepth`と`maxAttributeExpansionBytes`を追加しました。省略時は従来と同じ32と1048576を使用します。combined requestで`analysisOptions.syntax.limits`へ非既定値を指定する場合は、前処理側にも同じ値を指定してください。不一致は処理前に`invalid-options`として拒否されます。",
  "`PreprocessedAnalysisError`へ`Options`と`Cancelled`を追加しました。この列挙型を網羅的に`match`するRustコードは、設定不一致と処理の取り消しを扱う両方の分岐を追加してください。`WorkspaceErrorCode`を網羅的に扱う場合は、削除した`Filesystem`を除き、新しい`InvalidOptions`を追加してください。協調キャンセルが必要な場合は`preprocess_cancellable`、`preprocess_and_analyze_cancellable_with_options`、`lint_analysis_cancellable`または`PreprocessedAnalysis::project_origins_cancellable`を使用してください。",
  "LSPでproject設定が不正になった場合は、旧設定で構築したhover、semantic tokenおよびdiagnosticを失効させ、`workspace-input-error`を返します。有効な設定へ厳格化した結果open overlayが上限を超えた場合も、disk本文へ切り替えません。一時的な設定読込失敗では、直前に検証済みのsnapshotとplanを維持します。",
  "JSONの`packageVersion`は文字列のままです。source mapの`mapping`も`identity`または`whole-origin`の文字列を維持します。",
  `CLI、LSP、browser、ZedおよびVS Code向け配布物のversionを${RELEASE_NOTES_VERSION}へそろえてください。`,
];

const knownConstraints = [
  `対応Rust toolchain：${manifest.rustVersion}。このreleaseのflake.lockで固定しています。`,
  "native binaryは配布計画に定義したLinux、macOSおよびWindows環境へ提供します。macOSとWindowsのbinaryはOSのsystem libraryへ動的linkします。",
  "macOS binaryへDeveloper ID署名とnotarizationを行わず、Windows binaryへAuthenticode署名を行いません。OSの警告が表示された場合はchecksumとattestationを確認してください。",
  "Zed拡張はdevelopment extension、VS Code拡張はVSIXとして手動導入します。拡張registryへは公開しません。",
  "公式Playgroundはこのreleaseに含みません。`adocweave preview`は利用者の端末で実行するローカル機能です。",
  "packageはcrates.io、npmまたはOS package registryへ公開しません。Nix packageはこのrepositoryのflakeから直接buildします。",
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
