import { createPositionMapper } from "./position.mjs";

function requiredType(node) {
  if (typeof node?.type !== "string" || node.type.length === 0) {
    throw new Error("TxtAST planに有効なtypeがありません。");
  }
  return node.type;
}

function assertAstInvariants(source, root, positions) {
  const visit = (node, parent) => {
    positions.assertRange(node.range, `${node.type}のrange`);
    if (node.raw !== source.slice(node.range[0], node.range[1])) {
      throw new Error(`${node.type}のrawとrangeが一致しません。`);
    }
    const expectedLocation = positions.location(node.range);
    if (JSON.stringify(node.loc) !== JSON.stringify(expectedLocation)) {
      throw new Error(`${node.type}のlocとrangeが一致しません。`);
    }
    if (parent && (node.range[0] < parent.range[0] || node.range[1] > parent.range[1])) {
      throw new Error(`${node.type}のrangeが親nodeのrangeに含まれていません。`);
    }
    if (node.children !== undefined) {
      if (!Array.isArray(node.children)) {
        throw new Error(`${node.type}のchildrenが配列ではありません。`);
      }
      for (let index = 1; index < node.children.length; index += 1) {
        if (node.children[index - 1].range[1] > node.children[index].range[0]) {
          throw new Error(`${node.type}のchildrenが原文順ではないか重複しています。`);
        }
      }
      for (const child of node.children) visit(child, node);
    }
  };
  visit(root, null);
}

export function materializeTxtAST(source, plan) {
  if (typeof source !== "string") {
    throw new TypeError("AsciiDocの入力は文字列で指定してください。");
  }
  const positions = createPositionMapper(source);

  function materialize(node) {
    if (node === null || typeof node !== "object" || Array.isArray(node)) {
      throw new Error("TxtAST planのnodeがobjectではありません。");
    }
    const type = requiredType(node);
    const range = positions.assertRange(node.range, `${type}のrange`);
    const {
      type: _type,
      range: _range,
      valueRange,
      children,
      raw: _raw,
      value: _value,
      loc: _loc,
      ...properties
    } = node;
    const result = { type, ...positions.base(range), ...properties };

    if (valueRange !== undefined) {
      const value = positions.assertRange(valueRange, `${type}のvalueRange`);
      if (value[0] < range[0] || value[1] > range[1]) {
        throw new Error(`${type}のvalueRangeがrangeに含まれていません。`);
      }
      result.value = source.slice(value[0], value[1]);
    }
    if (children !== undefined) {
      if (!Array.isArray(children)) {
        throw new Error(`${type}のchildrenが配列ではありません。`);
      }
      result.children = children.map(materialize);
    }
    return result;
  }

  const root = materialize(plan);
  if (root.type !== "Document" || !Array.isArray(root.children)) {
    throw new Error("TxtAST planのrootはchildrenを持つDocumentではありません。");
  }
  if (root.range[0] !== 0 || root.range[1] !== source.length) {
    throw new Error("Documentのrangeが入力全体を指していません。");
  }
  assertAstInvariants(source, root, positions);
  return root;
}
