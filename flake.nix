{
  description = "hitui — a terminal system information browser";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems =
        f:
        nixpkgs.lib.genAttrs systems (system: f system nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (
        system: pkgs: rec {
          default = hitui;
          hitui = pkgs.rustPlatform.buildRustPackage {
            pname = "hitui";
            version = (nixpkgs.lib.importTOML ./Cargo.toml).package.version;
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;

            # A terminal program still wants a menu entry. The icon is named
            # rather than shipped: icon names resolve through whatever icon
            # theme is in use, so this needs no hardinfo2 installed.
            postInstall = ''
              install -Dm644 assets/hitui.desktop \
                $out/share/applications/hitui.desktop
            '';

            meta = {
              description = "Terminal system information browser";
              mainProgram = "hitui";
              platforms = nixpkgs.lib.platforms.linux;
              # A from-scratch reimplementation, not derived from hardinfo2's
              # GPL-2.0 sources, so this was a free choice rather than an
              # inherited obligation — chosen to match the sibling TUIs.
              license = nixpkgs.lib.licenses.gpl3Plus;
            };
          };
        }
      );

      devShells = forAllSystems (
        system: pkgs: {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              clippy
              rustfmt
              rust-analyzer
              # Handy for cross-checking collector output against the tools
              # whose data sources hitui reads.
              pciutils
              usbutils
              lm_sensors
              util-linux
            ];
            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          };
        }
      );

      checks = forAllSystems (
        system: pkgs: {
          # buildRustPackage runs `cargo test` in its check phase.
          build = self.packages.${system}.hitui;

          # The desktop entry is parsed by other people's menu code, so it is
          # validated here rather than discovered to be wrong at install time.
          desktop =
            pkgs.runCommand "hitui-desktop-check"
              { nativeBuildInputs = [ pkgs.desktop-file-utils ]; }
              ''
                desktop-file-validate ${./assets/hitui.desktop}
                touch $out
              '';

          # Reuses the package's vendored dependencies, so this costs a lint
          # pass rather than a second fetch.
          clippy = self.packages.${system}.hitui.overrideAttrs (old: {
            pname = "hitui-clippy";
            nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [ pkgs.clippy ];
            buildPhase = "cargo clippy --all-targets -- -D warnings";
            installPhase = "touch $out";
            doCheck = false;
          });
        }
      );

      formatter = forAllSystems (system: pkgs: pkgs.nixfmt-rfc-style);
    };
}
