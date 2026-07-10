# SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: GPL-3.0-or-later
{
  lib,
  stdenv,
  writeShellApplication,
  libxkbcommon,
  wayland,
  xorg,
}: {
  sim-runner = writeShellApplication {
    name = "sim-runner";
    text = ''
      export LD_LIBRARY_PATH="${
        lib.makeLibraryPath (
          [libxkbcommon]
          ++ lib.optionals stdenv.isLinux [
            wayland
            xorg.libX11
            xorg.libXcursor
            xorg.libXi
          ]
        )
      }''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
      export SLINT_BACKEND=winit-software
      exec cargo xtask run --hosted "$@"
    '';
  };
}
