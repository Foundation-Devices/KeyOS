# SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: MIT
{
  description = "Foundation SDK maintainer development and release environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-24.11";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = inputs:
    (import ./nix/maintainer-flake.nix {
      root = ./.;
      # `just build` copies foundation-slint.nix into this flake's own nix/
      # so the standalone flake stays self-contained under a `path:` store
      # copy (where `../nix` would escape the tree and fail pure eval). Fall
      # back to the repo-level nix/ for in-tree use.
      commonNixRoot =
        if builtins.pathExists ./nix/foundation-slint.nix
        then ./nix
        else ../nix;
    }).outputs
    inputs;
}
