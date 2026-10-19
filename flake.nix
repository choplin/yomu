{
  description = "Rust development environment for yomu";

  inputs = {
    nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/0";
    flake-parts.url = "github:hercules-ci/flake-parts";
    devshell = {
      url = "github:numtide/devshell";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{
      nixpkgs,
      flake-parts,
      devshell,
      fenix,
      ...
    }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [ ];

      flake = { };

      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
        "x86_64-darwin"
      ];

      perSystem =
        {
          config,
          self',
          inputs',
          pkgs,
          system,
          ...
        }:
        let
          cargoManifest = builtins.fromTOML (builtins.readFile ./Cargo.toml);
          releaseVersion = "0.1.0";
          releaseArtifacts = {
            aarch64-darwin = {
              target = "aarch64-apple-darwin";
              hash = "sha256-bFXHiFrQGjf+xx8/GmjHnCqHCxUF8VQ8wljcqSldc0w=";
            };
            x86_64-darwin = {
              target = "x86_64-apple-darwin";
              hash = "sha256-MNcOJmvC89I8PDgZ7WaN3ingcIDU+8/xC97TVuVO3UY=";
            };
            x86_64-linux = {
              target = "x86_64-unknown-linux-gnu";
              hash = "sha256-OZHzae2xEVpnSBm8Wyy8JIB0fIU2N3Vwyfpj7qMVm1M=";
            };
          };
          releaseArtifact = releaseArtifacts.${system} or null;
          rustToolchain = fenix.packages.${system}.stable.withComponents [
            "cargo"
            "clippy"
            "rust-analyzer"
            "rust-src"
            "rustc"
            "rustfmt"
          ];
          rustPlatform = pkgs.makeRustPlatform {
            cargo = rustToolchain;
            rustc = rustToolchain;
          };
          yomuSource = rustPlatform.buildRustPackage {
            pname = "yomu";
            version = cargoManifest.package.version;
            src = pkgs.lib.cleanSource ./.;
            cargoLock.lockFile = ./Cargo.lock;

            meta = {
              description = "A terminal Markdown reader with rich image, table, and math rendering";
              homepage = "https://github.com/choplin/yomu";
              license = pkgs.lib.licenses.mit;
              mainProgram = "yomu";
            };
          };
          yomuBin =
            if releaseArtifact == null then
              null
            else
              pkgs.stdenvNoCC.mkDerivation {
                pname = "yomu-bin";
                version = releaseVersion;
                src = pkgs.fetchurl {
                  url = "https://github.com/choplin/yomu/releases/download/v${releaseVersion}/yomu-${releaseArtifact.target}.tar.xz";
                  inherit (releaseArtifact) hash;
                };
                sourceRoot = "yomu-${releaseArtifact.target}";
                nativeBuildInputs = pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.autoPatchelfHook ];
                buildInputs = pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.stdenv.cc.cc.lib ];

                installPhase = ''
                  runHook preInstall

                  install -Dm755 yomu "$out/bin/yomu"
                  install -Dm644 README.md "$out/share/doc/yomu/README.md"
                  install -Dm644 CHANGELOG.md "$out/share/doc/yomu/CHANGELOG.md"
                  install -Dm644 LICENSE "$out/share/licenses/yomu/LICENSE"

                  runHook postInstall
                '';

                meta = {
                  description = "A terminal Markdown reader with rich image, table, and math rendering";
                  homepage = "https://github.com/choplin/yomu";
                  license = pkgs.lib.licenses.mit;
                  mainProgram = "yomu";
                  sourceProvenance = [ pkgs.lib.sourceTypes.binaryNativeCode ];
                };
              };
        in
        {
          _module.args.pkgs = import nixpkgs {
            inherit system;
            overlays = [ devshell.overlays.default ];
          };

          devShells.default = pkgs.devshell.mkShell {
            imports = [ (pkgs.devshell.importTOML ./devshell.toml) ];
            packages = [
              rustToolchain
              pkgs.python3
            ];
          };

          packages = {
            yomu = yomuSource;
          }
          // pkgs.lib.optionalAttrs (yomuBin != null) {
            yomu-bin = yomuBin;
          };
        };
    };
}
