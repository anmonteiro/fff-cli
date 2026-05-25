{
  description = "fff-tui experimental Rust port";

  nixConfig = {
    extra-substituters = "https://anmonteiro.nix-cache.workers.dev";
    extra-trusted-public-keys = "ocaml.nix-cache.com-1:/xI2h2+56rwFfKyyFVbkJSeGqSIYMC/Je+7XXqGKDIY=";
  };

  inputs = {
    nixpkgs.url = "github:nix-ocaml/nix-overlays";
    fff = {
      url = "github:dmtrKovalenko/fff.nvim";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      fff,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-darwin"
        "x86_64-darwin"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      patchFffCargoLock = ''
        perl -0pi -e '
          s/name = "fff-c"\nversion = "0\.8\.1"/name = "fff-c"\nversion = "0.8.2"/g;
          s/name = "fff-grep"\nversion = "0\.8\.1"/name = "fff-grep"\nversion = "0.8.2"/g;
          s/name = "fff-mcp"\nversion = "0\.8\.1"/name = "fff-mcp"\nversion = "0.8.2"/g;
          s/name = "fff-nvim"\nversion = "0\.8\.1"/name = "fff-nvim"\nversion = "0.8.2"/g;
          s/name = "fff-query-parser"\nversion = "0\.8\.1"/name = "fff-query-parser"\nversion = "0.8.2"/g;
          s/name = "fff-search"\nversion = "0\.8\.1"/name = "fff-search"\nversion = "0.8.2"/g;
        ' Cargo.lock
      '';
    in
    {
      sources = {
        fff = fff.outPath;
      };

      packages = forAllSystems (pkgs: {
        fff-nvim = fff.outputs.packages.${pkgs.stdenv.hostPlatform.system}.default.overrideAttrs (old: {
          cargoArtifacts = old.cargoArtifacts.overrideAttrs (depsOld: {
            postPatch = (depsOld.postPatch or "") + patchFffCargoLock;
          });
          postPatch = (old.postPatch or "") + patchFffCargoLock;
          postInstall = (old.postInstall or "") + ''
            ln -sfn $out/lib/ $out/release
          '';
        });

        default =
          let
            nixSrc = pkgs.runCommand "fff-cli-src" { nativeBuildInputs = [ pkgs.python3 ]; } ''
              cp -R ${self} "$out"
              chmod -R +w "$out"
              cp -R ${fff.outPath} "$out/.nix-fff"
              chmod -R +w "$out/.nix-fff"

              python - "$out/Cargo.toml" "$out/Cargo.lock" <<'PY'
              from pathlib import Path
              import re
              import sys

              cargo_toml = Path(sys.argv[1])
              cargo_lock = Path(sys.argv[2])

              cargo_toml.write_text(
                  re.sub(
                      r'^fff = \{ package = "fff-search", git = "https://github.com/dmtrKovalenko/fff\.nvim", (?:branch|rev) = "[^"]+", features = \["zlob"\] \}$',
                      'fff = { package = "fff-search", path = ".nix-fff/crates/fff-core", features = ["zlob"] }',
                      re.sub(
                          r'^fff-query-parser = \{ git = "https://github.com/dmtrKovalenko/fff\.nvim", package = "fff-query-parser", (?:branch|rev) = "[^"]+" \}$',
                          'fff-query-parser = { package = "fff-query-parser", path = ".nix-fff/crates/fff-query-parser" }',
                          cargo_toml.read_text(),
                          flags=re.MULTILINE,
                      ),
                      flags=re.MULTILINE,
                  )
              )

              cargo_lock.write_text(
                  re.sub(
                      r'^source = "git\+https://github.com/dmtrKovalenko/fff\.nvim(?:\?(?:branch|rev)=[^"#]+)?#[^"]+"\n',
                      "",
                      cargo_lock.read_text(),
                      flags=re.MULTILINE,
                  )
              )
              PY
            '';
          in
          pkgs.rustPlatform.buildRustPackage {
            pname = "fff-cli";
            version = "0.1.0";
            src = nixSrc;
            cargoLock = {
              lockFile = nixSrc + "/Cargo.lock";
            };

            nativeBuildInputs = with pkgs; [
              pkg-config
              zig_0_16
              llvmPackages.libclang
            ];

            buildInputs = with pkgs; [ openssl ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ libiconv ];

            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            preBuild = ''
              export HOME="$TMPDIR"
              export ZIG_GLOBAL_CACHE_DIR="$TMPDIR/zig-global-cache"
            '';
            dontUseZigConfigure = true;
            dontUseZigBuild = true;
            dontUseZigCheck = true;
            dontUseZigInstall = true;
            doCheck = false;
          };
      });

      apps = forAllSystems (pkgs: {
        default = {
          type = "app";
          program = "${self.packages.${pkgs.stdenv.hostPlatform.system}.default}/bin/fff";
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages =
            with pkgs;
            [
              cargo
              rustc
              rustfmt
              clippy
              zig_0_16
              pkg-config
              openssl
            ]
            ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
              pkgs.libiconv
            ];

          LIBCLANG_PATH = pkgs.lib.makeLibraryPath (with pkgs; [ llvmPackages.libclang ]);
          PKG_CONFIG_PATH = pkgs.lib.makeSearchPathOutput "dev" "lib/pkgconfig" [
            pkgs.openssl
          ];
        };
      });

      checks = forAllSystems (pkgs: {
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      });
    };
}
