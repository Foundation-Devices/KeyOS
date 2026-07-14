# SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: GPL-3.0-or-later
{pkgs}: let
  version = "v1.17.0-foundation2";
  foundationSlint = pkgs.callPackage ./foundation-slint.nix {
    inherit version;
    hash = "sha256-+eriY9l5KFrJFVau27mScWvMemPFx6op5iSI5MvMWBE=";
  };
in {
  # https://github.com/NixOS/nixpkgs/blob/nixos-unstable/pkgs/by-name/sl/slint-lsp/package.nix
  slint-lsp = pkgs.slint-lsp.overrideAttrs (old: {
    pname = "foundation-slint-lsp";
    inherit version;
    src = foundationSlint.source;

    cargoDeps = pkgs.rustPlatform.importCargoLock {
      lockFile = "${foundationSlint.source}/Cargo.lock";
    };
    buildAndTestSubdir = "tools/lsp";

    doCheck = false;
    auditable = false;
    doInstallCheck = false;
  });

  # https://github.com/NixOS/nixpkgs/blob/nixos-unstable/pkgs/by-name/sl/slint-viewer/package.nix
  slint-viewer = pkgs.slint-viewer.overrideAttrs (old: {
    pname = "foundation-slint-viewer";
    inherit version;
    src = foundationSlint.source;

    cargoDeps = pkgs.rustPlatform.importCargoLock {
      lockFile = "${foundationSlint.source}/Cargo.lock";
    };
    buildAndTestSubdir = "tools/viewer";
    # Explicit backend selection: the default `backend-default` feature pulls
    # in i-slint-backend-qt, so nixpkgs' package builds the Qt backend and the
    # previewed backend would diverge from the SDK viewer. Selecting winit
    # directly keeps Qt out of the dependency graph entirely, so the Qt inputs
    # inherited from nixpkgs can be dropped as well.
    cargoBuildFlags = [
      "--no-default-features"
      "--features"
      "backend-winit,renderer-femtovg,renderer-software,custom-translations"
    ];
    buildInputs =
      builtins.filter (
        drv: !(pkgs.lib.hasPrefix "qt" (drv.pname or drv.name))
      )
      old.buildInputs;
    nativeBuildInputs =
      builtins.filter (
        drv: !(pkgs.lib.hasPrefix "wrap-qt" (drv.pname or drv.name))
      )
      old.nativeBuildInputs;

    doCheck = false;
    auditable = false;
    doInstallCheck = false;
  });
}
