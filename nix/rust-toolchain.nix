# SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: GPL-3.0-or-later
{
  self,
  system,
  pkgs,
  fenix,
}: let
  toolchainSha256 = "sha256-NvWKV8CXj8AQXESvz5uGr6qv0JF0UHUdjYb2murEG/A=";

  baseToolchain = fenix.packages.${system}.fromToolchainFile {
    file = self + "/rust-toolchain.toml";
    sha256 = toolchainSha256;
  };

  armv7aStd = fenix.packages.${system}.targets.armv7a-none-eabi.fromToolchainFile {
    file = self + "/rust-toolchain.toml";
    sha256 = toolchainSha256;
  };

  channel = (builtins.fromTOML (builtins.readFile (self + "/rust-toolchain.toml"))).toolchain.channel;
  customTargetLib = pkgs.fetchzip {
    url = "https://github.com/Foundation-Devices/rust-keyos/releases/download/1.96.0-${channel}/armv7a-unknown-xous-elf_${channel}.zip";
    sha256 = "sha256-BvQyJ6BfMeaqGjSeE28iMKXpiQcuIMuW02XxMS9Pcrw=";
    stripRoot = false;
  };
in {
  rust-keyos = fenix.packages.${system}.combine [
    baseToolchain
    armv7aStd
    customTargetLib
  ];
  rust-analyzer = fenix.packages.${system}.rust-analyzer;
}
