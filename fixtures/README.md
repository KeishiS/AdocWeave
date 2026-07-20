# Fixtures

Fixtures are grouped by the feature that owns them. Each feature directory may contain:

- `*.adoc`: source documents;
- `*.html`: expected HTML fragments;
- `*.diagnostics.json`: expected diagnostics;
- `*.formatted.adoc`: expected formatter output.

Keep each fixture focused on one behavior. Use descriptive names and add separate files for valid, invalid, incomplete, and boundary inputs.
