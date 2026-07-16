<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: GPL-3.0-or-later
-->

# passport-drive

Rust CLI and MCP server for driving Passport Prime over USB and SAM-BA bootloader.

## Architecture

```
┌─────────────────┐   USB-CDC-ACM serial   ┌─────────────────────┐
│  passport-drive │◄──────────────────────►│  Passport Prime hw  │
│  (this binary)  │                        │                     │
│                 │                        │  usb-debug          │
│  CLI mode  OR   │                        │  (keyOS daemon)     │
│  MCP server     │                        │                     │
└─────────────────┘                        └─────────────────────┘
       ▲                                           ▲
       │ JSON-RPC 2.0 (stdio)                      │
  Claude Code                              gui-server-api
                                    CaptureScreen / InjectTouch
                                           / CloseApp
```

### Wire protocol

Normal log records flow as UTF-8 text terminated by `0x1E` (ASCII record
separator). The test protocol is multiplexed on the same port:

**Host → Device** (test command frame):
```
[0xFF][CMD][LEN_LO][LEN_HI][PAYLOAD...]
```

**Device → Host** (response frame):
```
[0xFE][CMD][STATUS][LEN_0][LEN_1][LEN_2][LEN_3][PAYLOAD...]
STATUS: 0x00 = OK, 0x01 = ERR
LEN: u32 little-endian
```

Commands:
| CMD  | Name       | Payload (host→device)            | Response payload            |
|------|------------|----------------------------------|-----------------------------|
| 0x01 | SCREENSHOT | none                             | ARGB8888 raw (480×800×4 B)  |
| 0x02 | TAP        | x_lo x_hi y_lo y_hi kind (5 B)  | none                        |
| 0x03 | POWER_BTN  | 1 byte (1=pressed, 0=released)   | none                        |
| 0x04 | REBOOT_SAM | none                             | none (device reboots)       |
| 0x05 | CLOSE_APP  | pid_lo pid_hi (2 B)              | none                        |
| 0x06 | KERNEL_CMD | 1 byte command character         | kernel debug output         |
| 0x07 | INPUT_TEXT | UTF-8 text                       | none                        |
| 0x08 | GET_VERSION | none                            | KeyOS version UTF-8 string  |

## Requirements

- Rust toolchain (stable)
- The device must be running firmware with the `usb-debug` service built
  from this branch (includes the debug protocol).

## Build

```sh
cargo build -p passport-drive --release
```

The binary is at `target/release/passport-drive`.

## CLI Usage

### Single commands

```sh
# Take a screenshot (saved as PNG)
passport-drive screenshot -o /tmp/screen.png

# Tap at coordinates
passport-drive tap 240 400

# Swipe from (240,600) to (240,200)
passport-drive swipe 240 600 240 200

# Press power button (press + release)
passport-drive power

# Tap then screenshot
passport-drive tap-screenshot 240 400 -o /tmp/screen.png

# Swipe then screenshot
passport-drive swipe-screenshot 240 600 240 200 -o /tmp/screen.png

# Stream logs from device
passport-drive logs
passport-drive logs --max-lines 50 --filter "gui"

# Close an app by PID
passport-drive close-app 40
```

### JSON action sequences

For multi-step interactions, use the `run` command with a JSON file.
This keeps the serial port open for the entire sequence, avoiding
reconnection overhead and timing issues.

```sh
passport-drive run actions.json
```

Action file format:
```json
[
  {"power": true},
  {"wait": 200},
  {"power": false},
  {"wait": 1500},
  {"screenshot": "/tmp/step1.png"},
  {"tap": [240, 400]},
  {"wait": 800},
  {"screenshot": "/tmp/step2.png"},
  {"swipe": [240, 600, 240, 200]},
  {"wait": 1000},
  {"screenshot": "/tmp/step3.png"}
]
```

### SAM-BA bootloader commands

When the device is in SAM-BA mode (either via `reboot-samba` or by
holding the boot button during power-on), you can flash firmware and
access memory directly:

```sh
# Reboot running device into SAM-BA mode
passport-drive reboot-samba

# Show SAM-BA monitor version
passport-drive samba version

# Flash a full boot image
passport-drive samba flash target/armv7a-unknown-xous-elf/release/images/boot.bin

# Flash only boot or system partition
passport-drive samba flash boot.bin --boot
passport-drive samba flash boot.bin --system

# Flash without verification
passport-drive samba flash boot.bin --no-verify

# Dump 8 MB of flash to a file
passport-drive samba dump-flash -o flash_dump.bin -n 8

# Read/write memory
passport-drive samba read-u32 0xF8048000
passport-drive samba write-u32 0xF8048054 0x66830000

# Reboot from SAM-BA back to normal mode
passport-drive samba reboot
```

### Options

```
-p, --port <PORT>    Serial port path [default: /dev/ttyACM0]
```

## MCP Server Mode

Run as a Model Context Protocol server over stdio for AI tool integration:

```sh
passport-drive mcp
```

This speaks JSON-RPC 2.0 (newline-delimited JSON) on stdin/stdout and exposes
24 tools for full device control. Configure in `.mcp.json`:

```json
{
  "mcpServers": {
    "passport-drive": {
      "command": "cargo",
      "args": ["run", "-p", "passport-drive", "--release", "--", "mcp"],
      "cwd": "."
    }
  }
}
```

### MCP Tools

#### Connection & Logs

| Tool | Description |
|------|-------------|
| `list_ports` | List available serial ports with USB metadata |
| `connect` | Connect to device (params: `port`, optional `baud_rate`) |
| `disconnect` | Disconnect from device |
| `get_logs` | Get recent log lines (params: optional `max_lines`, `filter`) |
| `clear_logs` | Clear the in-memory log buffer |

#### Device Interaction

| Tool | Description |
|------|-------------|
| `screenshot` | Capture screen as base64 PNG (480×800) |
| `tap` | Tap at coordinates (params: `x`, `y`, optional `timeout_ms`) |
| `touch` | Raw touch event (params: `x`, `y`, `kind`: 0=Press/1=Release/2=Drag) |
| `power_button` | Press/release power button (params: `pressed`) |
| `send_debug_command` | Send single-char kernel debug command |
| `reboot_to_samba` | Reboot device into SAM-BA bootloader mode |
| `close_app` | Close/kill an app by PID via gui-server (params: `pid`) |

#### SAM-BA Bootloader

| Tool | Description |
|------|-------------|
| `samba_list_devices` | List SAM-BA devices (VID:PID 03eb:6124) |
| `samba_connect` | Connect to SAM-BA device (auto-detects port) |
| `samba_disconnect` | Disconnect from SAM-BA device |
| `samba_version` | Read SAM-BA monitor version string |
| `samba_read_u32` | Read 32-bit word from address |
| `samba_write_u32` | Write 32-bit word to address |
| `samba_init_flash` | Initialize flash applet (params: optional `instance`, `partition`) |
| `samba_flash_info` | Show flash applet parameters (instance, ioset, partition, bus width, voltage) |
| `samba_read_flash` | Read flash region as base64 (params: `offset`, `length`) |
| `samba_write_flash` | Write base64 data to flash (params: `offset`, `data_base64`) |
| `samba_verify_flash` | Verify flash matches data (params: `offset`, `data_base64`) |
| `samba_reboot` | Reboot device from SAM-BA mode |

## Screen coordinates

Origin is top-left (0, 0). Screen is 480×800 pixels.

Touch kinds: `0` = Press, `1` = Release, `2` = Drag.
