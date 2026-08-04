export function createPositionMapper(source) {
  let lineCount = 1;
  for (let index = 0; index < source.length; index += 1) {
    const character = source[index];
    if (character === "\r") {
      if (source[index + 1] === "\n") index += 1;
      lineCount += 1;
    } else if (character === "\n" || character === "\u2028" || character === "\u2029") {
      lineCount += 1;
    }
  }
  const lineStarts = new Uint32Array(lineCount);
  let line = 1;
  for (let index = 0; index < source.length; index += 1) {
    const character = source[index];
    if (character === "\r") {
      if (source[index + 1] === "\n") index += 1;
      lineStarts[line] = index + 1;
      line += 1;
    } else if (character === "\n" || character === "\u2028" || character === "\u2029") {
      lineStarts[line] = index + 1;
      line += 1;
    }
  }

  function assertRange(range, label = "range") {
    const splitsSurrogatePair = (offset) =>
      offset > 0 &&
      offset < source.length &&
      /[\uD800-\uDBFF]/u.test(source[offset - 1]) &&
      /[\uDC00-\uDFFF]/u.test(source[offset]);
    if (
      !Array.isArray(range) ||
      range.length !== 2 ||
      !range.every(Number.isSafeInteger) ||
      range[0] < 0 ||
      range[0] > range[1] ||
      range[1] > source.length ||
      splitsSurrogatePair(range[0]) ||
      splitsSurrogatePair(range[1])
    ) {
      throw new Error(`${label}が不正です: ${JSON.stringify(range)}`);
    }
    return range;
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

  function location(range) {
    const valid = assertRange(range);
    return { start: position(valid[0]), end: position(valid[1]) };
  }

  function base(range) {
    const valid = assertRange(range);
    return {
      raw: source.slice(valid[0], valid[1]),
      range: [...valid],
      loc: location(valid)
    };
  }

  return { assertRange, base, location, position };
}
