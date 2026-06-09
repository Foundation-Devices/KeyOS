# SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: GPL-3.0-or-later
{
  self,
  system,
  pkgs,
}: let
  version = "v1.12.1-foundation6";
  foundationSlint = pkgs.callPackage ./foundation-slint.nix {
    inherit version;
    hash = "sha256-sCOZ+aXKmx+c2sfnNhjjM+oEUHQpBX2s54LNPTrDKTE=";
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
    cargoBuildFlags = ["--features" "custom-translations"];

    doCheck = false;
    auditable = false;
    doInstallCheck = false;
  });
}
