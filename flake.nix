{
  description = "AdocWeave CLI, Language Server, and development environment";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.rust-overlay = {
    url = "github:oxalica/rust-overlay";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    { self, nixpkgs, rust-overlay, ... }:
    let
      supportedSystems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
      packageSystems = [
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllPackageSystems = nixpkgs.lib.genAttrs packageSystems;
      releaseManifest = builtins.fromJSON (builtins.readFile ./release-manifest.json);
      packageVersion = releaseManifest.packageVersion;
      rustVersion = releaseManifest.rustVersion;
      mkPkgs = system: import nixpkgs {
        inherit system;
        overlays = [ (import rust-overlay) ];
      };
      stableRust = pkgs: pkgs.rust-bin.stable.latest.default;
      developmentRust = pkgs: (stableRust pkgs).override {
        extensions = [
          "clippy"
          "rust-src"
          "rustfmt"
        ];
        targets = [
          "wasm32-unknown-unknown"
          "wasm32-wasip2"
        ];
      };
      rustPlatform = pkgs: pkgs.makeRustPlatform {
        cargo = stableRust pkgs;
        rustc = stableRust pkgs;
      };
      mkAdocWeave = pkgs:
        assert (stableRust pkgs).version == rustVersion;
        (rustPlatform pkgs).buildRustPackage {
        pname = "adocweave";
        version = packageVersion;
        src = self;
        cargoLock.lockFile = ./Cargo.lock;
        cargoBuildFlags = [
          "-p=adocweave-cli"
          "-p=adocweave-lsp"
        ];
        doCheck = false;
        strictDeps = true;
        installPhase = ''
          runHook preInstall
          releaseDir="target/${pkgs.stdenv.hostPlatform.rust.rustcTarget}/release"
          install -Dm755 "$releaseDir/adocweave" "$out/bin/adocweave"
          install -Dm755 "$releaseDir/adocweave-lsp" "$out/bin/adocweave-lsp"
          runHook postInstall
        '';
        meta = {
          description = "AsciiDoc converter and Language Server";
          homepage = "https://github.com/KeishiS/AdocWeave";
          license = with pkgs.lib.licenses; [ asl20 mit ];
          mainProgram = "adocweave";
          platforms = pkgs.lib.platforms.linux;
        };
      };
    in
    {
      overlays.default = final: _previous: {
        adocweave = mkAdocWeave final;
      };

      packages = forAllPackageSystems (
        system:
        let
          pkgs = (mkPkgs system).extend self.overlays.default;
          package = pkgs.adocweave;
        in
        {
          default = package;
          adocweave = package;
          adocweave-cli = package;
          adocweave-lsp = package;
        }
      );

      apps = forAllPackageSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/adocweave";
          meta.description = "Run the AdocWeave command-line converter";
        };
        adocweave-lsp = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/adocweave-lsp";
          meta.description = "Run the AdocWeave Language Server";
        };
      });

      checks = forAllPackageSystems (
        system:
        let
          pkgs = mkPkgs system;
          package = self.packages.${system}.default;
          runtimeClosure = pkgs.closureInfo {
            rootPaths = [ package ];
          };
          nixos = nixpkgs.lib.nixosSystem {
            inherit system;
            modules = [
              {
                environment.systemPackages = [ package ];
                system.stateVersion = "24.11";
              }
            ];
          };
        in
        {
          package = package;
          public-contract =
            assert self.packages.${system}.adocweave == package;
            assert self.packages.${system}.adocweave-cli == package;
            assert self.packages.${system}.adocweave-lsp == package;
            assert self.apps.${system}.default.program == "${package}/bin/adocweave";
            assert self.apps.${system}.adocweave-lsp.program == "${package}/bin/adocweave-lsp";
            assert builtins.isFunction self.overlays.default;
            pkgs.runCommand "adocweave-public-flake-contract" { } ''
              touch "$out"
            '';
          package-smoke = pkgs.runCommand "adocweave-package-smoke" {
            nativeBuildInputs = [ pkgs.jq ];
          } ''
            test "$(${package}/bin/adocweave --version --json | jq -r .packageVersion)" = "${packageVersion}"
            test "$(${package}/bin/adocweave-lsp --version --json | jq -r .packageVersion)" = "${packageVersion}"
            if grep -E '/[^/]*(chromium|nodejs|rustc|cargo)-' ${runtimeClosure}/store-paths; then
              echo "development or browser tool found in the AdocWeave runtime closure" >&2
              exit 1
            fi
            touch "$out"
          '';
          nixos-package-evaluation =
            assert builtins.elem package nixos.config.environment.systemPackages;
            pkgs.runCommand "adocweave-nixos-package-evaluation" { } ''
              test -x ${package}/bin/adocweave
              test -x ${package}/bin/adocweave-lsp
              touch "$out"
            '';
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = mkPkgs system;
          fuzzRust = pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.default);
          adocweave-fuzz = pkgs.writeShellScriptBin "adocweave-fuzz" ''
            export PATH=${fuzzRust}/bin:${pkgs.cargo-fuzz}/bin:$PATH
            exec cargo fuzz "$@"
          '';
          commonPackages = with pkgs; [
            actionlint
            cargo-dist
            cargo-audit
            cargo-deny
            cargo-make
            curl
            dejavu_fonts
            esbuild
            fontconfig
            gh
            git
            gnutar
            jq
            lld
            nodejs
            typescript
            ripgrep
            rust-analyzer
            (developmentRust pkgs)
            stdenv.cc
            wasm-bindgen-cli
            xz
            yq-go
            adocweave-fuzz
          ];
          shell = packages: pkgs.mkShell {
            inherit packages;
            ADOCWEAVE_DIST_BIN = "${pkgs.cargo-dist}/bin/dist";
            RUST_SRC_PATH = "${developmentRust pkgs}/lib/rustlib/src/rust/library";
          };
        in
        {
          default = shell (commonPackages ++ pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.chromium ]);
          ci = shell commonPackages;
        }
      );
    };
}
