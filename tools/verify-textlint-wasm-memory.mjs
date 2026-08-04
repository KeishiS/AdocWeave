import { readFileSync } from "node:fs";
import process from "node:process";
import { pathToFileURL } from "node:url";

export const WASM_PAGE_BYTES = 64 * 1024;

function fail(message) {
  throw new Error(message);
}

function readU32(bytes, cursor) {
  let value = 0;
  for (let shift = 0; shift < 35; shift += 7) {
    if (cursor.offset >= bytes.length) fail("WebAssemblyのLEB128整数が途中で終わっています");
    const byte = bytes[cursor.offset++];
    value |= (byte & 0x7f) << shift;
    if ((byte & 0x80) === 0) return value >>> 0;
  }
  fail("WebAssemblyのLEB128整数が長すぎます");
}

export function memoryLimits(moduleBytes) {
  const bytes = new Uint8Array(moduleBytes);
  if (
    bytes.length < 8 ||
    ![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00].every(
      (byte, index) => bytes[index] === byte,
    )
  ) {
    fail("有効なWebAssembly moduleではありません");
  }
  const cursor = { offset: 8 };
  const memories = [];
  while (cursor.offset < bytes.length) {
    const sectionId = bytes[cursor.offset++];
    const sectionSize = readU32(bytes, cursor);
    const sectionEnd = cursor.offset + sectionSize;
    if (sectionEnd > bytes.length) fail("WebAssembly sectionが途中で終わっています");
    if (sectionId === 5) {
      const count = readU32(bytes, cursor);
      for (let index = 0; index < count; index += 1) {
        const flags = readU32(bytes, cursor);
        if ((flags & ~0x03) !== 0) fail("未対応のWebAssembly memory形式です");
        const minimumPages = readU32(bytes, cursor);
        const maximumPages = (flags & 0x01) === 0 ? undefined : readU32(bytes, cursor);
        memories.push({ maximumPages, minimumPages });
      }
      if (cursor.offset !== sectionEnd) fail("WebAssembly memory sectionに余分なbyteがあります");
    }
    cursor.offset = sectionEnd;
  }
  if (memories.length !== 1) fail(`WebAssembly memoryは1件必要です：${memories.length}件`);
  return memories[0];
}

export function verifyMemoryMaximum(moduleBytes, expectedMaximumBytes) {
  if (
    !Number.isSafeInteger(expectedMaximumBytes) ||
    expectedMaximumBytes <= 0 ||
    expectedMaximumBytes % WASM_PAGE_BYTES !== 0
  ) {
    fail("memory上限は64 KiB単位の正の整数で指定してください");
  }
  const limits = memoryLimits(moduleBytes);
  const expectedPages = expectedMaximumBytes / WASM_PAGE_BYTES;
  if (limits.maximumPages !== expectedPages) {
    fail(
      `WebAssembly memory上限が一致しません：期待${expectedPages} page、` +
        `実際${limits.maximumPages ?? "上限なし"}`,
    );
  }
  if (limits.minimumPages > limits.maximumPages) {
    fail("WebAssembly memoryの初期値が上限を超えています");
  }
  return limits;
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  const [path, maximum] = process.argv.slice(2);
  if (!path || !maximum) {
    process.stderr.write("usage: node tools/verify-textlint-wasm-memory.mjs WASM MAXIMUM_BYTES\n");
    process.exit(2);
  }
  try {
    const limits = verifyMemoryMaximum(readFileSync(path), Number(maximum));
    process.stdout.write(
      `textlint WebAssembly memory上限を検査しました：${limits.maximumPages} page\n`,
    );
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
