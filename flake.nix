{
    description = "dim-map-builder: build, clean, annotate and save maps from dimos recordings, as a dimOS Desktop app";

    inputs = {
        nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
        rust-overlay.url = "github:oxalica/rust-overlay";
        rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
    };

    outputs = { self, nixpkgs, rust-overlay }:
        let
            systems = [ "aarch64-darwin" "x86_64-darwin" "x86_64-linux" "aarch64-linux" ];
            forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs { inherit system; overlays = [ (import rust-overlay) ]; }));
        in {
            packages = forAllSystems (pkgs:
                let
                    rust = pkgs.rust-bin.stable.latest.default;
                    rustPlatform = pkgs.makeRustPlatform { cargo = rust; rustc = rust; };

                    # the page: React + Vite (type-checked first), from package-lock.json
                    frontend = pkgs.buildNpmPackage {
                        pname = "dim-map-builder-frontend";
                        version = "0.1.0";
                        src = ./frontend;
                        npmDepsHash = "sha256-Xcui+5T8LHsBXLt75xADOYFqlork3EQ1w8ap+J4UjAs=";
                        installPhase = ''
                            cp -r dist $out
                        '';
                    };

                    # the backend: one Rust binary (map building, editing, saving, the agent tools); no Python at runtime
                    server = rustPlatform.buildRustPackage {
                        pname = "dim-map-builder-server";
                        version = "0.1.0";
                        src = pkgs.lib.cleanSourceWith {
                            src = ./.;
                            filter = path: type: let base = baseNameOf path; in !(builtins.elem base [ "frontend" "target" "result" "docs" "eval" ]);
                        };
                        cargoLock.lockFile = ./Cargo.lock;
                        cargoBuildFlags = [ "-p" "dimos-app-server" ];
                        doCheck = false;
                    };
                in {
                    inherit frontend server;
                    # what Desktop builds: bin/dimos-app-server, which serves everything under /apps/<name>/
                    dimosApp = pkgs.runCommand "dim-map-builder" { nativeBuildInputs = [ pkgs.makeWrapper ]; } ''
                        mkdir -p $out/bin
                        makeWrapper ${server}/bin/dimos-app-server $out/bin/dimos-app-server --set-default MAP_BUILDER_FRONTEND ${frontend}
                        cp ${self}/icon.svg $out/icon.svg
                    '';
                    default = self.packages.${pkgs.system}.dimosApp;
                });
        };
}
