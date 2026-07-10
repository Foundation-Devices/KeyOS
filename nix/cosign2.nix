# SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: GPL-3.0-or-later
{
  self,
  pkgs,
}: let
  src = pkgs.stdenv.mkDerivation {
    name = "cosign2-src";
    src = self + "/imports/cosign2";
    installPhase = ''
      cp -r . $out
    '';
    outputHash = "sha256-h9QFHyHO0bvQEPZ5sIbrTj0HOg5YKoqse0O2FR87yRI=";
    outputHashMode = "recursive";
  };
in {
  cosign2 = pkgs.rustPlatform.buildRustPackage {
    name = "cosign2";
    inherit src;
    cargoLock = {
      lockFile = src + "/Cargo.lock";
    };
  };
}
