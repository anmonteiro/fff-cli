{
  description = "fff-tui experimental Rust port";

  nixConfig = {
    extra-substituters = "https://anmonteiro.nix-cache.workers.dev";
    extra-trusted-public-keys = "ocaml.nix-cache.com-1:/xI2h2+56rwFfKyyFVbkJSeGqSIYMC/Je+7XXqGKDIY=";
  };

  inputs.nixpkgs.url = "github:nix-ocaml/nix-overlays";

  outputs =
    { self, nixpkgs }:
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
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "fff-cli";
          version = "0.1.0";
          src = self;
          cargoLock = {
            lockFile = ./Cargo.lock;
            allowBuiltinFetchGit = true;
          };

          nativeBuildInputs = with pkgs; [
            pkg-config
            zig
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
              zig
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
