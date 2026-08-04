import { createPositionMapper } from "./position.mjs";

const phrasingKinds = new Set([
  "text",
  "code",
  "strong",
  "emphasis",
  "link",
  "reference",
  "hard-break",
  "comment",
  "container"
]);

function required(node, property, predicate) {
  const value = node[property];
  if (!predicate(value)) {
    throw new Error(`${node.kind}に有効な${property}がありません。`);
  }
  return value;
}

function assertAstInvariants(source, root, positions) {
  const visit = (node, parent) => {
    if (
      !Array.isArray(node.range) ||
      node.range.length !== 2 ||
      !node.range.every(Number.isSafeInteger) ||
      node.range[0] < 0 ||
      node.range[0] > node.range[1] ||
      node.range[1] > source.length
    ) {
      throw new Error(`${node.type}に不正なrangeがあります。`);
    }
    if (node.raw !== source.slice(node.range[0], node.range[1])) {
      throw new Error(`${node.type}のrawとrangeが一致しません。`);
    }
    const expectedLocation = {
      start: positions.position(node.range[0]),
      end: positions.position(node.range[1])
    };
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
        if (node.children[index - 1].range[0] > node.children[index].range[0]) {
          throw new Error(`${node.type}のchildrenが原文順ではありません。`);
        }
      }
      for (const child of node.children) visit(child, node);
    }
  };
  visit(root, null);
}

export function toTxtAST(source, projection) {
  const positions = createPositionMapper(source);

  function content(node) {
    if (!node.contentRange) return "";
    const sourceRange = positions.range(node.sourceRange);
    const contentRange = positions.range(node.contentRange);
    if (contentRange[0] < sourceRange[0] || contentRange[1] > sourceRange[1]) {
      throw new Error(`${node.kind}のcontentRangeがsourceRangeに含まれていません。`);
    }
    return source.slice(contentRange[0], contentRange[1]);
  }

  function parent(type, node, children, extra = {}) {
    return { type, ...positions.base(node.sourceRange), ...extra, children };
  }

  function text(type, node, value = content(node), extra = {}) {
    return { type, ...positions.base(node.sourceRange), ...extra, value };
  }

  function phrasing(node) {
    switch (node.kind) {
      case "text":
        return [text("Str", node, positions.base(node.sourceRange).raw)];
      case "code":
        return [text("Code", node)];
      case "strong":
        return [parent("Strong", node, node.children.flatMap(phrasing))];
      case "emphasis":
        return [parent("Emphasis", node, node.children.flatMap(phrasing))];
      case "link":
      case "reference":
        return [
          parent("Link", node, node.children.flatMap(phrasing), {
            url: required(node, "url", (value) => typeof value === "string")
          })
        ];
      case "hard-break":
        return [{ type: "Break", ...positions.base(node.sourceRange) }];
      case "comment":
        return [text("Comment", node)];
      case "container":
      case "block-title":
      case "description-term":
        return node.children.flatMap(phrasing);
      case "excluded":
      case "code-block":
        return [];
      default:
        return node.children.flatMap(phrasing);
    }
  }

  function paragraphFor(node, children = node.children.flatMap(phrasing)) {
    return parent("Paragraph", node, children);
  }

  function convertBlock(node) {
    const title = node.children.find((child) => child.kind === "block-title");
    const bodyChildren = node.children.filter((child) => child !== title);
    const prefix = title ? [paragraphFor(title)] : [];
    switch (node.kind) {
      case "heading":
        return [
          ...prefix,
          parent("Header", node, bodyChildren.flatMap(phrasing), {
            depth: required(
              node,
              "level",
              (value) => Number.isSafeInteger(value) && value >= 1 && value <= 6
            )
          })
        ];
      case "paragraph":
        return [...prefix, parent("Paragraph", node, bodyChildren.flatMap(phrasing))];
      case "list":
        return [
          ...prefix,
          parent(
            "List",
            node,
            bodyChildren.filter((child) => child.kind === "list-item").flatMap(convertBlock),
            { ordered: required(node, "ordered", (value) => typeof value === "boolean") }
          )
        ];
      case "list-item": {
        const blocks = [];
        let pending = [];
        const flush = () => {
          if (pending.length === 0) return;
          const range = [pending[0].sourceRange[0], pending.at(-1).sourceRange[1]];
          blocks.push({
            type: "Paragraph",
            ...positions.base(range),
            children: pending.flatMap(phrasing)
          });
          pending = [];
        };
        for (const child of bodyChildren) {
          if (phrasingKinds.has(child.kind) || child.kind === "description-term") {
            pending.push(child);
          } else {
            flush();
            blocks.push(...convertBlock(child));
          }
        }
        flush();
        return [parent("ListItem", node, blocks)];
      }
      case "block-quote":
      case "container":
        return [...prefix, parent("BlockQuote", node, bodyChildren.flatMap(convertBlock))];
      case "table":
        return [
          ...prefix,
          parent(
            "Table",
            node,
            bodyChildren.filter((child) => child.kind === "table-row").flatMap(convertBlock)
          )
        ];
      case "table-row":
        return [
          parent(
            "TableRow",
            node,
            bodyChildren.filter((child) => child.kind === "table-cell").flatMap(convertBlock)
          )
        ];
      case "table-cell":
        return [parent("TableCell", node, bodyChildren.flatMap(phrasing))];
      case "code-block": {
        required(node, "contentRange", (value) => Array.isArray(value) && value.length === 2);
        const language = required(
          node,
          "language",
          (value) => value === null || typeof value === "string"
        );
        return [...prefix, text("CodeBlock", node, content(node), { lang: language })];
      }
      case "comment":
        return [paragraphFor(node, phrasing(node))];
      case "excluded":
        return prefix;
      case "block-title":
      case "description-term":
        return [paragraphFor(node)];
      default:
        if (phrasingKinds.has(node.kind)) return [paragraphFor(node, phrasing(node))];
        return bodyChildren.flatMap(convertBlock);
    }
  }

  const documentRange = positions.range(projection.sourceRange);
  if (documentRange[0] !== 0 || documentRange[1] !== source.length) {
    throw new Error("DocumentのsourceRangeが入力全体を指していません。");
  }
  const root = {
    type: "Document",
    ...positions.base(projection.sourceRange),
    children: projection.children.flatMap(convertBlock)
  };
  assertAstInvariants(source, root, positions);
  return root;
}
