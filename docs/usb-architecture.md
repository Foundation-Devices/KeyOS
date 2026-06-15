# Passport Prime USB Interface Architecture

Passport Prime presents itself as a USB 2.1 composite device using Interface Association
Descriptors (IAD) to group its three functions under a single configuration.

| Field | Value |
|-------|-------|
| VID:PID | `0x1307:0x0165` |
| Device Class | `0xEF` (Miscellaneous / IAD) |
| Manufacturer | Foundation Devices, Inc. |
| Product | Passport Prime |
| Max Power | 32 mA (self-powered) |

Each KeyOS server registers its interface(s) at boot by calling `register_interface()`
on the central USB device server (`os/usb`) with a fixed interface number.
Class-specific setup responders are attached to interface registration and are
routed by `wIndex`. The interface numbers below reflect the normal fixed
assignment with Developer Mode enabled.

When Developer Mode is disabled, the debug interface is omitted from the active
configuration descriptor and the device re-enumerates without it.

## USB Descriptor Tree — Normal Mode, Developer Mode Enabled

```mermaid
graph TD
    DEV["<b>Device Descriptor</b><br/>VID: 0x1307 | PID: 0x0165<br/>Class: 0xEF · Sub: 0x02 · Proto: 0x01<br/>USB 2.1 · Self-Powered"]
    CFG["<b>Configuration 1</b><br/>3 Interfaces"]

    DEV --> CFG

    IF0["<b>Interface 0 — HID</b><br/>Class 0x03<br/>CTAP2 / U2F"]
    IF1["<b>Interface 1 — Mass Storage</b><br/>Class 0x08 · Sub 0x06 · Proto 0x50<br/>SCSI / Bulk-Only"]
    IF2["<b>Interface 2 — Vendor Specific</b><br/>Class 0xFF<br/>Debug + Logs"]

    CFG --> IF0
    CFG --> IF1
    CFG --> IF2

    EP1O["EP 1 OUT<br/>Interrupt · 64 B · 5 ms"]
    EP2I["EP 2 IN<br/>Interrupt · 64 B · 5 ms"]
    IF0 --> EP1O
    IF0 --> EP2I

    HID["HID Report Descriptor<br/>Usage Page: 0xF1D0 (FIDO Alliance)<br/>Usage: U2F Authenticator<br/>64-byte IN + OUT reports"]
    IF0 --> HID

    EP4O["EP 4 OUT<br/>Bulk · 512 B · DMA"]
    EP3I["EP 3 IN<br/>Bulk · 512 B · DMA"]
    IF1 --> EP4O
    IF1 --> EP3I

    EP8O["EP 8 OUT<br/>Bulk · 512 B"]
    EP5I["EP 5 IN<br/>Bulk · 512 B · DMA"]
    IF2 --> EP8O
    IF2 --> EP5I

    style IF2 stroke-dasharray: 5 5
```

> Interface 2 (dashed) is only visible at runtime when Developer Mode is enabled.

## USB Descriptor Tree — Legacy Mode (Flux Emulator Active)

When a Flux app launches, `gui-app-emu-flux` switches the device to Ledger Flex
identity. The Legacy HID interface is promoted to Interface 0 and the existing
interfaces shift down. The VID:PID change triggers a USB disconnect / re-enumeration.
When the Flux app exits, the normal identity is restored (another re-enumeration).

```mermaid
graph TD
    DEV2["<b>Device Descriptor</b><br/>VID: 0x2C97 | PID: 0x0007<br/>Class: 0xEF · Sub: 0x02 · Proto: 0x01<br/>USB 2.1 · Self-Powered"]
    CFG2["<b>Configuration 1</b><br/>4 Interfaces"]

    DEV2 --> CFG2

    LIF0["<b>Interface 0 — Legacy HID</b><br/>Class 0x03 · Sub 0x00 · Proto 0x00<br/>Ledger APDU Transport"]
    LIF1["<b>Interface 1 — HID</b><br/>Class 0x03<br/>CTAP2 / U2F"]
    LIF2["<b>Interface 2 — Vendor Specific</b><br/>Class 0xFF<br/>Debug + Logs<br/><i>Developer Mode only</i>"]
    LIF3["<b>Interface 3 — Mass Storage</b><br/>Class 0x08 · Sub 0x06 · Proto 0x50<br/>SCSI / Bulk-Only"]

    CFG2 --> LIF0
    CFG2 --> LIF1
    CFG2 --> LIF2
    CFG2 --> LIF3

    LEP_OUT["EP OUT<br/>Interrupt · 64 B · 1 ms"]
    LEP_IN["EP IN<br/>Interrupt · 64 B · 1 ms"]
    LIF0 --> LEP_OUT
    LIF0 --> LEP_IN

    LHID["HID Report Descriptor<br/>Usage Page: 0xFFA0 (Vendor)<br/>64-byte IN + OUT reports"]
    LIF0 --> LHID

    LEP1O["EP 1 OUT<br/>Interrupt · 64 B · 5 ms"]
    LEP2I["EP 2 IN<br/>Interrupt · 64 B · 5 ms"]
    LIF1 --> LEP1O
    LIF1 --> LEP2I

    LEP8O["EP 8 OUT<br/>Bulk · 512 B"]
    LEP3I["EP 3 IN<br/>Bulk · 512 B · DMA"]
    LIF2 --> LEP8O
    LIF2 --> LEP3I

    LEP5O["EP 5 OUT<br/>Bulk · 512 B · DMA"]
    LEP4I["EP 4 IN<br/>Bulk · 512 B · DMA"]
    LIF3 --> LEP5O
    LIF3 --> LEP4I

    style LIF2 stroke-dasharray: 5 5
    style LIF0 fill:#1d4ed8,stroke:#60a5fa,color:#fff
```

> The Legacy HID interface (blue) is promoted to position 0 so that host wallets
> (e.g. MoneroGUI) see it as the primary HID interface, matching a real Ledger Flex.

## KeyOS Server Mapping

```mermaid
graph LR
    subgraph "USB Device Server (os/usb)"
        USB["register_interface()"]
    end

    CTAP["<b>os/ctap-hid</b><br/>CTAP2 / U2F<br/>Authenticator"]
    DBG["<b>os/usb-debug</b><br/>Debug Commands<br/>+ Log Streaming"]
    MSE["<b>os/mass-storage-<br/>emulation</b><br/>Airlock Filesystem"]
    FLUX["<b>gui-app-emu-flux</b><br/>Legacy HID<br/>Ledger APDU Transport<br/><i>when Flux app active</i>"]

    USB -- "IF 0 · HID 0x03" --- CTAP
    USB -- "IF 1 · MSC 0x08" --- MSE
    USB -- "IF 2 · Vendor 0xFF" --- DBG
    USB -. "IF 0 · HID 0x03<br/>promotes to IF 0<br/>+ changes VID:PID" .- FLUX

    FIDO["fido crate<br/>U2F + CTAP2"]
    GUI["gui-server<br/>Screen capture · Touch<br/>injection · App lifecycle"]
    LOG["log-server<br/>System log ring buffer"]
    FS["filesystem<br/>Airlock FAT32"]
    SETTINGS["settings<br/>Airlock mode"]
    SEPH["Flux app<br/>APDU processing"]

    CTAP --> FIDO
    DBG --> GUI
    DBG --> LOG
    MSE --> FS
    MSE --> SETTINGS
    FLUX --> SEPH

    style FLUX stroke-dasharray: 5 5
```

> The Legacy HID registration (dashed) only occurs while a Flux app is running.

---

## USB-Debug Interface — Protocol Reference

The vendor-specific debug interface (class `0xFF`) carries both debug commands and
system logs on a single pair of bulk endpoints. It is only present when the
`usb-debug` service is built and Developer Mode is enabled.

**Source files:**
- Device side: `os/usb-debug/src/main.rs`, `os/usb-debug/src/protocol.rs`
- Host side: `utils/passport-drive/src/usb_transport.rs`

### Frame Format

Each frame is a single USB bulk transfer, terminated by a short packet or ZLP.

**Host to Device (OUT endpoint):**

```
┌──────┬────────────────────┐
│ CMD  │ PAYLOAD (0..N)     │
│ 1 B  │                    │
└──────┴────────────────────┘
```

**Device to Host (IN endpoint):**

Log frames and debug responses are multiplexed on the same IN endpoint, distinguished
by a 1-byte TYPE prefix.

```
TYPE 0x01 — Log data:
┌──────┬─────────────────────────────────────┐
│ 0x01 │ UTF-8 log bytes (0x1E-terminated)   │
│ 1 B  │                                     │
└──────┴─────────────────────────────────────┘

TYPE 0x02 — Debug response:
┌──────┬────────┬──────────────────────┐
│ 0x02 │ STATUS │ Response data (0..N) │
│ 1 B  │ 1 B    │                      │
└──────┴────────┴──────────────────────┘
  STATUS: 0x00 = OK, 0x01 = Error
```

### Command Table

| Byte | Name | Payload (Host → Device) | Response (Device → Host) |
|------|------|-------------------------|--------------------------|
| `0x01` | `SCREENSHOT` | — | 1,536,000 bytes (480 x 800 x 4, ARGB8888) |
| `0x02` | `TAP` | `x_lo x_hi y_lo y_hi kind` (5 B, LE) | Ack (empty) |
| `0x03` | `POWER_BTN` | 1 B: `0x00` = release, else = press | Ack (empty) |
| `0x04` | `REBOOT_SAMBA` | — | Ack, then device reboots into SAM-BA |
| `0x05` | `CLOSE_APP` | `pid_lo pid_hi` (2 B LE) | Ack (empty) |
| `0x06` | `KERNEL_CMD` | 1 B: command character (see below) | Kernel debug output (variable) |
| `0x07` | `INPUT_TEXT` | UTF-8 text bytes | Ack (empty) |
| `0x08` | `GET_VERSION` | — | UTF-8 KeyOS version bytes |
| `0x09` | `LAUNCH_APP` | 16-byte AppId | `pid_lo pid_hi status` (3 B; status `0` launched, `1` already running) |
| `0x0A` | `GET_DEVELOPER_MODE` | — | 1 B: `0` disabled, `1` enabled |
| `0x0B` | `LOAD_APP_BEGIN` | UTF-8 sideloaded app-id directory name | Ack (empty) |
| `0x0C` | `LOAD_APP_FILE_BEGIN` | `kind:1` + expected size as 8 B LE | Ack (empty) |
| `0x0D` | `LOAD_APP_CHUNK` | File bytes, max 510 B per chunk | Ack; final chunk closes current file |
| `0x0E` | `LOAD_APP_END` | — | Ack (empty) |
| `0x0F` | `GET_PROCESS_LIST` | — | Compact process list output |

`LOAD_APP_FILE_BEGIN` file kinds are `0x00` for `app.elf` and `0x01` for
`manifest.json`. Normal uploads send `LOAD_APP_FILE_BEGIN`, then
`LOAD_APP_CHUNK` frames until the declared size is reached; that final chunk
flushes and promotes the current `.part` file. The `load_app` MCP tool drives
`0x0B`, the per-file `0x0C`/`0x0D` uploads, and `0x0E`, then asks app-manager to
rescan installed apps. In production builds, the device rejects `REBOOT_SAMBA`
and `KERNEL_CMD`; the other commands above are allowed.

**TAP touch kinds:** `0` = Press, `1` = Release, `2` = Drag. Screen coordinates:
origin top-left, 480 x 800 pixels.

### Kernel Debug Sub-Commands (CMD `0x06`)

| Char | Description |
|------|-------------|
| `h` | Help / command list |
| `i` | IRQ statistics |
| `m` | MMU state |
| `p` | Process list (verbose) |
| `t` | Process list (compact) |
| `s` | Server list |
| `c` | Cache statistics |
| `a` | AppID to PID mapping |
| `o` | Memory ownership |
| `k` | Consistency check |

### Wire Protocol — Sequence Diagram

```mermaid
sequenceDiagram
    participant Host
    participant Device as usb-debug

    Note over Host,Device: Each arrow is one USB bulk transfer

    Device->>Host: [0x01] log data ... 0x1E ...
    Note right of Host: Log frame (TYPE 0x01)<br/>arrives continuously

    Host->>Device: [0x01] SCREENSHOT
    Device->>Host: [0x01] log data ... 0x1E ...
    Note right of Host: Logs may interleave<br/>with pending response
    Device->>Host: [0x02][0x00] + 1,536,000 B ARGB
    Note right of Host: Screenshot response

    Host->>Device: [0x02][xL][xH][yL][yH][kind]
    Device->>Host: [0x02][0x00]
    Note right of Host: TAP ack

    Host->>Device: [0x06][0x74]
    Note left of Host: KERNEL_CMD 't'
    Device->>Host: [0x02][0x00] + process list UTF-8
    Note right of Host: Kernel output

    Host->>Device: [0x08] GET_VERSION
    Device->>Host: [0x02][0x00] + version UTF-8
    Note right of Host: GET_VERSION → KeyOS version

    Host->>Device: [0x04] REBOOT_SAMBA
    Device->>Host: [0x02][0x00]
    Note right of Host: Ack (device reboots)
```

**Log retention:** The `log-server` keeps a **16 KB ring buffer** that overwrites
old entries unconditionally — there is no backpressure to writers. When a host tool
connects, its `LogReader` receives up to 16 KB of the most recent logs already in
the ring, then streams new logs going forward. If the host stops draining (or
disconnects), the reader's position is eventually lapped by the write pointer and
the intermediate logs are silently lost. In practice this means that after extended
uptime you will see the last ~16 KB of log output on connect and everything before
that is gone.

---

See [Legacy Mode HID — Ledger-Compatible APDU Interface](legacy-mode-hid.md) for
the full protocol reference on the Ledger-compatible HID interface used by Flux apps.

---

## Host-Side Tools

Two checked-in tools communicate with the USB-debug interface from the host.

### passport-drive

**Location:** `utils/passport-drive/`

A Rust CLI and MCP (Model Context Protocol) server for driving Passport Prime
over USB. It is the most feature-complete host tool.

- **Transport:** `rusb` crate. Auto-detects the vendor-specific interface (class
  `0xFF`) by iterating the USB config descriptor. A background reader thread
  demuxes IN frames into separate `log_rx` and `resp_rx` channels.
- **Debug commands used:** All device debug commands (`0x01`–`0x0F`) through the
  CLI and MCP server.
- **MCP tools over the debug interface:** `connect`, `disconnect`, `get_logs`,
  `clear_logs`, `screenshot`, `tap`, `touch`, `power_button`,
  `send_debug_command`, `send_kernel_command`, `reboot_to_samba`, `input_text`,
  `close_app`, `load_app`, `launch_app`, `get_developer_mode`, `get_version`,
  `get_process_list`.
- **Additional capabilities:**
  - Device discovery: `list_ports`.
  - SAM-BA bootloader mode: flash read / write / verify (via `sambuca` crate).
  - HID APDU exchange: CTAP/FIDO mode (usage page `0xF1D0`) and Ledger mode
    (VID `0x2C97`, usage page `0xFFA0`) on Interface 0.

**MCP tools for SAM-BA mode:** `samba_list_devices`, `samba_connect`,
`samba_disconnect`, `samba_version`, `samba_read_u32`, `samba_write_u32`,
`samba_init_flash`, `samba_flash_info`, `samba_read_flash`,
`samba_write_flash`, `samba_verify_flash`, `samba_reboot`.

**MCP tool for HID APDU:** `send_apdu`.

### keyos-log-viewer

**Location:** `utils/keyos-log-viewer/`

A Rust TUI application built with `ratatui` for real-time log streaming, filtering,
and search.

- **Transport:** `rusb` crate with the same vendor interface auto-detection.
  Auto-reconnects on device disconnect.
- **Debug commands used:** `0x06` only (`KERNEL_CMD` with character `'t'` for
  compact process list snapshots).
- **Log parsing:** Accumulates bytes from TYPE `0x01` frames, splits on `0x1E`
  record terminators.

### foundation CLI (SDK)

**Location:** `sdk/crates/cli/`

The SDK CLI for Flux app developers.

- **`foundation logs`** — Launches `keyos-log-viewer` as a subprocess (no direct
  USB usage).
- **`foundation sideload`** — Builds and signs the app, copies the ELF and manifest
  to mass storage (Interface 1), then launches it through the `passport-drive`
  MCP server. The SDK package stages that helper as `foundation-passport-drive`.
- **Debug commands used:** `0x0A` for Developer Mode preflight and `0x09` to
  launch the installed app, via the `passport-drive` MCP server.

### Tool Command Matrix

| Capability | passport-drive | keyos-log-viewer | foundation sideload |
|------------|:-:|:-:|:-:|
| `0x01` SCREENSHOT | x | | |
| `0x02` TAP | x | | |
| `0x03` POWER_BTN | x | | |
| `0x04` REBOOT_SAMBA | x | | |
| `0x05` CLOSE_APP | x | | |
| `0x06` KERNEL_CMD | x | x | |
| `0x07` INPUT_TEXT | x | | |
| `0x08` GET_VERSION | x | | |
| `0x09` LAUNCH_APP | x | | x |
| `0x0A` GET_DEVELOPER_MODE | x | | x |
| `0x0B` LOAD_APP_BEGIN | x | | |
| `0x0C` LOAD_APP_FILE_BEGIN | x | | |
| `0x0D` LOAD_APP_CHUNK | x | | |
| `0x0E` LOAD_APP_END | x | | |
| `0x0F` GET_PROCESS_LIST | x | | |
| Log Streaming (TYPE 0x01) | x | x | |
| SAM-BA Flash R/W | x | | |
| HID APDU (CTAP + Ledger) | x | | |
| Mass Storage (IF 1) | | | x |

---

## Alternative USB Identities

| Mode | VID:PID | When |
|------|---------|------|
| Normal | `0x1307:0x0165` | Standard boot, no Flux app running |
| Legacy | `0x2C97:0x0007` | While the Flux emulator is on screen (Ledger Flex identity) |

The device switches between identities at runtime. When a Flux app launches,
`gui-app-emu-flux` calls `set_custom_vid_pid(0x2C97, 0x0007)` and the USB bus
re-enumerates with the Ledger Flex identity. When the Flux app exits, the normal
VID:PID is restored and the bus re-enumerates again. Each transition is visible
to the host as a USB disconnect followed by a new device appearing.

The host tools try the normal VID:PID first, then fall back to the Legacy one.

**CDC ACM serial** (`os/logging/usb-serial`): An optional logging transport that
adds two extra interfaces (CDC control + CDC data). Excluded from normal builds;
only enabled via the `--log-usb-serial` build flag for special production debugging.
