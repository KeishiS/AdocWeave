import assert from "node:assert/strict";
import test from "node:test";

import {
  WASM_PAGE_BYTES,
  memoryLimits,
  verifyMemoryMaximum,
} from "./verify-textlint-wasm-memory.mjs";

function u32(value) {
  const bytes = [];
  do {
    let byte = value & 0x7f;
    value >>>= 7;
    if (value !== 0) byte |= 0x80;
    bytes.push(byte);
  } while (value !== 0);
  return bytes;
}

function moduleWithMemory(minimum, maximum) {
  const limits = maximum === undefined
    ? [0x00, ...u32(minimum)]
    : [0x01, ...u32(minimum), ...u32(maximum)];
  const payload = [0x01, ...limits];
  return Uint8Array.from([
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
    0x05, ...u32(payload.length), ...payload,
  ]);
}

test("WebAssembly memoryの初期値と最大値を読み取る", () => {
  assert.deepEqual(memoryLimits(moduleWithMemory(17, 4096)), {
    maximumPages: 4096,
    minimumPages: 17,
  });
});

test("上限なし、異なる上限および不正moduleを拒否する", () => {
  assert.throws(
    () => verifyMemoryMaximum(moduleWithMemory(1), 4096 * WASM_PAGE_BYTES),
    /上限なし/,
  );
  assert.throws(
    () => verifyMemoryMaximum(moduleWithMemory(1, 2), 3 * WASM_PAGE_BYTES),
    /一致しません/,
  );
  assert.throws(() => memoryLimits(new Uint8Array()), /有効なWebAssembly/);
});
