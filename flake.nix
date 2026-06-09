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
          permittedInsecurePackages = ["segger-jlink-qt4-810"];
          segger-jlink.acceptLicense = true;
        };
      };

      customPackages = let
        ciPkgs = with pkgs; {
          inherit just reuse taplo;
          # upstream slint-lsp for CI (faster)
          slint-lsp-upstream = slint-lsp;
        };
        rustToolchain = import ./nix/rust-toolchain.nix {
          inherit self system fenix;
          pkgs = pkgs;
        };
        slintPkgs = import ./nix/slint.nix {inherit self system pkgs;};
        cosign2Pkgs = import ./nix/cosign2.nix {inherit self system pkgs;};
        localazy = import ./nix/localazy.nix {inherit self system pkgs;};
      in
        ciPkgs // rustToolchain // slintPkgs // cosign2Pkgs // localazy;

      buildPackages = with pkgs;
        [
          bc
          taplo
          cmake
          curl
          gcc-arm-embedded
          git
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
          # For Ledger SDK icon2glyph.py
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

      mkShell = packages:
        pkgs.mkShellNoCC (
          {
            strictDeps = true;
            packages = packages;
            hardeningDisable = ["all"];
            buildInputs = with pkgs;
              [
                fontconfig
                pcsclite
                libusb1
                zlib
              ]
              ++ darwinPkgs
              ++ lib.optionals stdenv.isLinux [udev];

            LD_LIBRARY_PATH = with pkgs;
              lib.makeLibraryPath (
                [
                  fontconfig
                  pcsclite
                  libusb1
                  zlib
                  # slint sim
                  libxkbcommon
                  llvmPackages.libclang.lib
                ]
                ++ darwinPkgs
                ++ lib.optionals stdenv.isLinux [
                  # slint sim
                  xorg.libX11
                  xorg.libXcursor
                  xorg.libXi
                  wayland
                  udev
                ]
              );

            shellHook = ''
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
