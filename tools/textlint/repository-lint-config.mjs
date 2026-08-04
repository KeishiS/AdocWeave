import technicalWriting from "textlint-rule-preset-ja-technical-writing";

import { createTerminologyRule } from "./terminology-rule.mjs";

const targetKeys = [
  "authoredDirectories",
  "authoredFiles",
  "excludedDirectories",
  "schemaVersion"
];
const excludedDirectoryKeys = ["path", "reason"];
const selectedTechnicalWritingRules = [
  "no-mix-dearu-desumasu",
  "no-double-negative-ja",
  "no-dropping-the-ra",
  "no-nfd",
  "no-hankaku-kana",
  "no-invalid-control-character",
  "no-unmatched-pair",
  "no-zero-width-spaces"
];

function assertRecord(value, name) {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${name}はオブジェクトで指定してください。`);
  }
}

function assertExactKeys(value, expected, name) {
  const actual = Object.keys(value).sort();
  if (actual.length !== expected.length || actual.some((key, index) => key !== expected[index])) {
    throw new Error(`${name}の項目がschemaと一致しません: ${actual.join(", ")}`);
  }
}

function validatePath(path, name, directory) {
  if (typeof path !== "string" || path.length === 0) {
    throw new Error(`${name}は空でない文字列で指定してください。`);
  }
  const pathWithoutTrailingSlash = directory ? path.slice(0, -1) : path;
  if (
    path.startsWith("/") ||
    path.includes("\\") ||
    path.includes("\0") ||
    pathWithoutTrailingSlash
      .split("/")
      .some((part) => part.length === 0 || part === "." || part === "..")
  ) {
    throw new Error(`${name}はリポジトリルートからの相対パスで指定してください: ${path}`);
  }
  if (directory !== path.endsWith("/")) {
    throw new Error(
      directory
        ? `${name}の末尾には/が必要です: ${path}`
        : `${name}にディレクトリは指定できません: ${path}`
    );
  }
  return path;
}

function validatePathList(value, name, directory) {
  if (!Array.isArray(value)) {
    throw new Error(`${name}は配列で指定してください。`);
  }
  const paths = value.map((path, index) => validatePath(path, `${name}[${index}]`, directory));
  if (new Set(paths).size !== paths.length) {
    throw new Error(`${name}に重複したパスがあります。`);
  }
  return Object.freeze(paths);
}

function assertUnambiguousTargets(authoredFiles, authoredDirectories, excludedDirectories) {
  const redundantAuthoredFile = authoredFiles.find((file) =>
    authoredDirectories.some((directory) => file.startsWith(directory))
  );
  if (redundantAuthoredFile !== undefined) {
    throw new Error(`authoredFilesとauthoredDirectoriesの指定が重複しています: ${redundantAuthoredFile}`);
  }

  const directoryGroups = [
    ["authoredDirectories", authoredDirectories],
    [
      "excludedDirectories",
      excludedDirectories.map(({ path }) => path)
    ]
  ];
  for (const [name, directories] of directoryGroups) {
    const overlap = directories.find((directory, index) =>
      directories.some(
        (other, otherIndex) => index !== otherIndex && directory.startsWith(other)
      )
    );
    if (overlap !== undefined) {
      throw new Error(`${name}に範囲が重複するディレクトリがあります: ${overlap}`);
    }
  }

  const excludedPaths = excludedDirectories.map(({ path }) => path);
  const conflictingFile = authoredFiles.find((file) =>
    excludedPaths.some((directory) => file.startsWith(directory))
  );
  const conflictingDirectory = authoredDirectories.find((authored) =>
    excludedPaths.some(
      (excluded) => authored.startsWith(excluded) || excluded.startsWith(authored)
    )
  );
  if (conflictingFile !== undefined || conflictingDirectory !== undefined) {
    throw new Error(
      `執筆対象と除外対象の範囲が重複しています: ${conflictingFile ?? conflictingDirectory}`
    );
  }
}

export function validateTargetConfiguration(value) {
  assertRecord(value, "文書対象一覧");
  assertExactKeys(value, targetKeys, "文書対象一覧");
  if (value.schemaVersion !== 1) {
    throw new Error("文書対象一覧のschemaVersionを解釈できません。");
  }

  const authoredFiles = validatePathList(value.authoredFiles, "authoredFiles", false);
  const authoredDirectories = validatePathList(
    value.authoredDirectories,
    "authoredDirectories",
    true
  );
  if (!Array.isArray(value.excludedDirectories)) {
    throw new Error("excludedDirectoriesは配列で指定してください。");
  }
  const excludedDirectories = value.excludedDirectories.map((entry, index) => {
    const name = `excludedDirectories[${index}]`;
    assertRecord(entry, name);
    assertExactKeys(entry, excludedDirectoryKeys, name);
    const path = validatePath(entry.path, `${name}.path`, true);
    if (typeof entry.reason !== "string" || entry.reason.trim().length === 0) {
      throw new Error(`${name}.reasonは空でない文字列で指定してください。`);
    }
    return Object.freeze({ path, reason: entry.reason });
  });
  const excludedPaths = excludedDirectories.map(({ path }) => path);
  if (new Set(excludedPaths).size !== excludedPaths.length) {
    throw new Error("excludedDirectoriesに重複したパスがあります。");
  }
  assertUnambiguousTargets(authoredFiles, authoredDirectories, excludedDirectories);

  return Object.freeze({
    schemaVersion: 1,
    authoredFiles,
    authoredDirectories,
    excludedDirectories: Object.freeze(excludedDirectories)
  });
}

export function classifyTrackedFiles(configuration, trackedFiles) {
  const targets = validateTargetConfiguration(configuration);
  if (!Array.isArray(trackedFiles) || trackedFiles.some((path) => typeof path !== "string")) {
    throw new Error("追跡文書一覧は文字列の配列で指定してください。");
  }

  const classified = { authored: [], excluded: [], unknown: [] };
  for (const path of trackedFiles) {
    if (
      targets.authoredFiles.includes(path) ||
      targets.authoredDirectories.some((directory) => path.startsWith(directory))
    ) {
      classified.authored.push(path);
    } else if (targets.excludedDirectories.some((entry) => path.startsWith(entry.path))) {
      classified.excluded.push(path);
    } else {
      classified.unknown.push(path);
    }
  }
  return Object.freeze({
    authored: Object.freeze(classified.authored),
    excluded: Object.freeze(classified.excluded),
    unknown: Object.freeze(classified.unknown)
  });
}

export function createRepositoryRules(terminologyCatalog) {
  const rules = selectedTechnicalWritingRules.map((ruleId) => {
    const rule = technicalWriting.rules[ruleId];
    if (rule === null || (typeof rule !== "function" && typeof rule !== "object")) {
      throw new Error(`日本語技術文書規則が見つかりません: ${ruleId}`);
    }
    return {
      ruleId,
      rule,
      options:
        ruleId === "no-mix-dearu-desumasu"
          ? { preferInHeader: "", preferInBody: "ですます", preferInList: "ですます", strict: false }
          : structuredClone(technicalWriting.rulesConfig[ruleId])
    };
  });
  rules.push({
    ruleId: "adocweave-terminology",
    rule: createTerminologyRule(terminologyCatalog)
  });
  return rules;
}
