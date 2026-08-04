import assert from "node:assert/strict";
import test from "node:test";

import {
  classifyTrackedFiles,
  createRepositoryRules,
  validateTargetConfiguration
} from "./repository-lint-config.mjs";

const targets = {
  schemaVersion: 1,
  authoredFiles: ["README.adoc"],
  authoredDirectories: ["docs/"],
  excludedDirectories: [{ path: "fixtures/", reason: "試験入力であるため" }]
};

const terminology = {
  schemaVersion: 1,
  forbiddenTerms: [
    {
      id: "sample",
      term: "禁止語",
      match: "substring",
      message: "別の表現を検討してください。",
      documentation: "docs/developer-guide/terminology.adoc#sample"
    }
  ]
};

test("追跡文書を執筆対象、除外対象、未分類へ分ける", () => {
  const result = classifyTrackedFiles(targets, [
    "README.adoc",
    "docs/guide.adoc",
    "fixtures/input.adoc",
    "notes.adoc"
  ]);
  assert.deepEqual(result, {
    authored: ["README.adoc", "docs/guide.adoc"],
    excluded: ["fixtures/input.adoc"],
    unknown: ["notes.adoc"]
  });
});

test("除外ディレクトリと似た接頭辞を持つ文書を除外しない", () => {
  const result = classifyTrackedFiles(targets, ["fixtures-other/input.adoc"]);
  assert.deepEqual(result.excluded, []);
  assert.deepEqual(result.unknown, ["fixtures-other/input.adoc"]);
});

test("設定値を検証済みのコピーとして返す", () => {
  const mutable = structuredClone(targets);
  const validated = validateTargetConfiguration(mutable);
  mutable.authoredFiles[0] = "changed.adoc";
  mutable.excludedDirectories[0].reason = "変更後";

  assert.deepEqual(validated.authoredFiles, ["README.adoc"]);
  assert.equal(validated.excludedDirectories[0].reason, "試験入力であるため");
  assert.ok(Object.isFrozen(validated));
});

test("壊れた対象設定を分類前に拒否する", () => {
  const cases = [
    [{ ...targets, schemaVersion: 2 }, /schemaVersion/],
    [{ ...targets, authoredFiles: "README.adoc" }, /authoredFilesは配列/],
    [{ ...targets, authoredDirectories: ["docs"] }, /末尾には\/が必要/],
    [
      { ...targets, excludedDirectories: [{ path: "fixtures/", reason: "" }] },
      /reasonは空でない文字列/
    ],
    [
      {
        ...targets,
        authoredDirectories: ["docs/"],
        excludedDirectories: [{ path: "docs/generated/", reason: "生成物であるため" }]
      },
      /執筆対象と除外対象の範囲が重複しています/
    ],
    [{ ...targets, unexpected: true }, /項目がschemaと一致しません/]
  ];
  for (const [configuration, expected] of cases) {
    assert.throws(() => classifyTrackedFiles(configuration, []), expected);
  }
});

test("リポジトリ用規則を安定した順序で組み立てる", () => {
  const rules = createRepositoryRules(terminology);
  assert.deepEqual(
    rules.map(({ ruleId }) => ruleId),
    [
      "no-mix-dearu-desumasu",
      "no-double-negative-ja",
      "no-dropping-the-ra",
      "no-nfd",
      "no-hankaku-kana",
      "no-invalid-control-character",
      "no-unmatched-pair",
      "no-zero-width-spaces",
      "adocweave-terminology"
    ]
  );
  assert.deepEqual(rules[0].options, {
    preferInHeader: "",
    preferInBody: "ですます",
    preferInList: "ですます",
    strict: false
  });
});
