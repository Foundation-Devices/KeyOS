# SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: MIT
{
  description = "Foundation SDK user environment";

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
    rustToolchainChannel = "nightly-2026-04-11";
    rustToolchainSha256 = "sha256-NvWKV8CXj8AQXESvz5uGr6qv0JF0UHUdjYb2murEG/A=";
    rustToolchainFile = builtins.toFile "foundation-sdk-rust-toolchain.toml" ''
      [toolchain]
      channel = "${rustToolchainChannel}"
      components = ["rustfmt", "clippy", "rustc", "rust-src"]
      targets = []
      profile = "minimal"
    '';
    systems = [
      "aarch64-darwin"
      "x86_64-darwin"
      "x86_64-linux"
      "aarch64-linux"
    ];
    forAllSystems = nixpkgs.lib.genAttrs systems;
  in {
    devShells = forAllSystems (
      system: let
        pkgs = import nixpkgs {inherit system;};
        baseToolchain = fenix.packages.${system}.fromToolchainFile {
          file = rustToolchainFile;
          sha256 = rustToolchainSha256;
        };
        armv7aStd = fenix.packages.${system}.targets.armv7a-none-eabi.fromToolchainFile {
          file = rustToolchainFile;
          sha256 = rustToolchainSha256;
        };
        customTargetLib = pkgs.fetchzip {
          url = "https://github.com/Foundation-Devices/rust-keyos/releases/download/1.96.0-${rustToolchainChannel}/armv7a-unknown-xous-elf_${rustToolchainChannel}.zip";
          sha256 = "sha256-BvQyJ6BfMeaqGjSeE28iMKXpiQcuIMuW02XxMS9Pcrw=";
          stripRoot = false;
        };
        rustKeyos = fenix.packages.${system}.combine [
          baseToolchain
          armv7aStd
          customTargetLib
        ];
      in {
        default = pkgs.mkShell {
          packages = with pkgs;
            [
              clang
              fontconfig
              gcc-arm-embedded
              git
              gnumake
              openssl
              pkg-config
              rustKeyos
              zlib
            ]
            ++ lib.optionals stdenv.isLinux [
              systemd
            ];

          LD_LIBRARY_PATH = with pkgs;
            lib.makeLibraryPath (
              [
                fontconfig
                zlib
              ]
              ++ lib.optionals stdenv.isLinux [
                libGL
                libxkbcommon
                systemd
                xorg.libX11
                xorg.libXcursor
                xorg.libXi
                wayland
              ]
            );

          shellHook = ''
            export FOUNDATION_SDK_ROOT="''${FOUNDATION_SDK_ROOT:-$PWD}"
            export FOUNDATION_SDK_BIN="''${FOUNDATION_SDK_BIN:-$FOUNDATION_SDK_ROOT/bin}"

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

            if [ -d "$FOUNDATION_SDK_BIN" ]; then
              export PATH="$FOUNDATION_SDK_BIN:$PATH"
            fi

            echo "Foundation SDK user shell ready."
            echo "Run: foundation doctor"
          '';
        };
      }
    );
  };
}
