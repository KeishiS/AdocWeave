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

export function toTxtAST(source, projection) {
  const positions = createPositionMapper(source);

  function content(node) {
    return node.contentRange ? positions.base(node.contentRange).raw : "";
  }

  function parent(type, node, children, extra = {}) {
    return { type, ...positions.base(node.sourceRange), ...extra, children };
  }

  function text(type, node, value = content(node)) {
    return { type, ...positions.base(node.sourceRange), value };
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
        return [parent("Link", node, node.children.flatMap(phrasing), { url: "" })];
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
            depth: Math.min(6, Math.max(1, node.level ?? 1))
          })
        ];
      case "paragraph":
        return [...prefix, parent("Paragraph", node, bodyChildren.flatMap(phrasing))];
      case "list": {
        const items = bodyChildren
          .filter((child) => child.kind === "list-item")
          .flatMap(convertBlock);
        return [...prefix, parent("List", node, items, { ordered: null })];
      }
      case "list-item": {
        const blocks = [];
        let pending = [];
        const flush = () => {
          if (pending.length > 0) {
            const range = [pending[0].sourceRange[0], pending.at(-1).sourceRange[1]];
            blocks.push({
              type: "Paragraph",
              ...positions.base(range),
              children: pending.flatMap(phrasing)
            });
            pending = [];
          }
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
        return [
          ...prefix,
          parent("BlockQuote", node, bodyChildren.flatMap(convertBlock))
        ];
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
      case "code-block":
        return [...prefix, text("CodeBlock", node)];
      case "comment":
        return [paragraphFor(node, phrasing(node))];
      case "excluded":
        return prefix;
      case "block-title":
      case "description-term":
        return [paragraphFor(node)];
      default:
        if (phrasingKinds.has(node.kind)) {
          return [paragraphFor(node, phrasing(node))];
        }
        return bodyChildren.flatMap(convertBlock);
    }
  }

  return {
    type: "Document",
    ...positions.base(projection.sourceRange),
    children: projection.children.flatMap(convertBlock)
  };
}
