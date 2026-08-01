import assert from "node:assert/strict";
import { Buffer } from "node:buffer";
import { readFileSync } from "node:fs";
import test from "node:test";

import { validSha512Integrity } from "./verify-vscode-dependencies.mjs";

const lock = JSON.parse(readFileSync(new URL("../editors/vscode/package-lock.json", import.meta.url)));
const recorded = JSON.parse(
  readFileSync(new URL("../security/vscode-build-licenses.json", import.meta.url)),
);
const script = readFileSync(new URL("dependency-governance.sh", import.meta.url), "utf8");
const verifier = readFileSync(new URL("verify-vscode-dependencies.mjs", import.meta.url), "utf8");

const entries = Object.entries(lock.packages).filter(([path]) => path !== "");
const build = entries.filter(([, entry]) => entry.dev === true);
const shipped = entries.filter(([, entry]) => entry.dev !== true);

test("ビルド用依存も取得元とintegrityを満たす", () => {
  // 配布物へ同梱されなくても、これらは配布物を作る過程で実行されます。
  assert.notEqual(build.length, 0);
  const violations = build
    .filter(
      ([, entry]) =>
        typeof entry.integrity !== "string" ||
        !validSha512Integrity(entry.integrity) ||
        typeof entry.resolved !== "string" ||
        !entry.resolved.startsWith("https://registry.npmjs.org/"),
    )
    .map(([path]) => path);
  assert.deepEqual(violations, []);
});

test("integrityは正しいBase64の64 byte SHA-512 digestだけを受理する", () => {
  const digest = Buffer.alloc(64, 0xa5).toString("base64");
  assert.equal(validSha512Integrity(`sha512-${digest}`), true);

  const malformed = [
    "sha512-",
    "sha512-!!!!",
    `sha512-${Buffer.alloc(63, 0xa5).toString("base64")}`,
    `sha512-${digest.slice(0, -2)}`,
    `sha512-${digest.slice(0, -2)}..`,
    `sha256-${digest}`,
  ];
  for (const integrity of malformed) {
    assert.equal(validSha512Integrity(integrity), false, integrity);
  }
});

test("ビルド用依存のライセンス目録は実際と一致する", () => {
  const observed = [...new Set(build.map(([, entry]) => entry.license))].sort();
  assert.deepEqual([...recorded.licenses].sort(), observed);
});

test("配布時依存とビルド用依存は別のライセンス方針で扱う", () => {
  // 二つは満たすべき条件が異なります。同じ方針にすると、配布時の許可集合が
  // ビルド用にも及んで通らないか、ビルド用に合わせて配布時が緩みます。
  const shippedLicenses = new Set(shipped.map(([, entry]) => entry.license));
  const buildLicenses = new Set(build.map(([, entry]) => entry.license));
  assert.ok(
    [...buildLicenses].some((license) => !shippedLicenses.has(license)),
    "二つの方針を分ける必要が無いなら、この検査は不要です",
  );
});

test("監査は配布時とビルド用の両方を対象にする", () => {
  assert.match(script, /npm audit --omit=dev --prefix editors\/vscode/);
  // NODE_ENV=productionなどによりnpmの既定値がomit=devでも、全体監査では
  // ビルド用依存を戻します。
  assert.match(script, /^npm audit --include=dev --prefix editors\/vscode$/m);
});

test("検査はdev依存を読み飛ばさない", () => {
  // 以前はentry.dev === trueをcontinueで飛ばし、取得元もintegrityも見ていません
  // でした。飛ばす対象がライセンスの判定だけであることを固定します。
  assert.doesNotMatch(verifier, /if \(!path \|\| entry\.dev === true\) continue/);
  assert.match(verifier, /if \(!fetchedSafely\(entry\)\)/);
});
