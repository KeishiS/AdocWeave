(document_title) @title
(title1) @title
(title2) @title
(title3) @title
(title4) @title
(title5) @title

(email) @link_uri

(author_line
  ";" @punctuation.delimiter)

(revision_line
  ["," ":"] @punctuation.delimiter)

(list_continuation) @constant

[
  (firstname)
  (middlename)
  (lastname)
] @attribute

(revnumber) @number
(revdate) @string.special
(revremark) @string

[
  (table_block_marker)
  (csv_table_block_marker)
  (dsv_table_block_marker)
] @punctuation.special

(table_cell_attr) @attribute
(table_cell "|" @punctuation.special)
(csv_record "," @punctuation.special)
(dsv_record ":" @punctuation.special)

[
  (breaks)
  (hard_wrap)
  (quoted_block_md_marker)
  (quoted_paragraph_marker)
  (open_block_marker)
  (listing_block_start_marker)
  (listing_block_end_marker)
  (literal_block_marker)
  (passthrough_block_marker)
  (quoted_block_start_marker)
  (quoted_block_end_marker)
  (sidebar_block_start_marker)
  (sidebar_block_end_marker)
  (ntable_block_marker)
  (callout_marker)
] @punctuation.special

(ntable_cell "!" @punctuation.special)
(checked_list_marker_unchecked) @punctuation.list_marker
(checked_list_marker_checked) @punctuation.list_marker

[
  (list_marker_star)
  (list_marker_hyphen)
  (list_marker_dot)
  (list_marker_digit)
  (list_marker_geek)
  (list_marker_alpha)
] @punctuation.list_marker

(description_marker) @punctuation.list_marker
(description_list_item (term) @emphasis.strong)

[
  (line_comment)
  (block_comment)
] @comment

[
  (document_attr_marker)
  (element_attr_marker)
] @punctuation.delimiter

(document_attr (attr_name) @property)
(block_style) @type
(positional_attr) @attribute
(id) @label
(role) @attribute
(option) @attribute

(block_title
  (block_title_marker) @punctuation.special) @attribute

(ident_block) @text.literal
(callout_list_marker) @punctuation.special

(block_macro
  (block_macro_name) @keyword
  "::" @punctuation.delimiter
  (target)? @link_uri
  "[" @punctuation.bracket
  "]" @punctuation.bracket)

(attribute_name) @attribute
(attribute_value) @variable.parameter

(admonition
  (admonition_important) @comment
  ":" @comment)

(admonition
  (admonition_warning) @comment
  ":" @comment)

(admonition
  (admonition_caution) @comment
  ":" @comment)

(admonition
  (admonition_note) @comment
  ":" @comment)

(admonition
  (admonition_tip) @comment
  ":" @comment)

((section_block
  (element_attr
    (positional_attr (block_style) @_style))
  (listing_block
    (listing_block_body) @text.literal))
  (#not-any-of? @_style
    "a2s" "barcode" "blockdiag" "bpmn" "bytefield" "d2" "dbml" "diagrams" "ditaa" "dpic" "erd"
    "gnuplot" "graphviz" "lilypond" "meme" "mermaid" "msc" "nomnoml" "pikchr" "plantuml" "shaape"
    "smcat" "structurizr" "svgbob" "symbolator" "syntrax" "tikz" "umlet" "vega" "wavedrom"))
