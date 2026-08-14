# SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: MIT
{
  root ? ../.,
  commonNixRoot ? root + "/nix",
}: {
  description = "Foundation SDK maintainer development and release environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
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
    foundationSlintHash = sdkBuildConfig.submodules.slint.source_hash;
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
      linuxCrossStd =
        if pkgs.stdenv.hostPlatform.isLinux
        then
          fenix.packages.${system}.targets.aarch64-unknown-linux-musl.fromToolchainFile {
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
        ++ pkgs.lib.optionals (linuxCrossStd != null) [
          linuxCrossStd
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
          --no-default-features \
          --features backend-winit,renderer-femtovg,renderer-software,custom-translations \
          -- "$@"
        EOF
        chmod +x "$out/bin/foundation-slint-viewer"
        ln -s foundation-slint-viewer "$out/bin/slint-viewer"
      '';
      linuxCrossPkgs =
        if pkgs.stdenv.hostPlatform.isLinux
        then pkgs.pkgsCross.aarch64-multiplatform-musl
        else null;
      linuxCrossCc =
        if linuxCrossPkgs == null
        then null
        else linuxCrossPkgs.stdenv.cc;
      linuxCrossCcPath =
        if linuxCrossCc == null
        then ""
        else "${linuxCrossCc}";
      linuxCrossPkgConfigInputs =
        if linuxCrossPkgs == null
        then []
        else [
          linuxCrossPkgs.libudev-zero
        ];
      linuxCrossPkgConfigPath =
        if linuxCrossPkgConfigInputs == []
        then ""
        else
          pkgs.lib.concatStringsSep ":" [
            (pkgs.lib.makeSearchPathOutput "dev" "lib/pkgconfig" linuxCrossPkgConfigInputs)
            (pkgs.lib.makeSearchPathOutput "dev" "share/pkgconfig" linuxCrossPkgConfigInputs)
          ];
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
            openssh
            openssl
            pkg-config
            protobuf
            viewerRunner
            zlib
          ])
          ++ [
            rustKeyos
          ]
          ++ pkgs.lib.optionals (linuxCrossCc != null) [
            linuxCrossCc
          ]
          ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux (with pkgs; [
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
              libx11
              libxcursor
              libxi
              wayland
            ]
          );

        shellHook = ''
          export FOUNDATION_DEVELOP_SHELL=1
          export FOUNDATION_SDK_ROOT="$PWD"
          export SDK_BUILD_CONFIG="$PWD/sdk-build.toml"
          export FOUNDATION_SDK_BIN="$PWD/bin"
          export FOUNDATION_PINNED_SLINT_DIR="${foundationSlint.source}"
          if [ -z "''${SLINT_DIR:-}" ]; then
            export SLINT_DIR="$FOUNDATION_PINNED_SLINT_DIR"
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

          if [ -n "${linuxCrossCcPath}" ] && [ -x "${linuxCrossCcPath}/bin/aarch64-unknown-linux-musl-gcc" ]; then
            export CC_aarch64_unknown_linux_musl="${linuxCrossCcPath}/bin/aarch64-unknown-linux-musl-gcc"
            export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="$CC_aarch64_unknown_linux_musl"
          fi

          if [ -n "${linuxCrossPkgConfigPath}" ]; then
            export PKG_CONFIG_ALLOW_CROSS_aarch64_unknown_linux_musl=1
            export PKG_CONFIG_PATH_aarch64_unknown_linux_musl="${linuxCrossPkgConfigPath}"
            export PKG_CONFIG_LIBDIR_aarch64_unknown_linux_musl="${linuxCrossPkgConfigPath}"
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
        linuxCrossStd
        maintainerShell
        rustKeyos
        ;
    };
  in {
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
        inherit (mkSystem system) linuxCrossCc pkgs rustKeyos;
      in
        {
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
        // nixpkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
          linux-cross-toolchain =
            pkgs.runCommand "foundation-sdk-linux-cross-toolchain"
            {
              nativeBuildInputs = [
                pkgs.file
                rustKeyos
                linuxCrossCc
              ];
            }
            ''
              rustc \
                --crate-name linux_cross_check \
                --target aarch64-unknown-linux-musl \
                -C linker=aarch64-unknown-linux-musl-gcc \
                ${pkgs.writeText "linux-cross-check.rs" "fn main() {}"} \
                -o linux-cross-check
              file linux-cross-check | grep -q "ARM aarch64"
              file linux-cross-check | grep -Eq "statically linked|static-pie linked"
              aarch64-unknown-linux-musl-strip --strip-debug linux-cross-check
              file linux-cross-check | grep -q "ARM aarch64"
              touch "$out"
            '';
        }
    );
  };
}
