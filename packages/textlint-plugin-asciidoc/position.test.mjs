import assert from "node:assert/strict";
import test from "node:test";

import { createPositionMapper } from "./position.mjs";

test("UTF-8 byte範囲をUTF-16位置へ変換する", () => {
  const mapper = createPositionMapper("a😀日\r\n次");
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

test("不正なbyte範囲と入力外の位置を拒否する", () => {
  const mapper = createPositionMapper("😀");
  assert.throws(() => mapper.range([1, 4]), /UTF-8文字の途中/);
  assert.throws(() => mapper.range([4, 1]), /不正なUTF-8 byte範囲/);
  assert.throws(() => mapper.position(3), /入力外/);
});
