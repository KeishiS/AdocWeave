# Core 0.1 compatibility

The 0.1 core profile supports:

- paragraphs, document titles, and section headings;
- constrained monospace, strong, and emphasis inline spans;
- delimited literal blocks;
- `[source, LANGUAGE]` source blocks;
- HTML conversion, lint diagnostics, conservative formatting, document
  symbols, and the initial Language Server features.

The following AsciiDoc features are intentionally outside this profile:

- document and block attributes other than the source-block language;
- explicit anchors and cross references;
- links and URL policy;
- lists and block continuations;
- note-reference macros and host-side UUID resolution;
- STEM and LaTeX math;
- raw HTML passthrough.

Unsupported constructs are preserved as explicit `Unsupported` semantic nodes
in permissive mode and rendered as escaped text. Strict processing rejects
documents containing such nodes. No unsupported construct is guessed or
silently interpreted.

AsciiLoom is not yet a complete AsciiDoc implementation and the 0.1 core
profile does not claim compatibility with a specific note application.
