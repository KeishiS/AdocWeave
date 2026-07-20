# Core 0.1 normative grammar

This document defines the accepted language of the AsciiLoom 0.1 core
profile. “Must” statements are normative. Byte ranges are UTF-8, zero-based,
half-open ranges.

## Lexical model

An input is valid UTF-8. A line is:

```ebnf
line        = content, [ line-ending ] ;
line-ending = LF | CRLF ;
```

A lone CR is content. The lossless layer must preserve every byte and the
original line ending. Block markers are recognized only at column zero unless
stated otherwise.

## Document and blocks

```ebnf
document       = { blank-line | heading | source-block | literal-block
                 | unsupported | paragraph } ;
blank-line     = { " " | TAB } ;
paragraph      = paragraph-line, { paragraph-line } ;
paragraph-line = nonblank content not beginning another recognized block ;

heading        = heading-marker, [ " " ], heading-text ;
heading-marker = "=", { "=" } ;

literal-block  = "....", line-ending,
                 { literal-line },
                 "....", [ line-ending ] ;

source-block   = source-attribute, line-ending,
                 "----", line-ending,
                 { source-line },
                 "----", [ line-ending ] ;
source-attribute = "[source]"
                 | "[source,", horizontal-space*, language,
                   horizontal-space*, "]" ;
language       = nonempty text excluding "," and "]" ;
```

The first one-marker heading before document content is a document title.
Markers of length two through six are section levels one through five.
Other placements or depths remain heading nodes with problems attached, so a
consumer can recover without discarding source.

A source attribute must be immediately followed by an exact `----` line.
There may be no blank line between them. Indented attributes and delimiters are
not recognized. Other attribute lines are `Unsupported`.

Literal and source content is opaque. A line equal to the opening delimiter
closes the block; a longer or indented delimiter-like line is content.

## Inline syntax

```ebnf
inline-sequence = { text | monospace | strong | emphasis } ;
monospace       = "`", literal-content, "`" ;
strong          = "*", inline-sequence, "*" ;
emphasis        = "_", inline-sequence, "_" ;
```

The EBNF is constrained by the following delimiter predicates:

- An opener is valid when its next scalar exists, is neither whitespace nor
  the marker, and its previous scalar is absent or non-alphanumeric.
- A closer is valid when its previous scalar exists, is neither whitespace nor
  the marker, and its next scalar is absent or non-alphanumeric.
- Scanning chooses the earliest valid opener. Its first valid same-marker
  closer is used. Ties therefore follow source order.
- Monospace content is opaque. Strong and emphasis content is parsed
  recursively.
- The default recursive depth limit is 32. A span at the limit becomes text
  and reports `nesting-limit-exceeded`.

Only constrained forms are supported. Unconstrained double-marker forms have
no special meaning. Backslash is ordinary text; the 0.1 profile defines no
escape sequence.

## Recovery

- An unclosed inline opener remains text, reports `unclosed-inline`, and
  scanning resumes after that marker so later safe spans remain available.
- Inline scanning never crosses a paragraph line or heading boundary.
- An unclosed delimited block consumes through end of file, except that an
  exact column-zero heading begins recovery. The heading is parsed normally
  and the opener reports `unclosed-block`.
- Unsupported syntax becomes an explicit `Unsupported` node in permissive
  mode. Strict processing rejects a document containing such a node.
- Empty input, incomplete input, and malformed headings must still yield a
  recoverable CST/AST without panic.

## AST traceability

| Grammar rule | Semantic node |
| --- | --- |
| `heading` | `AstBlock::Heading` |
| `paragraph` | `AstBlock::Paragraph` |
| `literal-block` | `AstBlock::Literal` |
| `source-block` | `AstBlock::Source` |
| `unsupported` | `AstBlock::Unsupported` |
| ordinary inline text | `Inline::Text` |
| `monospace` | `Inline::Literal { kind: Monospace }` |
| `strong` / `emphasis` | `Inline::Styled` |

The fixture `fixtures/grammar/ambiguous.adoc` is the normative ambiguous and
recovery example. Its assertions live in the tests named `grammar`.
