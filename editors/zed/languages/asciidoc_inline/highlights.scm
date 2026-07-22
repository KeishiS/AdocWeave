[
  (monospace)
  (passthrough)
] @text.literal

(emphasis) @emphasis.strong
(italic) @emphasis
(highlight) @emphasis
(superscript) @emphasis
(subscript) @emphasis

[
  (link_url)
  (email)
] @link_uri

(uri_label) @link_text

[
  "["
  "]"
  "{"
  "}"
  "<<"
  ">>"
] @punctuation.bracket

":" @punctuation.delimiter
(replacement) @string.special
(roled_text (role) @attribute)
(attribute_reference (attribute_name) @constant)

(xref (reftext) @link_uri)
(xref (id) @link_uri .)
(xref
  (id) @link_text
  (reftext) @link_uri)

[
  (macro_name)
  "((("
  ")))"
  "(("
  "))"
] @keyword

(inline_macro (target) @label)
(inline_macro (attr) @attribute)
(escaped_sequence) @string.escape

(inline_macro
  (target)? @link_uri
  (attr)? @label)

(stem_macro
  (target)? @label
  (attr)? @text.literal)

(footnote
  (target)? @label
  (attr) @attribute)

(named_attr (attribute_value) @string)
(term) @attribute
(id_assignment) @label
(super_escape) @string.special
(hard_wrap) @punctuation.special
