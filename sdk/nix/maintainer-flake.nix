# SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: MIT
{
  root ? ../.,
  commonNixRoot ? root + "/nix",
}: {
  description = "Foundation SDK maintainer development and release environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-24.11";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    self,
    nixpkgs,
    fenix,
  }: let
    # Single source of truth for the toolchain so plain rustup and nix agree.
    rustToolchainFile = root + "/rust-toolchain.toml";
    rustToolchainChannel = (builtins.fromTOML (builtins.readFile rustToolchainFile)).toolchain.channel;
    rustToolchainSha256 = "sha256-NvWKV8CXj8AQXESvz5uGr6qv0JF0UHUdjYb2murEG/A=";
    sdkBuildConfig = builtins.fromTOML (builtins.readFile (root + "/sdk-build.toml"));
    foundationSlintVersion = sdkBuildConfig.submodules.slint.ref;
    foundationSlintHash = "sha256-7vZ3LnTm1l3+Q4tRSogesNzGp/iCy4IIpkP0w5/l/9k=";
    systems = [
      "aarch64-darwin"
      "x86_64-darwin"
      "x86_64-linux"
      "aarch64-linux"
    ];
    forAllSystems = nixpkgs.lib.genAttrs systems;
    mkSystem = system: let
      pkgs = import nixpkgs {inherit system;};
      baseToolchain = fenix.packages.${system}.fromToolchainFile {
        file = rustToolchainFile;
        sha256 = rustToolchainSha256;
      };
      armv7aStd = fenix.packages.${system}.targets.armv7a-none-eabi.fromToolchainFile {
        file = rustToolchainFile;
        sha256 = rustToolchainSha256;
      };
      macosCrossStd =
        if system == "aarch64-darwin"
        then
          fenix.packages.${system}.targets.x86_64-apple-darwin.fromToolchainFile {
            file = rustToolchainFile;
            sha256 = rustToolchainSha256;
          }
        else if system == "x86_64-darwin"
        then
          fenix.packages.${system}.targets.aarch64-apple-darwin.fromToolchainFile {
            file = rustToolchainFile;
            sha256 = rustToolchainSha256;
          }
        else null;
      customTargetLib = pkgs.fetchzip {
        url = "https://github.com/Foundation-Devices/rust-keyos/releases/download/1.96.0-${rustToolchainChannel}/armv7a-unknown-xous-elf_${rustToolchainChannel}.zip";
        sha256 = "sha256-BvQyJ6BfMeaqGjSeE28iMKXpiQcuIMuW02XxMS9Pcrw=";
        stripRoot = false;
      };
      rustKeyos = fenix.packages.${system}.combine (
        [
          baseToolchain
          armv7aStd
          customTargetLib
        ]
        ++ pkgs.lib.optionals (macosCrossStd != null) [
          macosCrossStd
        ]
      );
      foundationSlint = pkgs.callPackage (commonNixRoot + "/foundation-slint.nix") {
        version = foundationSlintVersion;
        hash = foundationSlintHash;
      };
      viewerRunner = pkgs.runCommand "foundation-slint-viewer-runner" {} ''
        mkdir -p "$out/bin"
        cat >"$out/bin/foundation-slint-viewer" <<'EOF'
        #!/usr/bin/env bash
        set -euo pipefail
        : "''${SLINT_DIR:=${foundationSlint.source}}"
        export CARGO_TARGET_DIR="${root}/target/foundation-slint-viewer"
        exec cargo run \
          --manifest-path "''${SLINT_DIR}/tools/viewer/Cargo.toml" \
          --bin slint-viewer \
          --features custom-translations \
          -- "$@"
        EOF
        chmod +x "$out/bin/foundation-slint-viewer"
        ln -s foundation-slint-viewer "$out/bin/slint-viewer"
      '';
      linuxCrossCc =
        if pkgs.stdenv.hostPlatform.isLinux
        then pkgs.pkgsCross.aarch64-multiplatform.stdenv.cc
        else null;
      linuxCrossCcPath =
        if linuxCrossCc == null
        then ""
        else "${linuxCrossCc}";
      maintainerShell = pkgs.mkShell {
        packages =
          (with pkgs; [
            clang
            cmake
            fontconfig
            gcc-arm-embedded
            git
            gnumake
            gnutar
            gzip
            just
            mdbook
            openssl
            pkg-config
            protobuf
            viewerRunner
            zlib
          ])
          ++ [
            rustKeyos
          ]
          ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux (with pkgs; [
            linuxCrossCc
            systemd
          ]);

        LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

        LD_LIBRARY_PATH = with pkgs;
          lib.makeLibraryPath (
            [
              fontconfig
              libGL
              libxkbcommon
              zlib
            ]
            ++ lib.optionals stdenv.isLinux [
              systemd
              xorg.libX11
              xorg.libXcursor
              xorg.libXi
              wayland
            ]
          );

        shellHook = ''
          export FOUNDATION_SDK_ROOT="$PWD"
          export SDK_BUILD_CONFIG="$PWD/sdk-build.toml"
          export FOUNDATION_SDK_BIN="$PWD/bin"
          if [ -z "''${SLINT_DIR:-}" ]; then
            export SLINT_DIR="${foundationSlint.source}"
          fi

          if [ "$(uname -s)" = "Darwin" ]; then
            unset DEVELOPER_DIR SDKROOT || true

            if [ -x /usr/bin/xcode-select ]; then
              FOUNDATION_DEVELOPER_DIR="$(
                /usr/bin/xcode-select -p 2>/dev/null || true
              )"
              if [ -n "$FOUNDATION_DEVELOPER_DIR" ]; then
                export DEVELOPER_DIR="$FOUNDATION_DEVELOPER_DIR"
              fi
            fi

            if [ -x /usr/bin/xcrun ]; then
              FOUNDATION_SDKROOT="$(
                /usr/bin/xcrun --sdk macosx --show-sdk-path 2>/dev/null || true
              )"
              if [ -n "$FOUNDATION_SDKROOT" ]; then
                export SDKROOT="$FOUNDATION_SDKROOT"
              fi
            fi

            export CC="/usr/bin/cc"
            export CXX="/usr/bin/c++"
            export CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER="/usr/bin/cc"
            export CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER="/usr/bin/cc"
          fi

          if [ -n "${linuxCrossCcPath}" ] && [ -x "${linuxCrossCcPath}/bin/aarch64-unknown-linux-gnu-gcc" ]; then
            export CC_aarch64_unknown_linux_gnu="${linuxCrossCcPath}/bin/aarch64-unknown-linux-gnu-gcc"
            export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="$CC_aarch64_unknown_linux_gnu"
          fi

          if [ -d "$FOUNDATION_SDK_BIN" ]; then
            export PATH="$FOUNDATION_SDK_BIN:$PATH"
          fi

          echo "Foundation SDK maintainer shell ready."
          echo "Run: cargo xtask build --target all --release"
        '';
      };
    in {
      inherit
        pkgs
        foundationSlint
        linuxCrossCc
        linuxCrossCcPath
        maintainerShell
        ;
    };
  in {
    packages = forAllSystems (
      system: let
        inherit (mkSystem system) foundationSlint;
      in {
        foundation-slint-viewer = foundationSlint."foundation-slint-viewer";
      }
    );
    devShells = forAllSystems (
      system: let
        inherit (mkSystem system) maintainerShell;
      in {
        default = maintainerShell;
        maintainer = maintainerShell;
      }
    );
    checks = forAllSystems (
      system: let
        inherit (mkSystem system) pkgs;
      in {
        workspace-tests =
          pkgs.runCommand "foundation-sdk-workspace-tests"
          {
            nativeBuildInputs = with pkgs; [
              cargo
              rustc
              git
            ];
          }
          ''
            export HOME="$TMPDIR/home"
            export CARGO_HOME="$TMPDIR/cargo-home"
            mkdir -p "$HOME" "$CARGO_HOME"
            cd ${root}
            cargo test --offline --locked --manifest-path Cargo.toml
            touch "$out"
          '';
      }
    );
  };
}
