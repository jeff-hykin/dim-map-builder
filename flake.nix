{
    description = "dim-map-builder: build, clean, annotate and save maps from dimos recordings, as a dimOS Desktop app";

    inputs = {
        nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
        # the page's deno: nixos-25.11's 2.6.10, the version dimos pins (unstable's newer deno refuses packages younger than a day)
        nixpkgs-deno.url = "github:NixOS/nixpkgs/nixos-25.11";
        rust-overlay.url = "github:oxalica/rust-overlay";
        rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
        # deps build once into their own derivation (keyed on Cargo.toml / Cargo.lock), so a commit that only edits our code rebuilds just our crates
        crane.url = "github:ipetkov/crane";
    };

    outputs = { self, nixpkgs, nixpkgs-deno, rust-overlay, crane }:
        let
            systems = [ "aarch64-darwin" "x86_64-darwin" "x86_64-linux" "aarch64-linux" ];
            forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs { inherit system; overlays = [ (import rust-overlay) ]; }));
        in {
            packages = forAllSystems (pkgs:
                let
                    rust = pkgs.rust-bin.stable.latest.default;
                    rustPlatform = pkgs.makeRustPlatform { cargo = rust; rustc = rust; };

                    deno = nixpkgs-deno.legacyPackages.${pkgs.stdenv.hostPlatform.system}.deno;
                    denoEnv = "export HOME=$TMPDIR DENO_DIR=$TMPDIR/deno DENO_NO_UPDATE_CHECK=1 DENO_NO_PROMPT=1";
                    frontendFiles = paths: pkgs.lib.fileset.toSource { root = ./frontend; fileset = pkgs.lib.fileset.unions paths; };
                    # `deno install --frozen` (deno.lock pins every package) with network, so fixed-output. Its hash is per
                    # build machine (vite's native rollup and esbuild differ by OS and CPU): `deno task update-hashes` refreshes
                    # this machine's after frontend/deno.json or deno.lock change.
                    frontendModules = pkgs.stdenvNoCC.mkDerivation {
                        name = "dim-map-builder-frontend-modules";
                        src = frontendFiles [ ./frontend/deno.json ./frontend/deno.lock ];
                        nativeBuildInputs = [ deno ];
                        buildPhase = ''
                            ${denoEnv}
                            export SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt
                            deno install --frozen
                            # deno's own record of the install: not needed, and not byte-identical between installs
                            rm -f node_modules/.deno/.setup-cache.bin
                        '';
                        installPhase = "cp -R node_modules $out";
                        dontFixup = true;
                        outputHashMode = "recursive";
                        outputHashAlgo = "sha256";
                        outputHash = {
                            aarch64-darwin = "sha256-oO+P8sfp+UkDIpcmAzY4FqTDhNeop0Y4AHHbxGjwf+Q=";
                            x86_64-darwin = "";
                            x86_64-linux = "sha256-UHGas9HqIgcuMMgGITTDW7mGb93EZH7dw7TW0SpmCJw=";
                            aarch64-linux = "";
                        }.${pkgs.stdenv.hostPlatform.system};
                    };

                    pageFiles = frontendFiles [ ./frontend/deno.json ./frontend/deno.lock ./frontend/tsconfig.json ./frontend/index.html ./frontend/vite.config.ts ./frontend/vite.share.config.ts ./frontend/dev ./frontend/tools ./frontend/src ./frontend/public ];
                    # both vite builds: the page, then the shared map's viewer into the same dist
                    viteBuild = "deno run -A --cached-only --node-modules-dir=manual npm:vite build --logLevel warn && deno run -A --cached-only --node-modules-dir=manual npm:vite build -c vite.share.config.ts --logLevel warn";
                    # dimos-react as esm.sh builds its tag (frontend/tools/esm_fetch.ts): a vite build with network that
                    # keeps only what it fetched, so fixed-output; the same on every system. `deno task update-hashes` refreshes it.
                    frontendEsm = pkgs.stdenvNoCC.mkDerivation {
                        name = "dim-map-builder-frontend-esm";
                        src = pageFiles;
                        nativeBuildInputs = [ deno ];
                        buildPhase = ''
                            ${denoEnv}
                            export SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt
                            cp -R ${frontendModules} node_modules
                            chmod -R u+w node_modules
                            export ESM_CACHE=$TMPDIR/esm
                            ${viteBuild}
                        '';
                        installPhase = "cp -R $TMPDIR/esm $out";
                        dontFixup = true;
                        outputHashMode = "recursive";
                        outputHashAlgo = "sha256";
                        outputHash = "sha256-4ty8hSqT2w2LcHl4TWvXZy0O1Uf+sB/RdHuzKwf0ORU=";
                    };

                    # the page: React + Vite against those modules and that esm.sh cache, offline
                    frontend = pkgs.stdenvNoCC.mkDerivation {
                        name = "dim-map-builder-frontend";
                        src = pageFiles;
                        nativeBuildInputs = [ deno ];
                        buildPhase = ''
                            ${denoEnv}
                            cp -R ${frontendModules} node_modules
                            chmod -R u+w node_modules
                            export ESM_CACHE=${frontendEsm} ESM_OFFLINE=1
                            ${viteBuild}
                        '';
                        installPhase = "cp -R dist $out";
                    };

                    # the git dependencies (one hash per repo + rev): the dimos ray tracer and Point-LIO, the dimos-module they build on, livox-wire, lcm-msgs
                    # vendored by nixpkgs (crane's own fetcher would pull dimos's LFS data), then handed to crane as a vendor dir
                    vendorDir = pkgs.rustPlatform.importCargoLock {
                        lockFile = ./Cargo.lock;
                        outputHashes = {
                            "dimos-voxel-ray-tracing-0.1.0" = "sha256-GFdIBvweICBNyEaHPcI2WS6O534NpZk6cnwrtvDDu78=";
                            "dimos-module-0.1.0" = "sha256-6ikqdS3sroOJGI4WGuSyFx2yZjtJ+4Qgt+IGlkFl+qA=";
                            "dimos-pointlio-0.1.0" = "sha256-GFdIBvweICBNyEaHPcI2WS6O534NpZk6cnwrtvDDu78=";
                            "livox-wire-0.1.0" = "sha256-wDNVchRiqm4N9wPGwvYJ9JssmPOC33wBTE4QT6vZkGA=";
                            "pointlio-core-0.1.0" = "sha256-FjUcsZVI8nTcmKw8WhdxBuHelBjOCeJmyEsyqRxDvX0=";
                            "lcm-msgs-0.1.0" = "sha256-GGkx4Mn6NYP6KZecmoRLKGWIih/+y8OgNn12DeXX6n8=";
                        };
                    };
                    cargoVendorDir = pkgs.runCommand "dim-map-builder-vendor" { } ''
                        mkdir $out
                        sed 's|^directory = "cargo-vendor-dir"$|directory = "${vendorDir}"|' ${vendorDir}/.cargo/config.toml > $out/config.toml
                    '';

                    # only what cargo reads, so a dimos.yaml or frontend change doesn't rebuild the server
                    src = pkgs.lib.fileset.toSource {
                        root = ./.;
                        fileset = pkgs.lib.fileset.unions [ ./Cargo.toml ./Cargo.lock ./crates ./server ];
                    };

                    # the backend: one Rust binary (map building, editing, saving, the agent tools); no Python at runtime
                    # Cargo.toml's thin LTO stays: it is most of the ~1 min a code-only commit costs, but without it a 60 s map build took 89 s
                    # crane: buildDepsOnly compiles every dependency against a dummy of our crates (so its hash only moves with Cargo.toml / Cargo.lock), buildPackage starts from that target dir
                    rustBuild = { craneLib, pname, cargoExtraArgs, extra ? { }, install ? { } }:
                        let
                            common = {
                                inherit src cargoVendorDir pname cargoExtraArgs;
                                version = "0.1.0";
                                doCheck = false;
                                strictDeps = true;
                            } // extra;
                            cargoArtifacts = craneLib.buildDepsOnly (common // {
                                # only the build's artifacts are reused; a `cargo check` pass would be a second compile nobody reads
                                cargoCheckCommand = "true";
                            });
                        in
                        craneLib.buildPackage (common // {
                            inherit cargoArtifacts;
                            # nothing builds on this target dir, so don't archive it into $out
                            doInstallCargoArtifacts = false;
                        } // install);

                    craneLib = (crane.mkLib pkgs).overrideToolchain (_: rust);
                    server = rustBuild {
                        inherit craneLib;
                        pname = "dim-map-builder-server";
                        cargoExtraArgs = "--locked -p dimos-app-server";
                    };

                    # <arch> Linux from any machine: a static musl binary linked by zig, so no Linux builder or cross gcc
                    crossServer = arch:
                        let
                            target = "${arch}-unknown-linux-musl";
                            envTarget = builtins.replaceStrings [ "-" ] [ "_" ] target;
                            zigShim = wrapper: pkgs.writeShellScript "zigbuild-shim" "exec \"$CARGO_ZIGBUILD_CACHE_DIR\"/cargo-zigbuild/*/wrappers/*/${wrapper} \"$@\"";
                            rustCross = rust.override { targets = [ target ]; };
                        in
                        rustBuild {
                            craneLib = (crane.mkLib pkgs).overrideToolchain (_: rustCross);
                            pname = "dim-map-builder-server-${arch}-linux";
                            cargoExtraArgs = "--locked -p dimos-app-server --target ${target}";
                            extra = {
                                nativeBuildInputs = [ pkgs.cargo-zigbuild pkgs.zig ];
                                cargoBuildCommand = "cargo zigbuild --profile release";
                                # cargo fingerprints the linker, CC and AR paths, so they are nix-store shims (the same in every build, whichever nixbld user runs it) that hand off to the wrappers zigbuild writes in this build's own $TMPDIR; zigbuild leaves a CC / linker it finds set alone
                                # (not one shared dir: zigbuild writes its wrappers 0700, so a second nixbld user, as in CI's parallel builds, can't run them)
                                preBuild = ''
                                    export HOME=$TMPDIR ZIG_GLOBAL_CACHE_DIR=$TMPDIR/zig CARGO_ZIGBUILD_CACHE_DIR=$TMPDIR/zigbuild
                                    export CC_${envTarget}=${zigShim "zigcc-${target}-*.sh"} CXX_${envTarget}=${zigShim "zigcxx-${target}-*.sh"} AR_${envTarget}=${zigShim "ar"} RANLIB_${envTarget}=${zigShim "zigranlib.sh"}
                                    export CARGO_TARGET_${pkgs.lib.toUpper envTarget}_LINKER=$CC_${envTarget}
                                '';
                            };
                            install = {
                                installPhaseCommand = "install -Dm755 target/${target}/release/dimos-app-server $out/bin/dimos-app-server";
                            };
                        };
                    # the same wrapper as dimosApp, but its shell is the target's (a cache.nixos.org download); the frontend is plain JS
                    linuxApp = arch:
                        let linux = nixpkgs.legacyPackages."${arch}-linux"; in
                        pkgs.runCommand "dim-map-builder-${arch}-linux" { } ''
                            mkdir -p $out/bin
                            printf '#!%s\nexport MAP_BUILDER_FRONTEND="''${MAP_BUILDER_FRONTEND:-%s}"\nexec %s "$@"\n' \
                                ${linux.runtimeShell} ${frontend} ${crossServer arch}/bin/dimos-app-server > $out/bin/dimos-app-server
                            chmod +x $out/bin/dimos-app-server
                            cp ${./icon.svg} $out/icon.svg
                        '';
                in {
                    inherit frontend frontendModules frontendEsm server;
                    # what Desktop builds: bin/dimos-app-server, which serves everything under /apps/<name>/
                    dimosApp = pkgs.runCommand "dim-map-builder" { nativeBuildInputs = [ pkgs.makeWrapper ]; } ''
                        mkdir -p $out/bin
                        makeWrapper ${server}/bin/dimos-app-server $out/bin/dimos-app-server --set-default MAP_BUILDER_FRONTEND ${frontend}
                        cp ${./icon.svg} $out/icon.svg
                    '';
                    default = self.packages.${pkgs.system}.dimosApp;
                    dimosApp-aarch64-linux = linuxApp "aarch64";
                    dimosApp-x86_64-linux = linuxApp "x86_64";
                });
        };
}
