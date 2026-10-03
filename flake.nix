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

                    # <arch> Linux from any machine: a static musl binary linked by zig, so no Linux builder or cross gcc
                    crossServer = arch:
                        let
                            target = "${arch}-unknown-linux-musl";
                            rustCross = rust.override { targets = [ target ]; };
                        in
                        (pkgs.makeRustPlatform { cargo = rustCross; rustc = rustCross; }).buildRustPackage {
                            pname = "dim-map-builder-server-${arch}-linux";
                            version = "0.1.0";
                            src = server.src;
                            cargoLock.lockFile = ./Cargo.lock;
                            nativeBuildInputs = [ pkgs.cargo-zigbuild pkgs.zig ];
                            # cargo-auditable's -Wl,--undefined is a flag zig's linker rejects
                            auditable = false;
                            buildPhase = ''
                                export HOME=$TMPDIR ZIG_GLOBAL_CACHE_DIR=$TMPDIR/zig
                                cargo zigbuild --release --offline --target ${target} -p dimos-app-server
                            '';
                            doCheck = false;
                            installPhase = "install -Dm755 target/${target}/release/dimos-app-server $out/bin/dimos-app-server";
                        };
                    # the same wrapper as dimosApp, but its shell is the target's (a cache.nixos.org download); the frontend is plain JS
                    linuxApp = arch:
                        let linux = nixpkgs.legacyPackages."${arch}-linux"; in
                        pkgs.runCommand "dim-map-builder-${arch}-linux" { } ''
                            mkdir -p $out/bin
                            printf '#!%s\nexport MAP_BUILDER_FRONTEND="''${MAP_BUILDER_FRONTEND:-%s}"\nexec %s "$@"\n' \
                                ${linux.runtimeShell} ${frontend} ${crossServer arch}/bin/dimos-app-server > $out/bin/dimos-app-server
                            chmod +x $out/bin/dimos-app-server
                            cp ${self}/icon.svg $out/icon.svg
                        '';
                in {
                    inherit frontend server;
                    # what Desktop builds: bin/dimos-app-server, which serves everything under /apps/<name>/
                    dimosApp = pkgs.runCommand "dim-map-builder" { nativeBuildInputs = [ pkgs.makeWrapper ]; } ''
                        mkdir -p $out/bin
                        makeWrapper ${server}/bin/dimos-app-server $out/bin/dimos-app-server --set-default MAP_BUILDER_FRONTEND ${frontend}
                        cp ${self}/icon.svg $out/icon.svg
                    '';
                    default = self.packages.${pkgs.system}.dimosApp;
                    dimosApp-aarch64-linux = linuxApp "aarch64";
                    dimosApp-x86_64-linux = linuxApp "x86_64";
                });
        };
}
