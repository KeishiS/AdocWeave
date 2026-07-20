# AsciiLoom

AsciiLoom is an incremental AsciiDoc processor written in Rust.

## Development

Enter the Nix development environment to obtain the Rust toolchain and `cargo-make`:

```console
nix develop
```

The following tasks are available:

- `cargo make format`: format Rust source files;
- `cargo make format-check`: check formatting without modifying files;
- `cargo make check`: type-check all targets and features;
- `cargo make clippy`: run Clippy with warnings denied;
- `cargo make test`: run the workspace tests;
- `cargo make build`: build all targets and features;
- `cargo make verify` or `cargo make`: run every non-mutating validation task.
