{
  description = "Ali Kernel — Sovereign AI execution engine with training flywheel";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ rust-overlay ];
        pkgs = import nixpkgs { inherit overlays; inherit system; };

        # Build a Rust package from a cargo workspace member
        buildAli = { pname, version, cargoToml, installPhase ? null }:
          let
            rustPkgs = pkgs.makeRustPlatform {
              cargo = pkgs.cargo;
              rustc = pkgs.rustc;
            };
          in
          rustPkgs.buildRustPackage {
            inherit pname version;
            src = self;
            cargoLock = ./Cargo.lock;
            cargoToml = ./${cargoToml};
            nativeBuildInputs = with pkgs; [ pkg-config ];
            buildInputs = with pkgs; [ openssl zlib ]
              ++ (lib.optionals pkgs.stdenv.isLinux [ pkgs.dbus ]);
          };

        lib = pkgs.lib;
      in
      {
        packages = {
          ali-kernel = buildAli {
            pname = "ali-kernel";
            version = "1.0.0";
            cargoToml = "ali-kernel/Cargo.toml";
          };

          ali-tui = buildAli {
            pname = "ali-tui";
            version = "1.0.0";
            cargoToml = "ali-tui/Cargo.toml";
          };

          ali = let
            bin = pkgs.writeScript "ali" ''
              #!${pkgs.bash}/bin/bash
              # Ali Kernel launcher — self-locating, portable
              set -euo pipefail

              # Resolve the directory containing this script (follows symlinks)
              SOURCE="$0"
              while [ -L "$SOURCE" ]; do
                  TARGET="$(readlink -f "$SOURCE")"
                  case "$TARGET" in
                      */*) SOURCE="$TARGET";;
                      *) SOURCE="./$TARGET";;
                  esac
              done
              SCRIPT_DIR="$(cd "$(dirname "$SOURCE")" && pwd)"

              export ALI_CONFIG_DIR="''${XDG_CONFIG_HOME:-$HOME/.config}/ali"
              export ALI_CONFIG="''$ALI_CONFIG_DIR/config.toml"

              # Create default config dir
              mkdir -p "''$ALI_CONFIG_DIR"

              # If no config exists, create a minimal one
              if [ ! -f "''$ALI_CONFIG" ]; then
                  cat > "''$ALI_CONFIG" << 'EOFCFG'
llama_bin = "${pkgs.llama-cpp}/bin/llama-server"
model_path = "./model.gguf"
llama_port = 8081
gateway_port = 9000
human_gate = false
backend_model = "ornith"
EOFCFG
              fi

              # Check for llama-server in PATH, start if not running
              if ! ${pkgs.netstat}/bin/netstat -tln 2>/dev/null | grep -q ':8081 '; then
                  LLAMA_BIN="''${LLAMA_BIN:-$(grep '^llama_bin' "''$ALI_CONFIG" 2>/dev/null | cut -d'"' -f2)}"
                  MODEL_PATH="''${MODEL_PATH:-$(grep '^model_path' "''$ALI_CONFIG" 2>/dev/null | cut -d'"' -f2)}"
                  if [ -x "''$LLAMA_BIN" ] && [ -f "''$MODEL_PATH" ]; then
                      nohup "''$LLAMA_BIN" \
                          --model "''$MODEL_PATH" \
                          -ngl 99 \
                          -c 4096 \
                          -fa &
                      sleep 2
                  fi
              fi

              # Start gateway if not running
              if ! ${pkgs.netstat}/bin/netstat -tln 2>/dev/null | grep -q ':9000 '; then
                  nohup "$SCRIPT_DIR/../../../target/release/ali-kernel" &
                  sleep 2
              fi

              # Launch TUI
              exec "$SCRIPT_DIR/../../../target/release/ali-tui" "''$@"
            '';
          in
          pkgs.symlinkJoin {
            name = "ali";
            paths = [ bin ];
            nativeBuildInputs = [ pkgs.makeWrapper ];
            postBuild = ''
              wrapProgram "$out/bin/ali" --set ALI_CONFIG_DIR "$HOME/.config/ali"
            '';
          };

          default = self.packages.${system}.ali-kernel;
        };

        devShells.default = pkgs.mkShell {
          name = "ali-kernel-dev";
          inputsFrom = [ ];
          buildInputs = with pkgs; [
            cargo rustc rustfmt clippy
            pkg-config openssl zlib
            rust-analyzer
          ];
          RUST_BACKTRACE = "1";
          RUST_LOG = "ali_kernel=info";
          env = {
            BUILD_TIMESTAMP = builtins.currentTime;
          };
        };

        apps.ali = {
          type = "app";
          program = "${self.packages.${system}.ali}/bin/ali";
        };
        defaultApp = self.apps.${system}.ali;
      }
    );
}
