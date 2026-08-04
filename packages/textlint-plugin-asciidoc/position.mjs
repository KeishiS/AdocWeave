const encoder = new TextEncoder();

export function createPositionMapper(source) {
  const byteToUtf16 = new Map([[0, 0]]);
  let byteOffset = 0;
  let utf16Offset = 0;
  for (const character of source) {
    byteOffset += encoder.encode(character).length;
    utf16Offset += character.length;
    byteToUtf16.set(byteOffset, utf16Offset);
  }

  const lineStarts = [0];
  for (let index = 0; index < source.length; index += 1) {
    if (source[index] === "\r") {
      if (source[index + 1] === "\n") index += 1;
      lineStarts.push(index + 1);
    } else if (source[index] === "\n") {
      lineStarts.push(index + 1);
    }
  }

  function utf16(byte) {
    const offset = byteToUtf16.get(byte);
    if (offset === undefined) {
      throw new Error(`UTF-8文字の途中または入力外を指す範囲です: ${byte}`);
    }
    return offset;
  }

  function position(offset) {
    if (!Number.isSafeInteger(offset) || offset < 0 || offset > source.length) {
      throw new Error(`JavaScript文字列の入力外を指す位置です: ${offset}`);
    }
    let low = 0;
    let high = lineStarts.length;
    while (low + 1 < high) {
      const middle = Math.floor((low + high) / 2);
      if (lineStarts[middle] <= offset) low = middle;
      else high = middle;
    }
    return { line: low + 1, column: offset - lineStarts[low] };
  }

  function range(byteRange) {
    if (
      !Array.isArray(byteRange) ||
      byteRange.length !== 2 ||
      !Number.isSafeInteger(byteRange[0]) ||
      !Number.isSafeInteger(byteRange[1]) ||
      byteRange[0] < 0 ||
      byteRange[0] > byteRange[1]
    ) {
      throw new Error(`不正なUTF-8 byte範囲です: ${JSON.stringify(byteRange)}`);
    }
    return [utf16(byteRange[0]), utf16(byteRange[1])];
  }

  function base(byteRange) {
    const mapped = range(byteRange);
    return {
      raw: source.slice(mapped[0], mapped[1]),
      range: mapped,
      loc: { start: position(mapped[0]), end: position(mapped[1]) }
    };
  }

  return { base, position, range, utf16 };
}
