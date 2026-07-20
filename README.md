# AsciiLoom

AsciiLoom is an incremental AsciiDoc processor written in Rust.

## Development

Enter the Nix development environment to obtain the Rust toolchain and `cargo-make`:

```console
nix develop
```

The following tasks are available:

- `cargo make fmt` or `cargo make format`: format Rust source files;
- `cargo make fmt-check` or `cargo make format-check`: check formatting without modifying files;
- `cargo make check`: type-check all targets and features;
- `cargo make clippy`: run Clippy with warnings denied;
- `cargo make test`: run the workspace tests;
- `cargo make test-core`: run core and CLI tests;
- `cargo make test-lsp`: run Language Server tests;
- `cargo make doc-check`: build API documentation with warnings denied;
- `cargo make build`: build all targets and features;
- `cargo make run-lsp`: run the Language Server over standard input/output;
- `cargo make verify`, `cargo make ci`, or `cargo make`: run every non-mutating validation task.
