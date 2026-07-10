# SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: GPL-3.0-or-later
{
  lib,
  stdenv,
  writeShellApplication,
  fontconfig,
  libxkbcommon,
  wayland,
  xorg,
}: {
  slint-runner = writeShellApplication {
    name = "slint-runner";
    text = ''
      export LD_LIBRARY_PATH="${
        lib.makeLibraryPath (
          [libxkbcommon]
          ++ lib.optionals stdenv.isLinux [
            fontconfig
            wayland
            xorg.libX11
            xorg.libXcursor
            xorg.libXi
          ]
        )
      }''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
      export SLINT_BACKEND=winit-software
      exec "$@"
    '';
  };
}
