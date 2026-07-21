{
  description = "AdocWeave development environment";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.rust-overlay = {
    url = "github:oxalica/rust-overlay";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    { nixpkgs, rust-overlay, ... }:
    let
      supportedSystems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
    in
    {
      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };
          fuzzRust = pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.default);
          adocweave-fuzz = pkgs.writeShellScriptBin "adocweave-fuzz" ''
            export PATH=${fuzzRust}/bin:${pkgs.cargo-fuzz}/bin:$PATH
            exec cargo fuzz "$@"
          '';
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              actionlint
              cargo
              cargo-dist
              cargo-make
              chromium
              dejavu_fonts
              esbuild
              fontconfig
              gh
              clippy
              gnutar
              lld
              nodejs
              typescript
              ripgrep
              rust-analyzer
              rustc
              rustfmt
              stdenv.cc
              wasm-bindgen-cli
              xz
              adocweave-fuzz
            ];

            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          };
        }
      );
    };
}
