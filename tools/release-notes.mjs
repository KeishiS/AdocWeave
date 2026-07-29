import { readFileSync } from "node:fs";
import process from "node:process";

const ROOT = new URL("../", import.meta.url);
const manifest = JSON.parse(readFileSync(new URL("release-manifest.json", ROOT), "utf8"));
const plan = JSON.parse(readFileSync(new URL("release/distribution-plan.json", ROOT), "utf8"));
const protocol = JSON.parse(readFileSync(new URL("protocol/public-api.json", ROOT), "utf8"));
export const RELEASE_NOTES_VERSION = "0.17.0";
export const RELEASE_NOTES_PROTOCOL_SCHEMA_VERSION = 6;

export const REQUIRED_RELEASE_NOTE_HEADINGS = [
  "## 対応環境",
  "## 公開契約と破壊的変更",
  "## 既知の制約",
  "## 配布物の検証",
  "## 更新とロールバック",
];

const highlights = [
  "#85：`adocweave preview`を追加しました。起点文書、includeで読み込んだ文書、まだ存在しないinclude対象およびCSSを監視し、変更後のHTMLと診断をWebブラウザーへ反映します。",
  "連続した変更をまとめて処理し、完了したうち最も新しい更新だけを表示します。監視対象は解析で見つかった依存関係に限定し、ファイルシステム全体は探索しません。",
  "既定では同じ端末からだけ接続できる`127.0.0.1:4000`で待ち受けます。ループバック以外のIPアドレスを使うには`--allow-external`が必要です。",
];

const contractNotes = [
  `統一package version：${RELEASE_NOTES_VERSION}`,
  `release manifest schema version：${manifest.schemaVersion}、distribution plan schema version：${plan.schemaVersion}、配布manifest schema version：2。v0.16.0からschema形状を変更していません。`,
  `WASM protocol schema version：${RELEASE_NOTES_PROTOCOL_SCHEMA_VERSION}、Worker protocol version：${protocol.workerProtocolVersion}。WASM protocolは未選択のprojectionをnullで表す契約などを反映してschema 5から6へ破壊的に更新し、Worker envelopeはversion 2を維持します。`,
  "`preview`は新しいCLIコマンドです。既存のCLIコマンドとWorker protocolはv0.16.0から維持します。Browser APIとWASM responseには次の移行が必要です。",
  "プレビューのHTMLは既存の変換処理と同じ安全性方針で生成します。任意のファイルやディレクトリ一覧を配信せず、配信するURLを表示画面、生成文書、更新番号、診断および固定のクライアントスクリプトに限定します。",
  "`adocweave-host`のfilesystem読込APIを、検証後のpathを後から開き直す方式から、rootのhandleを基準に検証と読込を一体で行うsession方式へ変更しました。これはRust APIの破壊的変更です。",
  "`LocalFilesystemSession`が全rootで共有する`ResourceBudget`を所有します。同じcanonical pathを再読込するとfile数を維持したままbyte数の差分を反映し、上限超過時は以前の計上値を維持します。workspaceから除いたpathは`session.release(path)`で計上を解放し、現在値は`session.budget()`で確認できます。",
  "filesystem errorは`ResourceError`の`Missing`、`PermissionDenied`、`PathNotAbsolute`、`OutsideRoots`、`NotRegularFile`および`Unverifiable`などで分類します。表示文字列ではなくvariantを処理してください。",
  "複数の許可rootを設定していても、読込は入力pathに対応する1つのroot handleへ限定します。symlinkの参照先が別の許可root内にある場合も、選択したrootの境界を越えるため`OutsideRoots`として拒否します。",
  "Browser clientの`onResult`と`onError`で発生した例外は、解析Promiseの完了状態とWorkerの生存状態へ影響させません。callback内の例外を記録または表示する必要がある場合は、利用側で処理してください。",
  "WASM productを選択しなかった場合もresponse fieldは省略しません。`projection`は`null`、配列で表すproductは空配列、文字列で表すproductは空文字列を返します。",
  "公式Playgroundはこのリリースに含みません。`preview`は利用者の端末で実行するローカル機能です。",
  "GitHub Release以外のregistryへpackageまたは拡張を公開しません。",
];

const rustApiMigration = `### \`adocweave-host\` Rust APIの移行

| v0.16まで | v0.17 |
| --- | --- |
| \`LocalResourcePolicy::new(...)\` | \`LocalFilesystemPolicy::new(...)\`の後に\`policy.session()\` |
| \`policy.validate_file(&mut budget, path)\`と\`ValidatedFilesystemTarget::into_loaded_utf8()\` | \`session.read_utf8(LogicalSourceId::new(...)?, absolute_path)\` |
| \`LoadedLocalResource\` | \`LoadedFilesystemSource\`。論理上のsource識別子は\`source_id()\`、実体の由来は\`canonical_path()\`で取得 |
| \`normalize_relative(target)\`または\`policy.resolve_relative(base, target)\`による解決だけの操作 | 解決専用の公開APIは廃止。\`session.read_target_utf8(LogicalSourceId::new(...)?, absolute_base, target)\`で解決、読込、UTF-8検証および予算計上を一体で実行 |
| 呼出側が所有する\`ResourceBudget\` | sessionが所有する共有budget。現在値は\`session.budget()\`で参照 |
| workspace scan後に別のpolicyで再読込 | \`scan_filesystem_with_session(&mut session)\`を使用し、scanと後続の再読込で同じsessionとbudgetを共有 |

\`read_utf8\`と\`reread_utf8\`へ渡すpath、および\`read_target_utf8\`へ渡すbaseは絶対pathにしてください。診断やsource mapへ公開する名前はfilesystem pathから暗黙に作らず、制御文字を含まない\`LogicalSourceId\`として明示します。再読込には\`reread_utf8\`を使用し、監視対象から削除したpathには\`release\`を呼び出してください。`;

const browserApiMigration = `### Browser APIとWASM responseの移行

| v0.16まで | v0.17 |
| --- | --- |
| \`AdocWeaveResult.result\`内の\`AdocWeaveWasmResponse\` | \`AdocWeaveResult\`自体がWASM responseを表すflatな結果 |
| \`result.result.projection\`など | \`result.projection\`など、すべてのWASM productを結果直下から参照 |
| nested WASM responseの\`version\`とcallback adapterの\`sourceVersion\` | WASM wireでは\`version\`を維持。Browserのflatな結果では同じ値を\`sourceVersion\`として公開 |
| 投影済み参照の\`notices: ReferenceNotice[]\`と値\`fallback\` | \`notices: ProjectedReferenceNotice[]\`と値\`reference-resolution-fallback\`。入力側の\`ReferenceNotice\`と区別 |

\`result.html\`、\`result.diagnostics\`および\`result.renderDiagnostics\`など、従来から結果直下にあった主なfieldは同じ名前で利用できます。互換aliasの\`AdocWeaveWorkerClient\`も維持します。schema versionを検査する処理を6へ更新し、生成済みのbindings、型定義およびfixtureをschema 6から再生成してください。`;

const knownConstraints = [
  `対応Rust toolchain：${manifest.rustVersion}。このreleaseのflake.lockで固定しています。`,
  "native binaryは配布計画に定義したLinux、macOSおよびWindows環境へ提供します。macOSとWindowsのbinaryはOSのsystem libraryへ動的linkします。",
  "macOS binaryへDeveloper ID署名とnotarizationを行わず、Windows binaryへAuthenticode署名を行いません。OSの警告が表示された場合はchecksumとattestationを確認してください。",
  "Zed拡張はdevelopment extension、VS Code拡張はVSIXとして手動導入します。拡張registryへは公開しません。",
  "Zed extension APIではhost OS versionを取得できないため、macOSとWindowsの最小versionはdownload前ではなく、配布binaryのdeployment targetをOS loaderが強制します。",
  "プレビューサーバーは利用者認証とTLSによる通信の暗号化を提供しません。ループバック以外で待ち受ける場合は、信頼できないネットワークへ直接公開しないでください。",
  "プレビューは200ミリ秒ごとにファイルの情報を確認し、内容の長さと更新時刻が同じ変更も2秒ごとのハッシュ値確認で検出します。変更検出後に`--debounce-ms`の待ち時間を適用します。",
  "プレビューの停止は生成処理の段階間で協調的に行います。停止通知に対応しない処理段階は完了まで待ちます。",
  "プレビューのHTTP処理には同時実行数、待機数、request headerの長さおよび通信時間の上限があります。接続数が上限に達した場合は、新しい接続を応答せずに閉じます。",
  "packageはcrates.io、npmまたはOS package registryへ公開しません。Nix packageはこのrepositoryのflakeから直接buildします。",
];

function markdownList(items) {
  return items.map((item) => `- ${item}`).join("\n");
}

export function buildReleaseNotes(tag) {
  if (tag !== `v${RELEASE_NOTES_VERSION}`) {
    throw new Error(`Release Notesはv${RELEASE_NOTES_VERSION}専用です`);
  }
  const osNames = { darwin: "macOS", linux: "Linux", win32: "Windows" };
  const targets = plan.targets
    .map((target) => `- ${osNames[target.os]} ${target.architecture}（\`${target.triple}\`）`)
    .join("\n");
  const notes = `## 主な変更\n\n${markdownList(highlights)}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[0]}\n\n${targets}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[1]}\n\n${markdownList(contractNotes)}\n\n` +
    `${rustApiMigration}\n\n` +
    `${browserApiMigration}\n\n` +
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
