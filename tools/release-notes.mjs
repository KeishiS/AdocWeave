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
  "#108：ソースブロックのタイトル、言語、`linenums`および開始行を解析結果へ保持し、安全なHTML表示契約を追加しました。",
  "#109：インライン数式とブロック数式の表示形式、言語、未加工sourceおよび入力範囲を公開し、HTMLへ描画adapter向け属性を追加しました。",
  "#111：解析結果、HTML、解析診断および描画診断を対応付ける公開適合性fixtureを追加しました。",
  "#110：`rendering_features()`で数式言語、正規化済みソース言語および実際の目次出力の有無を決定的に取得できるようにしました。",
  "#81：Browser clientへPromise形式の`analyze()`と`analyzeOnce()`、初期化状態、型付きlifecycle errorおよびbundler向けasset URL契約を追加しました。",
];

const contractNotes = [
  `統一package version：${manifest.packageVersion}`,
  `release manifest schema version：${manifest.schemaVersion}、distribution plan schema version：${plan.schemaVersion}、配布manifest schema version：2。これらのschema形状はv0.15.0から変更していません。`,
  `WASM protocol schema version：${protocol.schemaVersion}、Worker protocol version：${protocol.workerProtocolVersion}。WASM protocolはschema 4から5へ更新し、Worker envelopeはversion 2を維持します。`,
  "schema 5では`SourceBlockProjection`へ必須fieldの`title`、`lineNumbers`および`startLine`を追加しました。schema 4の保存済みprojectionを読むconsumerは、schema versionを検査して型とfixtureを更新してください。",
  "ソースブロックは必要な場合だけ`figure.source-block`と`figcaption`を生成し、`pre`の`data-language`、`data-line-numbers`および`data-line-start`で表示情報を公開します。タイトルと行番号指定がない従来の`pre > code`構造は維持します。",
  "数式HTMLは`data-math-language=\"latexmath\"`と`data-math-display=\"inline|block\"`を公開します。JSONとWASMの言語値は互換性のため`latex`を維持します。",
  "`rendering_features()`は追加描画が必要な数式言語、正規化済みソース言語および空でない目次の有無だけを返します。renderer、theme、JavaScript libraryまたはasset URLは選択しません。",
  "Browser clientの従来の`update()`、`onResult`および`onError`は維持します。新しい`ready`、`analyze()`および`analyzeOnce()`は、cancel、dispose、世代の上書き、package不一致およびWorker障害を型付きerrorとしてrejectします。",
  "`defaultAssetUrls()`はmodule URLを基準にWorkerとWASMを解決します。bundler利用時もWorkerとWASMを別assetとして配備し、JavaScript bundleへinline化しないでください。",
  "公開適合性fixtureの安定契約はmanifestの`stableContract`に列挙したJSON pointer、HTML断片および診断codeです。期待出力file全体の空白、key順および属性順はconsumer向け契約ではありません。",
  "HTMLは入力由来のraw HTML、任意属性、event handler、`script`およびSVGを生成しません。構文強調、行番号の見た目、数式engine、themeおよび操作buttonは利用側が安全なadapterとして提供します。",
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
