((block_macro
  (block_macro_name)
  (target) @injection.content)
  (#set! injection.language "AsciiDoc Inline"))

((table_cell
  (table_cell_content) @injection.content)
  (#set! injection.language "AsciiDoc Inline"))

((paragraph) @injection.content
  (#set! injection.include-children)
  (#set! injection.language "AsciiDoc Inline"))

((line) @injection.content
  (#set! injection.include-children)
  (#set! injection.language "AsciiDoc Inline"))

((section_block
  (element_attr
    (positional_attr (block_style))
    (positional_attr) @injection.language)
  (listing_block
    (listing_block_body) @injection.content)))

((section_block
  (element_attr
    (positional_attr (block_style) @injection.language))
  (listing_block
    (listing_block_body) @injection.content))
  (#any-of? @injection.language
    "a2s" "barcode" "blockdiag" "bpmn" "bytefield" "d2" "dbml" "diagrams" "ditaa" "dpic" "erd"
    "gnuplot" "graphviz" "lilypond" "meme" "mermaid" "msc" "nomnoml" "pikchr" "plantuml" "shaape"
    "smcat" "structurizr" "svgbob" "symbolator" "syntrax" "tikz" "umlet" "vega" "wavedrom"))

((section_block
  (element_attr
    (positional_attr (block_style))
    (positional_attr) @injection.language)
  (paragraph) @injection.content)
  (#set! injection.include-children))
