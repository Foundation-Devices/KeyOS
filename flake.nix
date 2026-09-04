# SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: GPL-3.0-or-later
{
  description = "KeyOS development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
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
    inherit (nixpkgs) lib;

    systems = [
      "aarch64-darwin"
      "x86_64-darwin"
      "aarch64-linux"
      "x86_64-linux"
    ];

    forAllSystems = f:
      lib.foldl' lib.recursiveUpdate {} (
        map (
          system:
            lib.mapAttrs (_: value: {${system} = value;}) (f system)
        )
        systems
      );
  in
    forAllSystems (system: let
      pkgs = import nixpkgs {
        inherit system;
        config = {
          allowUnfree = true;
          permittedInsecurePackages = ["segger-jlink-qt4-874"];
          segger-jlink.acceptLicense = true;
        };
      };

      customPackages =
        (with pkgs; {
          inherit just reuse taplo;
          # upstream slint-lsp for CI (faster)
          slint-lsp-upstream = slint-lsp;
        })
        // pkgs.callPackage ./nix/rust-toolchain.nix {inherit self fenix system;}
        // pkgs.callPackage ./nix/slint.nix {}
        // pkgs.callPackage ./nix/cosign2.nix {inherit self;}
        // pkgs.callPackage ./nix/localazy.nix {}
        // pkgs.callPackage ./nix/slint-runner.nix {};

      buildPackages = with pkgs;
        [
          bc
          taplo
          cmake
          curl
          gcc-arm-embedded
          gh
          git
          git-lfs
          gnumake
          just
          openssl
          protobuf
          pkg-config
          reuse
          unixtools.xxd

          clang
          gcc
          llvmPackages.libclang
          llvmPackages.libcxxClang
          llvmPackages.llvm
          # For the SDK icon2glyph.py
          (python3.withPackages (ps: [ps.pillow]))
        ]
        ++ (with customPackages; [
          cosign2
          rust-keyos
        ]);

      devPackages =
        buildPackages
        ++ (with customPackages; [
          localazy
          rust-analyzer
          slint-runner
          slint-lsp
          slint-viewer
        ])
        ++ (
          with pkgs;
            [
              mdcat
              mermaid-cli
            ]
            ++ lib.optionals stdenv.isLinux [
              segger-jlink
            ]
        );

      clangAttrs = {
        # for bindgen in c++ libs
        LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
      };

      darwinPkgs = with pkgs;
        lib.optionals stdenv.isDarwin [
          libiconv
        ];

      sharedLibs = with pkgs;
        [
          pcsclite
          libusb1
          zlib
        ]
        ++ darwinPkgs
        ++ lib.optionals stdenv.isLinux [udev fontconfig];

      mkShell = packages:
        pkgs.mkShellNoCC (
          {
            strictDeps = true;
            packages = packages;
            hardeningDisable = ["all"];
            buildInputs = sharedLibs;
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath sharedLibs;

            shellHook = ''
              export FOUNDATION_DEVELOP_SHELL=1

              # darwin xcode
              unset DEVELOPER_DIR
              unset SDKROOT

              # unset clang env variables
              unset CC
              unset CXX
              unset AR
              unset RANLIB
            '';
          }
          // clangAttrs
          // lib.optionalAttrs pkgs.stdenv.isDarwin {
            LIBRARY_PATH = lib.makeLibraryPath darwinPkgs;
          }
        );
    in {
      packages = customPackages;
      formatter = pkgs.alejandra;
      devShells = {
        # full development shell
        default = mkShell devPackages;
        # minimal build shell
        build = mkShell buildPackages;
      };
    });
}
