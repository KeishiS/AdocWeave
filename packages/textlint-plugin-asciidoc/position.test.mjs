import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import test from "node:test";

import { createPositionMapper } from "./position.mjs";

test("UTF-16 rangeから位置と原文を生成する", () => {
  const source = "a😀日\r\n次";
  const mapper = createPositionMapper(source);
  assert.deepEqual(mapper.base([1, 3]), {
    raw: "😀",
    range: [1, 3],
    loc: {
      start: { line: 1, column: 1 },
      end: { line: 1, column: 3 }
    }
  });
  assert.deepEqual(mapper.location([6, 7]), {
    start: { line: 2, column: 0 },
    end: { line: 2, column: 1 }
  });
});

test("CR、LF、CRLF、U+2028およびU+2029を改行として扱う", () => {
  const source = "a\rb\nc\r\nd\u2028e\u2029f";
  const mapper = createPositionMapper(source);
  for (const [character, line] of [["b", 2], ["c", 3], ["d", 4], ["e", 5], ["f", 6]]) {
    assert.deepEqual(mapper.position(source.indexOf(character)), { line, column: 0 });
  }
});

test("不正なrangeと入力外の位置を拒否する", () => {
  const mapper = createPositionMapper("😀");
  assert.throws(() => mapper.assertRange([-1, 1]), /不正/);
  assert.throws(() => mapper.assertRange([2, 1]), /不正/);
  assert.throws(() => mapper.assertRange([0, 3]), /不正/);
  assert.throws(() => mapper.assertRange([0, 1]), /不正/);
  assert.throws(() => mapper.position(3), /入力外/);
});

test("10 MiBの全改行とASCII長行を低いheap上限で処理する", () => {
  const moduleUrl = new URL("./position.mjs", import.meta.url).href;
  const script = `
    import { createPositionMapper } from ${JSON.stringify(moduleUrl)};
    const size = 10 * 1024 * 1024;
    const newlines = "\\n".repeat(size);
    const newlineMapper = createPositionMapper(newlines);
    if (newlineMapper.position(size).line !== size + 1) process.exit(2);
    const line = "a".repeat(size);
    const lineMapper = createPositionMapper(line);
    if (lineMapper.position(size).column !== size) process.exit(3);
  `;
  const result = spawnSync(
    process.execPath,
    ["--max-old-space-size=32", "--input-type=module", "--eval", script],
    { encoding: "utf8" }
  );
  assert.equal(result.status, 0, result.stderr);
});
