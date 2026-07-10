# SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: GPL-3.0-or-later
{pkgs}: {
  localazy = pkgs.stdenv.mkDerivation rec {
    pname = "localazy-cli";
    version = "2.0.8";

    src = pkgs.fetchurl {
      url = "https://registry.npmjs.org/@localazy/cli/-/cli-${version}.tgz";
      sha256 = "sha256-XJC010Mz2B+VVExEDenliccqwrt99Do0R6C+xGtzLws=";
    };

    installPhase = ''
      mkdir -p $out/lib
      tar -xzf $src -C $out/lib

      mkdir -p $out/bin
      cat > $out/bin/localazy << EOF
      #!/bin/sh
      exec ${pkgs.nodejs}/bin/node $out/lib/package/index.js "\$@"
      EOF
      chmod +x $out/bin/localazy
    '';
  };
}
