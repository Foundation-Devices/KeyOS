<!--
SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: GPL-3.0-or-later
-->

# `gui-app-emu-flux`

The Flux emulator. It runs upstream wallet apps unmodified on Passport Prime by
emulating the SDK's syscall and SEPH interfaces, and exposes them to a connected
host over USB as "Legacy Mode".

## What it is

A "Flux app" (`app-flux-ethereum`, `app-flux-solana`) is an upstream wallet app
compiled from its original C sources and linked against a Rust runtime
(`app-flux-runtime`) that stubs out the SDK. Each Flux app is a **headless**
process with no window of its own. `gui-app-emu-flux` is the single Slint GUI
that launches the children, draws their framebuffer, feeds them touch input, and
bridges their APDUs to the host.

## Architecture

```
 host (USB) <--legacy-hid--> gui-app-emu-flux <--SEPH / syscalls--> app-flux-<coin>
                             (Slint GUI + FluxServer)               (headless C app +
                                                                     app-flux-runtime)
```

- **The emulator is the only window.** Flux children are launched on demand
  through `app-manager` (`LaunchAppBlocking`), not through gui-server, so they
  never become the foreground window: the emulator stays visible while a child
  runs and renders the child's output through its `FluxServer`.
- **`FluxServer`** (`src/flux/mod.rs`) answers the child's SDK calls: the
  `SyscallBuffer` / `SvcCall` syscalls, NBGL draw commands, SEPH packets (touch
  in, APDU out), crypto, and BIP32 derivation.
- **`AppState`** (`src/main.rs`) is the one place tracking the installable Flux
  apps (`possible`) and the running children (`running`, PID to display name).

## Legacy Mode (USB)

While the emulator window is visible, the device advertises a compatibility USB
identity (VID:PID) through the `legacy-hid` server so a host wallet app
recognizes it. Inbound APDUs are queued to the on-screen child and its replies
are framed back to the host. The identity reverts when the emulator is hidden.

## Per-app state

- **Seed.** The emulator holds one `APP_SEED` (a 32-byte AppSeed or a 64-byte
  BIP39 seed) and derives keys on the children's behalf; a child never sees the
  seed bytes. It is configured on first run and persisted in the emulator's
  `settings.json`.
- **NVM.** Each child persists its own `N_storage` region to its own AppData via
  `FileBacked` (`app-flux-runtime/src/nvm.rs`), granted by the `flux-app`
  permission template. Options such as Ethereum's blind-signing opt-in survive a
  relaunch.
- **Version.** Each child reports its build version (`FLUX_APP_VERSION`, stamped
  from the app's git tag) at startup, so the emulator answers the host's
  `GET_APP_AND_VERSION` probe with the real value.

## Related crates

| Crate | Role |
| --- | --- |
| `apps/gui-app-emu-flux` | The emulator (this crate). |
| `apps/app-flux-<coin>` | A headless Flux app: vendored C sources plus a thin Rust `main`. |
| `utils/app-flux-runtime` | The child-side runtime: SDK syscall shims, SEPH, NVM. |
| `utils/app-flux-build-support` | Build-time codegen (NVM region, version) for the children. |
| `api/gui-app-emu-flux` | The syscall and message API shared by the emulator and the runtime. |

## Legacy frame assets

The theme-specific `ui/legacy-frame-overlay{,-dark}-9.png` files are compact
97 x 97 nine-slice overlays. Their 48 px margins preserve the pre-antialiased
corners while stretching one representative row and column for the straight
edges. Keep the assets and the two `nine-slice(48)` expressions in
`ui/pages/main/page.slint` synchronized if the corner radius changes.

## Build

```bash
just build   # builds the emulator and the Flux children
```

Child ELFs land at `target/armv7a-unknown-xous-elf/release/flux/<app>/app.elf`,
already stripped and signed; copy one onto the device's mass storage to update
it without a full reflash.
