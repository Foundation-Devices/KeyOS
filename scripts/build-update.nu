#!/usr/bin/env -S nix shell --inputs-from . nixpkgs#nushell --command nu
# SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: GPL-3.0-or-later

# build a development boot image and signed update from two commits
# firmware builds use each commit's xtask in this checkout
# the current checkout supplies tree export and patch generation
# example: ./scripts/build-update.nu 973219ab498a b97f4c629105 --out updates/test
def main [
    base: string # source commit or ref to build into boot.img
    target: string # destination commit or ref to install with the update
    --out: path # artifact directory, overwrites generated files, defaults to target/update-<base>-<target>
    --base-version: string # source version, defaults to KEYOS_VERSION in the source commit
    --target-version: string # destination version, defaults to KEYOS_VERSION in the destination commit
    --strategy: string = 'normal' # patch encoder: normal or forward, which avoids backward and overlapping source reads
    --rebuild # rebuild firmware even when a matching signed boot image already exists
] {
  if $strategy not-in ['normal' 'forward'] {
    error make {msg: 'strategy must be normal or forward'}
  }
  let repo = $env.FILE_PWD | path dirname
  if (^git -C $repo status --porcelain --untracked-files=no | str trim) != '' {
    error make {msg: 'commit or stash tracked changes before building updates'}
  }
  let original = try {
    ^git -C $repo symbolic-ref --quiet --short HEAD | str trim
  } catch {
    ^git -C $repo rev-parse HEAD | str trim
  }
  let base_sha = ^git -C $repo rev-parse --verify $'($base)^{commit}' | str trim
  let target_sha = ^git -C $repo rev-parse --verify $'($target)^{commit}' | str trim
  let cache = $repo | path join target
  let output = $out | default (
    $repo
    | path join target $'update-($base_sha | str substring 0..<12)-($target_sha | str substring 0..<12)'
  ) | path expand
  let builds = [
    {name: base, commit: $base_sha, version: $base_version}
    {name: target, commit: $target_sha, version: $target_version}
  ] | each {|item|
        let version = if $item.version != null { $item.version } else {
            ^git -C $repo show $'($item.commit):xtask/src/main.rs'
            | parse -r 'const KEYOS_VERSION: &str = "(?<version>[^"]+)";'
            | get version | first
        }
        $item | merge {
            version: $version
            epoch: (^git -C $repo show -s --format=%ct $item.commit | str trim)
            tree: ($cache | path join update-trees $'($version)-($item.commit | str substring 0..<12)')
        }
    }
  mkdir $cache $output
  $env.CARGO_TARGET_DIR = $cache
  $env.CARGO_TERM_COLOR = 'always'
  log $'building ($builds.0.version) -> ($builds.1.version) with ($strategy) patches'
  log $'original checkout: ($original)'
  log $'output: ($output)'
  log 'preparing build tools'
  let config = $repo | path join cosign2.toml
  let signing = {
    config: (open --raw $config | hash sha256)
    signer: (open --raw (open $config | get secret) | hash sha256)
  }
  cd $repo
  run-build 'build tools' cargo build --release --locked -p xtask
  let tools = $cache | path join release xtask
  try {
    for item in $builds {
      let tree = $item.tree
      rm --recursive --force $tree
      let inputs = $signing | merge {version: $item.version}
      let saved = $cache | path join update-builds $item.commit
      let record = $saved | path join inputs.json
      let image = $saved | path join boot.img
      let reuse = not $rebuild and ($image | path exists) and ($record | path exists) and (open $record) == $inputs
      if $reuse {
        log $'reusing signed ($item.name): ($item.version) at ($item.commit | str substring 0..<12)'
      } else {
        log $'building ($item.name): ($item.version) at ($item.commit | str substring 0..<12)'
        (run-step $'checkout ($item.name)'
          git -C $repo switch --detach $item.commit
        )
        $env.KEYOS_SOURCE_DATE_EPOCH = $item.epoch
        run-build $'build ($item.name) xtask' cargo build --locked -p xtask
        let xtask = $cache | path join debug xtask
        run-build $'build ($item.name) bootloader' $xtask build-bootloader
        (run-build $'build ($item.name) recovery' $xtask build
          --is-recovery --dont-sign --reproducible --keyos-version $item.version)
        (run-build $'build ($item.name) firmware' $xtask build
          --dont-sign --reproducible --keyos-version $item.version)
        let built = $cache | path join armv7a-unknown-xous-elf release
        let app_files = glob ($built | path join 'keyos/apps/*/manifest.json') | each {|manifest|
                    let version = open $manifest | get version
                    [{file: ($manifest | path dirname | path join app.elf), version: $version}, {file: $manifest, version: $version}]
                } | flatten
        let sign_files = [
          {
            file: ($built | path join images app.bin)
            version: $item.version
          }
          {
            file: ($built | path join images recovery.bin)
            version: $item.version
          }
        ] | append $app_files
        log $'signing ($sign_files | length) files for ($item.name)'
        for file in $sign_files {
          let name = $file.file | path relative-to $built
          (run-build $'sign ($item.name) ($name)' cosign2 sign
            -c $config -i $file.file --developer --in-place --binary-version $file.version)
        }
        run-build $'build ($item.name) image' $xtask build-firmware-image
        mkdir $saved
        rm --force $record
        ^cp --sparse=always ($repo | path join boot.img) $image
        $inputs | to json | save $record
      }
      run-build $'export ($item.name) tree' $tools export-update-tree $image --out $tree
    }
  } finally {
    run-step 'restore checkout' git -C $repo checkout $original
  }
  # copy the base boot.img last 
  ^cp --sparse=always ($cache | path join update-builds $builds.0.commit boot.img) ($output | path join boot.img)
  $env.KEYOS_SOURCE_DATE_EPOCH = $builds.1.epoch
  let patch = $output | path join update.tar
  (run-build 'build patch' $tools build-update
    $builds.0.version $builds.0.tree $builds.1.version $builds.1.tree
    --reboot-required --strategy $strategy --force --out $patch)
  (run-build 'sign update' cosign2 sign
    -c $config -i $patch --developer --in-place --binary-version $builds.1.version)
  {builds: $builds, strategy: $strategy, production: false} | to json | save --force ($output | path join commits.json)
  log $'artifacts: ($output)'
  log $'update size: (ls ($output | path join update.tar) | get size | first)'
}

def log [message: string] {
  const script_name = path self | path parse | get stem
  print $'(ansi cyan_bold)[($script_name)](ansi reset) (date now | format date "%H:%M:%S") ($message)'
}

def --wrapped run-build [step: string, ...args: string] {
  run-step $step nix develop .#build --command ...$args
}

def --wrapped run-step [step: string, command: string, ...args: string] {
  let started = date now
  log $'starting ($step)'
  try {
    ^$command ...$args out+err>| ^cat
  } catch {
    let invocation = [$command ...$args] | each {|arg| $arg | to nuon } | str join ' '
    error make {msg: $'command failed: ($invocation)'}
  }
  log $'finished ($step) in (((date now) - $started) | format duration sec)'
}
