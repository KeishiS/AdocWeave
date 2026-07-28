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
  "CLIとLanguage Serverが、厳密なschema version付きの`.adocweave.toml`を共通のプロジェクト設定として読み込むようになりました。",
  "`adocweave check`へCI用の失敗閾値、安定したJSON・GitHub Actions・SARIF出力、重要度別summaryを追加しました。",
  "runtimeに依存しない`adocweave-workspace` crateを追加し、上限付きdisk resource、editor overlay、immutable snapshot、依存関係追跡および古い解析結果の拒否を提供します。",
  "CLIが複数file、directoryおよびglobを決定的な順序で処理します。整形と一部の自動修正では、同じdirectory内で検証付きの原子的置換を利用できます。",
  "Bash、Zsh、FishおよびPowerShellの補完scriptをCLIから生成できます。",
];

const contractNotes = [
  `統一package version：${manifest.packageVersion}`,
  `WASM protocol schema version：${protocol.schemaVersion}、Worker protocol version：${protocol.workerProtocolVersion}。古いrequestとWorker envelopeは拒否されます。`,
  "プロジェクト設定schema version 1は、不明なfieldと未対応のschema versionを拒否します。設定によってfilesystemまたはnetworkへの権限が付与されることはありません。",
  "`adocweave check`の既定値は`--fail-on error`です。coreのerror診断でもprocessが失敗するようになりました。より厳格なgateには`--fail-on warning`、報告だけを行う場合は`--fail-on never`を使用してください。",
  "human、JSON、GitHub ActionsおよびSARIFの各形式では、診断集合、順序および終了状態が一致します。`--summary`は件数をstderrへ出力します。",
  "`format --write`と`check --fix`は置換前にすべての入力を検査し、symlinkと同時変更を拒否します。明示的に設定しない限り、modeと既存の改行規則を維持します。",
  "`check --fix`は`Applicability::Always`の修正だけを適用し、重複するeditを拒否します。",
  "Rust workspace APIでは型付きresource ID、revisionおよびgenerationを使用します。完了した解析結果は、現在のworkspace状態に対してacceptする必要があります。",
  "`ResourceDocument.source`を`String`から`Arc<str>`へ変更しました。呼出側では共有されたimmutableなsource storageを使用してください。",
  "新しい`adocweave-config`と`adocweave-workspace` crateはrepository内専用であり、crates.ioへは公開しません。",
];

const knownConstraints = [
  `対応Rust toolchain：${manifest.rustVersion}。このreleaseのflake.lockで固定しています。`,
  "native binaryの対応環境はLinux x86-64とARM64だけです。",
  "既定では、相対linkはhostが有効なURL policyで許可されたURLへ解決するまでHTMLで無効です。hostは文書に記述された相対URLを明示的に許可できます。",
  "local targetの検証では、一つのcommandの実行中にfilesystemが変化しないことを仮定します。symlinkの同時置換に対する強化はIssue #56で追跡しています。",
  "HTML5検証は標準適合性を確認しますが、生成したmarkupを信頼済みDOMにするものではありません。",
  "Zed拡張はdevelopment extensionとして導入します。Zed Extension Galleryへは公開しません。",
  "packageはcrates.io、npmまたはOS package registryへ公開しません。Nix packageはこのrepositoryのflakeから直接buildします。",
];

function markdownList(items) {
  return items.map((item) => `- ${item}`).join("\n");
}

export function buildReleaseNotes(tag) {
  if (tag !== `v${manifest.packageVersion}`) throw new Error("Release Notesのtagがpackage versionと一致しません");
  const targets = plan.targets.map((target) => `- Linux ${target}`).join("\n");
  const notes = `## 主な変更\n\n${markdownList(highlights)}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[0]}\n\n${targets}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[1]}\n\n${markdownList(contractNotes)}\n\n` +
    "consumerは記載されたpackage versionを厳密に一致させる必要があります。異なるversionのCLI、LSP、browserまたはZed向け配布物を混在させないでください。hostは`resourceQueries`を要求し、取得に成功した各resourceを具体的なMIME typeで解決し、文書のrevisionごとに`RenderInputs`を再構築する必要があります。\n\n" +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[2]}\n\n${markdownList(knownConstraints)}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[3]}\n\n` +
    "すべてのrelease assetをdownloadし、`sha256sum --check sha256.sum`を実行してください。その後、必要なassetを`gh attestation verify <asset> --repo KeishiS/AdocWeave`で検証してください。\n\n" +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[4]}\n\n` +
    "更新前に既存のCIを確認してください。coreのerror診断が既定の終了状態へ影響するようになりました。従来の報告のみの動作を一時的に維持するには`adocweave check --fail-on never`を使用し、その後プロジェクトの閾値を明示してください。\n\n" +
    "共通policyが必要な場合だけプロジェクト設定を作成してください。schema versionは必須です。\n\n" +
    "```toml\nschema-version = 1\n\n[lint]\nmax-diagnostics = 1000\n```\n\n" +
    "CIではv0.13.0のCLIを固定し、機械可読なstdoutとsummaryを分離してください。\n\n" +
    "```sh\nadocweave check --fail-on warning --format json --summary docs\n```\n\n" +
    "preprocess snapshotを構築するRust consumerは、所有するtextを`Arc<str>`へ変換してください。複数文書を管理するconsumerはresourceとoverlayの状態を`adocweave-workspace`へ移行できます。単一文書向けの`Engine`は引き続き利用できます。\n\n" +
    "version別directoryへ導入し、検証後にだけ`current` symlinkを切り替えてください。受入確認が成功するまで以前のversionを残し、rollback時はそのsymlinkを元へ戻してください。詳細は`docs/user-guide/release-installation.adoc`を参照してください。\n";
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
