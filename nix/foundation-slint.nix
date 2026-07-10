# SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: MIT
{
  lib,
  stdenv,
  rustPlatform,
  fetchFromGitHub,
  cmake,
  pkg-config,
  fontconfig,
  libGL,
  xorg,
  libxkbcommon,
  wayland,
  localSrc ? null,
  version,
  hash,
}: let
  src =
    if localSrc != null && builtins.pathExists (localSrc + "/tools/viewer/Cargo.toml")
    then localSrc
    else
      fetchFromGitHub {
        owner = "Foundation-Devices";
        repo = "slint";
        rev = version;
        inherit hash;
      };

  linuxViewerLibs = [
    fontconfig
    xorg.libX11
    xorg.libXcursor
    xorg.libXi
    xorg.libxcb
    libxkbcommon
    wayland
  ];
in {
  source = src;

  foundation-slint-viewer = rustPlatform.buildRustPackage {
    pname = "foundation-slint-viewer";
    inherit version src;

    cargoLock.lockFile = "${src}/Cargo.lock";
    buildAndTestSubdir = "tools/viewer";
    # Explicit backend selection: the default `backend-default` feature pulls
    # in i-slint-backend-qt, whose build script probes for qmake. Selecting
    # winit directly keeps Qt out of the dependency graph entirely.
    cargoBuildFlags = [
      "--no-default-features"
      "--features"
      "backend-winit,renderer-femtovg,renderer-software,custom-translations"
    ];

    nativeBuildInputs = [
      cmake
      pkg-config
    ];

    buildInputs =
      [
        libGL
      ]
      ++ lib.optionals stdenv.hostPlatform.isLinux linuxViewerLibs;

    doCheck = false;

    postInstall = ''
      if [ -x "$out/bin/slint-viewer" ]; then
        mv "$out/bin/slint-viewer" "$out/bin/foundation-slint-viewer"
        ln -sf "$out/bin/foundation-slint-viewer" "$out/bin/slint-viewer"
      fi
    '';

    meta = {
      description = "Foundation-customized Slint viewer";
      homepage = "https://github.com/Foundation-Devices/slint";
      license = with lib.licenses; [gpl3Only];
      mainProgram = "foundation-slint-viewer";
      platforms = lib.platforms.linux ++ lib.platforms.darwin;
    };
  };
}
