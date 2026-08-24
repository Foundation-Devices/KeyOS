<!--
SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Development

This document describes how to develop for KeyOS. The instructions below describe how to set up the development environment and build KeyOS on a system running **Ubuntu 22.04** or **Ubuntu 24.04**. Ubuntu 22.04 is used for official KeyOS builds.

## Setup

In order to build KeyOS images, you need to:

- Get the source code
- Install the dependencies
- Customize for your build environment
- Run the build or run command

### Get the Source Code

Make sure `git` is installed:

    sudo apt install git

Configure it to use your favorite editor for writing commit messages:

    git config --global core.editor "vim"

The instructions below assume you are installing into your home folder at `~/keyOS`. You can choose to install to a different folder, and just update command paths appropriately. While this repo is private before launch, this will require using [ssh with github](https://docs.github.com/en/authentication/connecting-to-github-with-ssh/adding-a-new-ssh-key-to-your-github-account).

    cd ~/
    git clone https://github.com/Foundation-Devices/KeyOS.git
    cd KeyOS

Foundation requires commits to be linted and to have specific commit messages in order to be merged.

    cp .githooks/* .git/hooks/

### Development Dependencies

You have two options for installing development dependencies:
- **[Nix install](#nix-install)** (recommended): Provides a fully configured, development environment
- **[Manual install](#manual-install)**: if you prefer not to use Nix

#### Nix install

> supports x86-linux, arm64-linux, arm64-darwin

- Go through the install process of [Determinate Nix](https://docs.determinate.systems/determinate-nix/).
- Navigate to the KeyOS project directory: `cd ~/keyOS`
- Run `nix develop` to enter the development environment
    - the first time this runs, it will take a few minutes to download dependencies
    - follow up runs will be instant
    - to exit the nix environment, type `exit` or press `Ctrl+D`
- Verify the environment is working: `cargo --version && just --version`

##### Preserving Your Shell Configuration

If you find yourself not in your default shell when running `nix develop`, you may want to run:

```
nix develop -c $SHELL
```

This will start your preferred shell (bash, zsh, etc.) with your personal config intact.


#### Manual install

This project is primarily developed in Rust. You may need `curl` to install it. KeyOS for Passport Prime also has a few other library and build dependencies here:

    sudo apt install curl gcc-arm-none-eabi llvm libclang-dev clang pkgconf libpcsclite-dev libfontconfig-dev build-essential reuse cmake

Follow the Rust installation instructions [here](https://www.rust-lang.org/tools/install). This will install `rustup` and `cargo`, but you may need to restart your terminal to update your `PATH` to include them. Afterwards, add the armv7a target to rust:

    rustup target add armv7a-none-eabi

We use a set of `Justfile` command scripts. Using these commands requires that you first install the `just` command runner. Also install the tools used in our scripts:

    cargo install just
    cargo install cargo-audit cargo-sort slint-lsp

Install the `cosign2` tool to complete and sign builds.

    cargo install --path imports/cosign2/cosign2-bin

Generate cosign2 developer keys with `scripts/generate-cosign2-dev-key.sh`

Now you will be able to download and install the KeyOS toolchain:

    cargo xtask install-toolchain

If there are toolchain updates, you would need to run:

    cargo xtask install-toolchain --reinstall

## Customizing

You can define some environment variables to customize the debug scripts to your specific need. Rename `env-example` into `.env` and edit it to your need.

## Swap

I have run out of memory many times when compiling KeyOS, so I recommend increasing your system's swap space up to 32G, especially for systems with lots of threads. I found [this guide](https://itsfoss.com/create-swap-file-linux/) helpful. Ubuntu 24.04 has `/swap.img` by default, so you may want to jump to the [resizing instructions](https://itsfoss.com/create-swap-file-linux/#resizing-swap-space-on-linux).

## Building

Build the complete package of the bootloader, recovery, and firmware for Passport Prime, or just the firmware:

    just build-all
    just build

### Building with UART console I/O disabled (for production)

A final firmware should not print anything to its UART console, as this can be a security risk.
To disable UART console I/O, supply any build command with (among other flags) a `--no-logging` flag, e.g.:

    cargo xtask build-all --no-logging

## Passport Udev Rules

Passport Prime can be debugged and flashed via USB, add these [udev rules](utils/passport-drive/99-passport.rules) on Linux systems to enable these options.

## Simulator

Run the simulator:

    just sim

### Hot Reload

While the simulator is running, an app can be rebuilt and relaunched
in place, without restarting the whole simulator:

    just sim-reload <crate>

For example:

    just sim-reload gui-app-lock-screen

To rebuild and reload automatically on every change to the crate's source directory
(requires `cargo install cargo-watch`):

    just sim-watch <crate>

Note that watch mode watches the crate's own directory plus the shared UI library
(`ui/ui`). Changes to other shared code won't trigger a reload — run
`just sim-reload <crate>` manually in that case; the rebuild picks up shared changes.

Hot reload works for apps only. System services whose exit takes
the hosted system down with them (see `SYSTEM_SERVICES_HOSTED` in `xtask/src/main.rs`,
e.g. `gui-server` or `fs-server`) require a full simulator restart.

### Driving the simulator

The running simulator serves the same debug protocol as a device does over USB, on
`127.0.0.1:7664` (override with `KEYOS_SIM_DEBUG_ADDR` on both ends). Point
`passport-drive` at it with `--sim` to get the commands you would run against hardware:

    cargo run -p passport-drive -- --sim screenshot -o /tmp/sim.png
    cargo run -p passport-drive -- --sim tap 240 400

The MCP server takes the same flag, and is registered in `.mcp.json` as `passport-sim`
alongside the device server:

    cargo run -p passport-drive -- --sim mcp

Commands that need real hardware (SAM-BA, HID APDUs, USB enumeration, kernel debug
commands) are refused with an explanation rather than attempted.

## Installation

### Full System Reprogram with SAM-BA

This method will completely erase the boot and system volume (encrypted volume will be kept), and update all the software on the device.
It's most convenient if many apps have been modified, and nothing too important for development is saved on the system volume.

First install `sam-ba` by downloading the latest `tar` file [here](https://www.microchip.com/en-us/development-tool/SAM-BA-In-system-Programmer#Software), extracting it with `tar xvf <sam-ba.tar.gz>`, and adding the resulting directory to your system `$PATH` in `~/.bashrc`.

    export PATH=$PATH:/home/<username>/<path-to-samba-files>

Refresh your shell for this change to take effect, either by closing your terminal and opening a new one, or running `source ~/.bashrc`.

Make sure your username is added to the `dialout` group on your system:

    sudo usermod -a -G dialout <username>

Log out of your session, then log back in for this change to take effect.

Connect your Passport Prime to your computer via USB. Then enter the SAM-BA mode:

1. Hold the power button down for 10 seconds to reboot the device
2. As soon as the logo is shown, click the power button 3 or more times to enter the Boot Menu
3. Tap the SAM-BA mode option, the device screen will turn black
4. A new serial USB device should appear on the computer

You can confirm that Passport Prime is connected in SAM-BA mode by running `lsusb`, and finding `Atmel Corp. at91sam SAMBA bootloader`.

Compile everything necessary for the system, and flash it to the device:

    just build-all
    just flash

Once the flashing process has started, make sure not to disconnect Passport Prime from the USB port until the process is complete.

### Non-System Apps

Non-system apps like `gui-app-authenticator` are built as part of `just build-all`. You can find their binaries at `keyOS/target/armv7a-unknown-xous-elf/release/keyos/apps/<app-name>/app.elf`.

To install them on hardware, connect your Passport Prime to your computer via USB, and ensure USB is enabled on the device. Then copy your `app.elf` to `/media/<username>/PRIME/keyos/apps/<app-name>/app.elf`. Eject the device from your computer, and hold the power button down for 10 seconds to reboot and load the new version of the app.

### System Services and Kernel

System services or apps like `gui-server` and `gui-app-qr-scanner`, as well as the system kernel, are built as part of `just build-all` into `keyos/target/armv7a-unknown-xous-elf/release/images/app.bin`.

To install them on hardware, connect your Passport Prime to your computer via USB, and ensure USB is enabled on the device. Then copy `app.bin` to `/media/<username>/PRIME/app.bin`. Eject the device from your computer, and hold the power button down for 10 seconds to reboot and load the updated firmware.

### Recovering a Bricked Device

If your device has been bricked, and cannot be updated normally, power it on, connect it to your computer via USB, remove the screen carefully without disconnecting the screen connector, and lay the screen to the left of the device on its side. At the top of the PCB, to the right of the screen connector, you should see a resistor, and two contacts to the right of it. Higher on the board in this area, you should see "CD", with contacts on either side of it:

![Contacts to be Shorted](/media/contacts.jpeg)

Using a conductor, like a male-male dupont wire, short the contact to the left of "CD" with the contact on the left side of the other pair, and hold this short while holding the power button for 10 seconds to reboot:

![Short CD](/media/short_cd.jpeg)

Once the screen flickers and is blank but powered, remove the short. The device is now in SAM-BA mode, and the [Full System Reprogram](#Full-System-Reprogram-with-SAM-BA) instructions can be followed to recover the device. Make sure the software being installed won't cause the same problem.

## Viewing Logs

KeyOS provides a log viewer tool that can display logs from the device in real-time via serial connection or from saved log files.
The viewer automatically detects and visually separates multiple logging sessions (e.g., after the device restarts).

### Viewing Serial Logs

To view logs from a connected device via serial port:

    just logs

The log viewer auto-discovers the KeyOS USB serial interface by VID:PID (`0x1307:0x0165`) and reconnects after device reboots.

### Viewing Log Files

To view logs from a previously saved log file:

    just logs-file <file>

Replace `<file>` with the path to your log file.

## Cleaning

If for some reason you need to clean out existing compiled objects, use `just clean` or `cargo clean`:

    just clean

## UI Development with Slint in VSCode

We recommend using [VSCode](https://code.visualstudio.com/) with the [Slint plugin](https://marketplace.visualstudio.com/items?itemName=Slint.slint) to preview UIs during development for KeyOS. Navigate to Manage (Gear Icon) > Settings > Search settings for "slint" > Slint: Library Paths (Edit in settings.json), and add this line to the "slint.libraryPaths" list:

    "ui": "<absolute path to KeyOS>/ui/ui",

Slint preview of images does not work if you are using a Rust callback (`Utils.common-image`) to fetch the raw pixels on target. This is most of the images so, for ease of development, one can use `just fix-preview` to temporarily modify .slint files to use path literals with `@image-url` instead. Run `just unfix-preview` to revert before committing any changes.

## Contributing

Foundation requires commits to be signed with GPG keys in order to be merged. Follow [GitHub's guide to commit signature verification](https://docs.github.com/en/authentication/managing-commit-signature-verification) to get started. You may also want to configure git to automatically sign commits in this repo by following [this guide](https://docs.github.com/en/authentication/managing-commit-signature-verification/telling-git-about-your-signing-key), and omitting the `--global` flag in the commands.

## New KeyOS App IDs

New KeyOS app IDs are 128 bit (32 character) hashes of their app names. These are found in each app's `manifest.toml`. These can be generated using `just app-id <app-name>`.

## Rust Code Standards

### Functional Code

New crates should attempt to build most of their own functionality into "functional" code, which is deterministic with respect to its inputs and outputs, and hase no side-effects. These functions can be [unit tested](#Functional-Unit-Tests) easily, while other code can be tested using [integration tests](#Integration-Tests). See more explanation in the [testing](#Testing) section.

### Abstract Code

New crates don't need to be over-engineered for maximum abstraction, but they should be written in a way that is easy to extend. If a situation arises where portions of functionality need to be changed or reused, consider making a more abstract function. See more in the [changing functionality](#Changing-Functionality) section.

### Avoid Panicking

Panics interrupt the user experience of KeyOS, and make debugging problems difficult. Avoid functions that can cause panics like `unwrap`, `expect`, and `unreachable`. Instead, if panicking isn't completely necessary, use error propagation for unrecoverable errors, and error logging for recoverable errors. KeyOS apps will often have to decide if errors propagated up to them are recoverable or not, and either log a warning, display a warning to the user, or fail as gracefully as possible. You can check your code for `unwrap` and `expect`:

    just panic-lint

### Unrecoverable Errors

These are errors that completely prevent the rest of a function from being executed. Rust's enums allow all possible errors that can be encountered by a crate to be expressed as a single `enum`, whose variants can encapsulate errors returned by other crates, or new structs and enums that describe error states in the current crate. Combined with `#[derive(thiserror::Error)]`, this can be used to produce detailed errors that the caller can understand and decide to recover, propagate, or panic, if continuing is impossible. When possible, use a unique error variant for each error case within a function, so that a caller can know exactly where the error originated. The following example is from `apps/gui-app-authenticator/src/error.rs`, and you can see in this crate how errors are encapsulated and propagated by using `Result<Value, TotpError>` as the return type for many functions.

    #[derive(Debug, thiserror::Error)]
    pub enum TotpError {
        #[error("Could not parse invalid TOTP URL: {0:?}")]
        UrlParseError(totp_rs::TotpUrlError),
        #[error("Could not parse old TOTP URL: {0:?}")]
        UrlParseOldError(totp_rs::TotpUrlError),
        #[error("Could not parse new TOTP URL: {0:?}")]
        UrlParseNewError(totp_rs::TotpUrlError),
        #[error("This TOTP is already registered: {0:?}")]
        DuplicateError(Contains),
        #...
    }

Add `thiserror` to your crate's Cargo.toml to use this derived trait, which makes future debugging easy with human-readable error messages.

### Recoverable Errors

These are errors that can be circumvented by using default values, or following a different execution path. However, they should not be ignored, because they may cause unexpected behavior in the future. KeyOS has a logging system, which can be used to log info, debug data, and warnings that describe errors. Even in cases where errors could be easily ignored using `unwrap_or`, it would be more helpful to future debuggers to use a `match` statement or `unwrap_or_else` to log a warning that an error was recovered from.

    log::info!("Device disconnected");
    log::debug!("Enabling OTG_ID IRQ");
    log::warn!("Error during ehci work(): {e:?}");
    log::error!("Fatal error: {}", e);

Add the following to your crate's Cargo.toml file to use the KeyOS logging system:

    log = "0.4.14"
    log-server = { package = "xous-api-log", path = "../../xous/api/log" }

### Indexing

**Always** check that indices are not out of bounds before using them. Even though some collections and indices may be hard-coded, if either changes to make the index out of bounds, a panic could occur that could have been avoided.

### No New Warnings

New and modified code should be warning-free. If a warning absolutely can not be avoided, use `#allow[]` to remove it from the compiler output.

    // Inner attribute applies to the entire function.
    fn some_unused_variables() {
      #![allow(unused_variables)]

      let x = ();
      let y = ();
      let z = ();
    }

### Formatting

Formatting can be applied easily using `just fmt`, and checked using `just lint`. `cargo fmt` is used to format code, and `cargo sort` is used to sort dependencies. [Reuse](https://github.com/fsfe/reuse-tool) is used to ensure licensing info is included for every file.

    just fmt
    just lint

The pre-push git hook runs `just short-lint` and rejects the push if it finds lint errors. Run `just fmt` to fix formatting issues; `reuse` lint errors must be fixed by hand. Commit the fixes and push again. The same checks can be run any time with `just short-lint` (or the full `just lint`).

Code files should start with a comment that includes license info to comply with `reuse` lints, for example:

    // SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
    // SPDX-License-Identifier: GPL-3.0-or-later

Binary files like images that are not ignored by `.gitignore` should be added to `.reuse/dep5`:

    Files:
      media/*
    Copyright: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
    License: GPL-3.0-or-later

### Real-time tracing with SEGGER SystemView

To get a real-time insight into processes, IRQs and syscalls, KeyOS supports [SEGGER SystemView] tracing.
This tool allows you to trace the execution of the OS processes and threads, visualize the timing of events.

- Install the SystemView application on your computer
- Ensure JLink is connected to the target device via the debug board
- Run `cargo xtask run --with-systemview`, this will make the kernel wait for the SystemView recorder to connect before starting
- Configure SystemView with the following RTT control block address: `0xbeef0410` <!-- depends on RTT_CONTROL_BLOCK_VIRT_ADDR -->

[SEGGER SystemView]: https://www.segger.com/products/development-tools/systemview/

### Slint Previews

Slint provides two tools for previewing `.slint` files:

1. `slint-viewer` - a standalone application that can be used to preview `.slint` files.
2. Visual Studio Code Slint Plugin - a plugin for Visual Studio Code that can be used to preview `.slint` files. It can be installed from the Visual Studio Code extensions marketplace.

These tools work, but there are some issues with them where some code will not render at all. Sometimes the `slint-viewer` is able to render code that the VSCode plugin can not. If you are having issues with the VSCode plugin, try using the `slint-viewer` instead.

#### Installing The KeyOS `slint-viewer`

For KeyOS development, install the custom `slint-viewer` from our Slint fork at the latest tagged version:

    git clone https://github.com/Foundation-Devices/slint.git
    cd slint
    git checkout v1.12.1-foundation6
    cargo install --path tools/viewer --no-default-features \
        --features backend-winit,renderer-femtovg,renderer-software,custom-translations --force

To view a slint file with KeyOS translations:

    just preview <path to .slint file> <optional locale code> <optional i18n directory>

The script automatically detects the i18n translation directory based on the app directory you launch the command from, but you can override this by specifying a custom i18n directory path.

#### Embedded Slint Previews

Slint does not have support for certain graphics primitives in the embedded systems software renderer. This mianly includes things like shadows and gradients. For KeyOS, we
have implemented Rust functions using the Skia library to render these primitives. Unfortunately, however, the preview tools are not able to use these functions. This means that
the previews will not be accurate.

To work around this, we have implemented a preview mode that can be enabled by running the following command:

```
just fix-preview
```

This swaps out some embedded code for some code that runs in the preview tools and in the simulator. This allows the previews to be accurate and makes iterating on the UI much easier. To revert to the original code, run the following command:

```
just unfix-preview
```

These features are implemented by the two tools in the following folders:

- `utils/update-slint-preview` - This tool updates the preview code to match the embedded code. This is useful when the embedded code changes and the preview code needs to be updated.

Whenever you run `just fix-preview`, it will run `update-slint-preview` to make sure the common images are all available for the preview code.

## Testing

New or modified code should have a thorough set of unit tests that cover functionality specific to that code, and apps should have integration tests that ensure their functionality. When a crate is created or modified, add it to the list of crates tested under `just unit-test`, if it isn't included already. When an app is created, make integration tests and add them to `just integration-test` along with the servers the app depends on. These commands test all crates and apps that have working tests, and run in GitHub actions upon push to prevent regressions.

### Functional Unit Tests

Code that interfaces with other KeyOS servers is more difficult to unit test, because the code would need to be designed to allow dependency injection of a mock of the other KeyOS servers it uses. Therefore, the more functionality that can be made "functional", and separated from KeyOS dependencies, the easier it is to cover more code with simpler tests.

### Integration Tests

Integration tests can be used to test high-level functionality that spans multiple KeyOS servers. Examples can be found in `integration-tests/`. They require access to public APIs for the functionality under test, and must be added to `Cargo.toml` as workspace members, and included in the `just integration-test` recipe along with the servers they depend on. These integration tests function by shutting down KeyOS and setting an exit code when they `pass()` or `fail()`. These functions are found in the `integration-tests/keyos-integration-test` crate.

    if totps[0].account_name != String::from("@foundationdvcs") {
        fail("Failed to edit account name".into());
    }

    pass();

### Changing Functionality

Any breaking change to an API must result in an update of its crate's [semantic version](https://semver.org/) "major" number. If the intention of an API function needs to be changed, even if its interface remains the same, sometimes it would be best to make a new function, either named with a version number like "v2", or named in a way that describes the difference in functionality. This makes you consider the changes in integration that are required in all crates that depend on the API, and potentially allow some crates to continue using the old version. For example, if you had a KeyOS server that concatenates a vector of strings into a specific case, and wanted to change its behavior, you would consider a few options.

    fn concat(strings: Vec<String>) -> String {
        strings.iter()
            .map(|word| word.to_uppercase())
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn get_strings() -> Vec<String> {
            vec![String::from("hello"),
                String::from("world"),
                String::from("rust")]
        }

        #[test]
        fn original_concat() {
            assert_eq!(String::from("HELLOWORLDRUST"), concat(get_strings()));
        }
    }

If some apps and servers still require the old behavior, create a new function and corresponding tests:

    fn concat_v2(strings: Vec<String>) -> String {
        strings.iter()
            .map(|word| word.to_uppercase())
            .collect::<Vec<String>>()
            .join("_")
    }

    #[test]
    fn test_concat_v2() {
        assert_eq!(String::from("HELLO_WORLD_RUST"), concat_v2(get_strings()));
    }

This particular case, and many you might encounter, would be better to fix by making the function more abstract. Allowing the caller to pass in parameters that determine the concatenation strategy could reduce the need for future interface changes, and allow the same function to add new behaviors without changing old ones, but increases the number of tests needed to handle different inputs:

    use convert_case::{Case, Casing};

    fn concat_v3(strings: Vec<String>, delimiter: &str, case: Case) -> String {
        strings.iter()
            .map(|word| word.to_case(case))
            .collect::<Vec<String>>()
            .join(delimiter)
    }

    #[test]
    fn test_concat_v3() {
        assert_eq!(String::from("Hello-World-Rust"), concat_v3(get_strings(), "-", Case::Title));
    }

In more extreme scenarios, update the behavior and tests of the old function if this must be a system-wide policy change applied to all apps and servers.

Old API functions and their corresponding messages can be phased out using deprecation warnings in the API, and eventually deleting them in favor of the newer APIs and messages.

    #[deprecated(since="0.5.0", note="please use `concat_v4` instead")]
    fn concat_v3(...) {...}

### Coverage

Unit tests should cover the intended use of all functionality of a crate, or the "happy path", as well as all of the reachable error cases. Integration tests can be used to test how servers interact.

Tests of the happy path may look like this:

    #[test]
    fn add_valid_url() {
        let auth_urls = one_url_struct().unwrap();
        assert_eq!(auth_urls.len(), 1);
    }

Tests of error handling may look like this:

    #[test]
    fn delete_invalid_index() {
        let mut auth_urls = one_url_struct().unwrap();
        match auth_urls.delete_index(1) {
            Ok(_) => panic!("Deleting an invalid index should fail."),
            Err(TotpError::OutOfBoundsError) => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
    }

These tests, combined with thorough error handling and messaging, clearly indicate if functionality has been broken, and what exactly is broken.

### Localization

The full documentation on KeyOS localization can be found in Notion:

https://www.notion.so/foundationdevices/keyOS-Localization-114f64516a36803ab19fd47a00547b81

Below is a quick summary of the practical bits.

### Install Localazy

The instructions can be found [here](https://localazy.com/docs/cli/installation).

#### Fetching the latest translations

The following command fetches the latest translations from Localazy and stores
the files in `ui/ui/i18n/sources/<lang>/figma.json`:

```
localazy download
```

#### Updating per-app localizations

The following command will run the localizer tool to extract localizations from
the files in `ui/ui/i18n/sources/<lang>/figma.json` and update the
`i18n/<lang.yml` files under each app.

#### Adding a new app to the localization process

Add a new entry to the`apps` array in the `localizer.json` file in the project root. Here is what the entries look like:

```
{
  "sources": "ui/ui/i18n/sources",
  "slint-file": "ui/ui/i18n/translations-sim.slint",
  "apps": {
    "controlcenter": {
      "system-name": "gui-app-control-center",
      "path": "os/gui-app-control-center"
    },
    "files": {
      "system-name": "gui-app-file-browser",
      "path": "os/gui-app-file-browser"
    },
    // Add your new entry here and keep them alphabetical
    // etc.
  }
}
```

You will need to ensure that the design team has created the Figma strings with
the proper ID format or you may not get any translation strings.

Run `localazy download` to ensure localizations are up to date. Then, run `just localize` to generate the localization module within the new app. This also updates other apps' localization modules, which may be best left to their own pull requests, so be aware of what changes are being committed.

## Development Tips

### App Router History

The slint apps in KeyOS each have a router system with forward and backward history stacks. The current page is the one at the top of the backward history stack. Both stacks can be printed using this line:

    cx.router.borrow().with_history(|history| log::info!("{history:#?}"));

It's best to prevent navigation actions that can leave unreachable history in the backward history stack, which would eventually consume excessive memory. Using the `Navigate.backward` callback in slint or `ui.global::<Navigate>().invoke_backward();` in rust moves the current page from the backward history stack to the forward history stack, then displays the page that is now at the top of the backward history stack.
