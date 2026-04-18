{
  description = "fff-tui experimental Rust port";

  nixConfig = {
    extra-substituters = "https://anmonteiro.nix-cache.workers.dev";
    extra-trusted-public-keys = "ocaml.nix-cache.com-1:/xI2h2+56rwFfKyyFVbkJSeGqSIYMC/Je+7XXqGKDIY=";
  };

  inputs = {
    nixpkgs.url = "github:nix-ocaml/nix-overlays";
    fff = {
      url = "github:dmtrKovalenko/fff.nvim/db4cd2825c32e5e53e12bb8af106ff724d33904e";
      inputs.nixpkgs.follows = "nixpkgs";
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
    in
    {
      packages = forAllSystems (pkgs: {
        default =
          let
            nixSrc = pkgs.runCommand "fff-cli-src" { nativeBuildInputs = [ pkgs.python3 ]; } ''
              cp -R ${self} "$out"
              chmod -R +w "$out"
              cp -R ${fff.outPath} "$out/.nix-fff"
              chmod -R +w "$out/.nix-fff"

              python - "$out/Cargo.toml" "$out/Cargo.lock" <<'PY'
              from pathlib import Path
              import sys

              cargo_toml = Path(sys.argv[1])
              cargo_lock = Path(sys.argv[2])

              cargo_toml.write_text(
                  cargo_toml.read_text()
                  .replace(
                      'fff = { package = "fff-search", git = "https://github.com/dmtrKovalenko/fff.nvim", rev = "db4cd2825c32e5e53e12bb8af106ff724d33904e", features = ["zlob"] }',
                      'fff = { package = "fff-search", path = ".nix-fff/crates/fff-core", features = ["zlob"] }',
                  )
                  .replace(
                      'fff-query-parser = { git = "https://github.com/dmtrKovalenko/fff.nvim", package = "fff-query-parser", rev = "db4cd2825c32e5e53e12bb8af106ff724d33904e" }',
                      'fff-query-parser = { package = "fff-query-parser", path = ".nix-fff/crates/fff-query-parser" }',
                  )
              )

              cargo_lock.write_text(
                  cargo_lock.read_text().replace(
                      'source = "git+https://github.com/dmtrKovalenko/fff.nvim?rev=db4cd2825c32e5e53e12bb8af106ff724d33904e#db4cd2825c32e5e53e12bb8af106ff724d33904e"\n',
                      "",
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
            zig_0_15
            llvmPackages.libclang
          ];

          buildInputs =
            with pkgs;
            [ openssl ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ libiconv ];

          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
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
              zig_0_15
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
