import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import {
  canonicalReleaseIntent,
  prepareReleaseIntent,
  validateReleaseIntent,
} from "./release-intent.mjs";

const ready = () => ({
  schemaVersion: 1,
  version: "1.2.3",
  state: "ready",
  generation: 4,
});

test("schemaとruntime validatorの公開契約が一致する", () => {
  const schema = JSON.parse(readFileSync(new URL("../release/intent.schema.json", import.meta.url)));
  assert.equal(schema.additionalProperties, false);
  assert.deepEqual(schema.required, ["schemaVersion", "version", "state", "generation"]);
  assert.deepEqual(schema.properties.schemaVersion, { const: 1 });
  assert.equal(schema.properties.version.pattern, "^(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)$");
  assert.deepEqual(schema.properties.state, { enum: ["preparing", "ready"] });
  assert.deepEqual(schema.properties.generation, {
    type: "integer",
    minimum: 1,
    maximum: Number.MAX_SAFE_INTEGER,
  });
});

test("preparingとreadyのrelease intentを厳密に検査する", () => {
  assert.doesNotThrow(() => validateReleaseIntent(ready(), "1.2.3", { requireReady: true }));
  const preparing = { ...ready(), state: "preparing" };
  assert.doesNotThrow(() => validateReleaseIntent(preparing, "1.2.3"));
  assert.throws(
    () => validateReleaseIntent(preparing, "1.2.3", { requireReady: true }),
    /readyではありません/,
  );
});

test("未知field、不正なversion、stateおよびgenerationを拒否する", () => {
  for (const [intent, pattern] of [
    [{ ...ready(), unknown: true }, /未知のfield/],
    [{ ...ready(), schemaVersion: 2 }, /schemaVersion/],
    [{ ...ready(), version: "v1.2.3" }, /stable SemVer/],
    [{ ...ready(), state: "published" }, /preparingまたはready/],
    [{ ...ready(), generation: 0 }, /1以上/],
    [{ ...ready(), generation: Number.MAX_SAFE_INTEGER + 1 }, /安全な整数/],
  ]) {
    assert.throws(() => validateReleaseIntent(intent, "1.2.3"), pattern);
  }
  assert.throws(() => validateReleaseIntent(ready(), "1.2.4"), /一致しません/);
});

test("version更新はintentをpreparingへ戻してgenerationを増やす", () => {
  const prepared = prepareReleaseIntent(ready(), "1.2.3", "1.3.0");
  assert.deepEqual(prepared, {
    schemaVersion: 1,
    version: "1.3.0",
    state: "preparing",
    generation: 5,
  });
  assert.equal(
    canonicalReleaseIntent(prepared),
    '{\n  "schemaVersion": 1,\n  "version": "1.3.0",\n  "state": "preparing",\n  "generation": 5\n}\n',
  );
  assert.throws(
    () => prepareReleaseIntent({ ...ready(), generation: Number.MAX_SAFE_INTEGER }, "1.2.3", "1.3.0"),
    /generationが上限/,
  );
});
