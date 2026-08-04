import assert from "node:assert/strict";
import test from "node:test";

import { createPositionMapper } from "./position.mjs";

test("UTF-8 byte rangeをUTF-16位置へ変換する", () => {
  const source = "a😀日\r\n次";
  const mapper = createPositionMapper(source);
  assert.deepEqual(mapper.range([1, 5]), [1, 3]);
  assert.deepEqual(mapper.base([8, 10]).loc, {
    start: { line: 1, column: 4 },
    end: { line: 2, column: 0 }
  });
  assert.deepEqual(mapper.base([10, 13]).loc, {
    start: { line: 2, column: 0 },
    end: { line: 2, column: 1 }
  });
});

test("UTF-8文字の途中を指す範囲を拒否する", () => {
  const mapper = createPositionMapper("😀");
  assert.throws(() => mapper.range([1, 4]), /UTF-8文字の途中/);
});
