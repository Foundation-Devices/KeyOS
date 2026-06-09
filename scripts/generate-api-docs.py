#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: GPL-3.0-or-later

from __future__ import annotations

import argparse
import html
import json
import os
import re
import shutil
import subprocess
import sys
import tomllib
from dataclasses import dataclass
from contextlib import contextmanager
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DOC_DIR = ROOT / "target" / "doc"
PERMISSION_GROUPS = ROOT / "permission-groups.md"
PERMISSION_TEMPLATES = ROOT / "permission_templates.toml"


@dataclass(frozen=True)
class CrateDoc:
    package: str
    crate: str
    description: str
    manifest: str | None = None

    @property
    def href(self) -> str:
        return f"{self.crate}/index.html"


CRATES = [
    CrateDoc("server", "server", "Type-safe server traits, checked connections, and IPC helpers."),
    CrateDoc("app-manager", "app_manager", "App launch, app metadata, QR acceptance criteria, and third-party certificates.", "api/app-manager/manifest.toml"),
    CrateDoc("backup", "backup", "Backup and restore operations.", "api/backup/manifest.toml"),
    CrateDoc("camera", "camera", "Camera frame subscription and camera configuration.", "api/camera/manifest.toml"),
    CrateDoc("crypto", "crypto", "Cryptographic server APIs.", "api/crypto/manifest.toml"),
    CrateDoc("fs", "fs", "Filesystem handles, directories, metadata, and mapped files.", "api/fs/manifest.toml"),
    CrateDoc("gui-server-api", "gui_server_api", "Rust API crate for the os/gui-server service.", "api/gui-server/manifest.toml"),
    CrateDoc("haptics", "haptics", "Haptic patterns and playback control.", "api/haptics/manifest.toml"),
    CrateDoc("keycard", "keycard", "Keycard operations and backup shard management.", "api/keycard/manifest.toml"),
    CrateDoc("nfc", "nfc", "NFC operations.", "api/nfc/manifest.toml"),
    CrateDoc("power-manager", "power_manager", "Reboot, shutdown, charging, battery, and OTG status.", "api/power-manager/manifest.toml"),
    CrateDoc("quantum-link", "quantum_link", "QuantumLink state, firmware fetch, pairing, PSBT, and account-sync helpers.", "api/quantum-link/manifest.toml"),
    CrateDoc("rgb-led", "rgb_led", "RGB LED color and animation control.", "api/rgb-led/manifest.toml"),
    CrateDoc("security", "security", "Authentication, seeds, keys, and device security state.", "api/security/manifest.toml"),
    CrateDoc("settings", "settings", "Typed settings get, set, and subscribe APIs.", "api/settings/manifest.toml"),
    CrateDoc("update", "update", "Firmware update status and progress APIs.", "api/update/manifest.toml"),
    CrateDoc("usb", "usb", "USB host and device-mode APIs.", "api/usb/manifest.toml"),
    CrateDoc("fido", "fido", "FIDO2/U2F security-key service APIs.", "os/fido/manifest.toml"),
]

DEFAULT_PACKAGES = [crate.package for crate in CRATES]
FORBIDDEN_CRATES = {"bt", "gpio", "i2c", "spi", "dma", "keyos_api_docs"}

IMPLEMENTOR_RE = re.compile(
    r'<details class="toggle implementors-toggle"[\s\S]*?'
    r'(?=<details class="toggle implementors-toggle"|<h2 id="trait-implementations"|'
    r'<h2 id="synthetic-implementations"|<h2 id="blanket-implementations"|'
    r'</div></section></div></main>)'
)
METHOD_RE = re.compile(
    r'(?P<prefix><details class="toggle method-toggle"[^>]*>\s*'
    r'<summary><section id="method\.(?P<name>[^"]+)" class="method[^"]*">'
    r'[\s\S]*?</section></summary>)'
    r'|(?P<bare_prefix><section id="method\.(?P<bare_name>[^"]+)" class="method[^"]*">'
    r'[\s\S]*?</section>)'
)
ITEM_DECL_RE = re.compile(r'(?P<decl><pre class="rust item-decl">[\s\S]*?</pre>)')
MESSAGE_ALLOWED_RE = re.compile(r"MessageAllowed\s*<\s*(?:(?:[A-Za-z_][A-Za-z0-9_]*)::)*([A-Za-z_][A-Za-z0-9_]*)\s*>")
TRAIT_HREF_RE = re.compile(r'<a class="trait" href="(?P<href>[^"]+)"')

DOC_PERMISSION_TEMPLATES = """\
[fs-generic]
"os/fs" = [
    "OpenDirMessage",
    "OpenFileMessage",
    "CloseFile",
    "CloseDir",
    "CreateDirMessage",
    "ReadFile",
    "SeekFile",
    "WriteFile",
    "TruncateFile",
    "SetLen",
    "GetMetadata",
    "NextEntry",
    "Flush",
    "FlushFs",
    "Remove",
    "Rename",
    "AtomicCopy",
    "AsyncRead",
    "AsyncWrite",
    "AsyncCopyBlock",
    "SubscribeFilesystemEvent",
]

[crypto-sha]
"os/crypto" = [
    "Hmac",
    "ShaSetContext",
    "ShaUpdate",
    "ShaGetContext",
    "ShaDrop",
]

[crypto-aes]
"os/crypto" = [
    "AesSetup",
    "AesExecute",
    "AesAad",
    "AesGcmTag",
    "AesClear",
]

[i2c]
"os/i2c" = [
    "ClaimPeripheral",
    "SingleTransfer",
]

[gpio]
"os/gpio" = [
    "ClaimPin",
    "EnableIrq",
    "SetIrq",
    "SetPin",
    "GetPin",
]

[gui-app]
"os/fs" = [
    "OpenDirMessage",
    "CloseDir",
    "NextEntry",
    "MapFileMessage",
    "GetMetadata",
]
"os/gui-server" = [
    "RegisterAppMessage",
    "RequestRedraw",
    "AnimateNextFrame",
    "SubmitFrame",
    "UpdateKeyboard",
    "HideKeyboard",
    "ShowModal",
]
"os/settings" = [
    "GetSystemTheme",
    "SubscribeSystemTheme",
    "GetDebugTouch",
    "SubscribeDebugTouch",
]

[navigable]
"os/gui-server" = [
    "GetPendingNavRequest",
    "FinishResponse",
    "NavigationCancel",
]
"""


def run(cmd: list[str], *, env: dict[str, str] | None = None) -> None:
    subprocess.run(cmd, cwd=ROOT, env=env, check=True)


@contextmanager
def doc_permission_templates():
    """Provide the template file proc macros expect without making it source state."""
    created = False
    if not PERMISSION_TEMPLATES.exists():
        PERMISSION_TEMPLATES.write_text(DOC_PERMISSION_TEMPLATES, encoding="utf-8")
        created = True
    try:
        yield
    finally:
        if created:
            PERMISSION_TEMPLATES.unlink(missing_ok=True)


def parse_args() -> list[CrateDoc]:
    parser = argparse.ArgumentParser(description="Generate public KeyOS API rustdoc.")
    parser.add_argument("packages", nargs="*", help="Cargo packages to include.")
    args = parser.parse_args()

    by_package = {crate.package: crate for crate in CRATES}
    packages = args.packages or DEFAULT_PACKAGES
    unknown = sorted(package for package in packages if package not in by_package)
    if unknown:
        print(f"Unknown API doc package(s): {', '.join(unknown)}", file=sys.stderr)
        return []
    return [by_package[package] for package in packages]


def run_rustdoc(crates: list[CrateDoc]) -> None:
    if (ROOT / "api" / "docs").exists():
        raise RuntimeError("api/docs still exists; keyos-api-docs must not be restored")

    shutil.rmtree(DOC_DIR, ignore_errors=True)
    env = os.environ.copy()
    env["RUSTDOCFLAGS"] = f"{env.get('RUSTDOCFLAGS', '').strip()} --default-theme ayu".strip()
    cmd = ["cargo", "doc", "--no-deps"]
    for crate in crates:
        cmd.extend(["-p", crate.package])
    with doc_permission_templates():
        run(cmd, env=env)


def find_static(pattern: str) -> str:
    matches = sorted((DOC_DIR / "static.files").glob(pattern))
    if not matches:
        raise RuntimeError(f"Missing rustdoc asset matching {pattern}")
    return matches[0].name


def write_index(crates: list[CrateDoc]) -> None:
    normalize_css = find_static("normalize-*.css")
    rustdoc_css = find_static("rustdoc-*.css")
    noscript_css = find_static("noscript-*.css")
    storage_js = find_static("storage-*.js")
    main_js = find_static("main-*.js")
    search_js = find_static("search-*.js")
    stringdex_js = find_static("stringdex-*.js")
    settings_js = find_static("settings-*.js")
    favicon_png = find_static("favicon-32x32-*.png")
    favicon_svg = find_static("favicon-*.svg")

    sidebar = "\n".join(
        f'<li><a href="{html.escape(crate.href)}">{html.escape(crate.crate)}</a></li>' for crate in crates
    )
    table_rows = "\n".join(
        '<dt><a class="mod" href="{href}" title="crate {crate}">{crate}</a></dt>'
        "<dd>{description}</dd>".format(
            href=html.escape(crate.href),
            crate=html.escape(crate.crate),
            description=html.escape(crate.description),
        )
        for crate in crates
    )

    (DOC_DIR / "index.html").write_text(
        f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<meta name="generator" content="rustdoc">
<meta name="description" content="KeyOS API documentation">
<title>KeyOS API Docs</title>
<link rel="stylesheet" href="./static.files/{normalize_css}">
<link rel="stylesheet" href="./static.files/{rustdoc_css}">
<script id="default-settings"
data-use_system_theme="false"
data-theme="ayu"></script>
<meta name="rustdoc-vars" data-root-path="./" data-static-root-path="./static.files/" data-current-crate="" data-themes="" data-resource-suffix="" data-rustdoc-version="generated" data-channel="" data-search-js="{search_js}" data-stringdex-js="{stringdex_js}" data-settings-js="{settings_js}">
<script src="./static.files/{storage_js}"></script>
<script defer src="./crates.js"></script>
<script defer src="./static.files/{main_js}"></script>
<noscript><link rel="stylesheet" href="./static.files/{noscript_css}"></noscript>
<link rel="stylesheet" href="./theme.css">
<link rel="alternate icon" type="image/png" href="./static.files/{favicon_png}">
<link rel="icon" type="image/svg+xml" href="./static.files/{favicon_svg}">
</head>
<body class="rustdoc mod crate">
<rustdoc-topbar><h2><a href="#">KeyOS API Docs</a></h2></rustdoc-topbar>
<nav class="sidebar">
<div class="sidebar-crate"><h2><a href="./index.html">KeyOS API Docs</a></h2></div>
<div class="sidebar-elems">
<section id="rustdoc-toc">
<h3><a href="#crates">Crates</a></h3>
<ul class="block">
{sidebar}
</ul>
</section>
</div>
</nav>
<div class="sidebar-resizer" title="Drag to resize sidebar"></div>
<main>
<div class="width-limiter">
<section id="main-content" class="content">
<div class="main-heading">
<h1>KeyOS API Docs</h1>
<rustdoc-toolbar></rustdoc-toolbar>
</div>
<details class="toggle top-doc" open>
<summary class="hideme"><span>Expand description</span></summary>
<div class="docblock">
<p>Generated rustdoc entry point for the public KeyOS API crates.</p>
</div>
</details>
<h2 id="crates" class="section-header">Crates<a href="#crates" class="anchor">&sect;</a></h2>
<dl class="item-table">
{table_rows}
</dl>
</section>
</div>
</main>
</body>
</html>
""",
        encoding="utf-8",
    )


def load_toml(path: Path) -> dict:
    with path.open("rb") as file:
        return tomllib.load(file)


def build_message_server_map(crates: list[CrateDoc]) -> dict[str, dict[str, str]]:
    by_crate: dict[str, dict[str, str]] = {}
    for crate in crates:
        by_crate[crate.crate] = {}
        if not crate.manifest:
            continue
        manifest_path = ROOT / crate.manifest
        if not manifest_path.exists():
            continue
        data = load_toml(manifest_path)
        servers = data.get("servers", {})
        for server_name, messages in servers.items():
            for message_name in messages:
                by_crate[crate.crate][message_name] = server_name
    return by_crate


def clean_cell(value: str) -> str:
    value = value.strip()
    if value.startswith("`") and value.endswith("`"):
        value = value[1:-1]
    return value.strip()


def code_values(value: str) -> list[str]:
    values = re.findall(r"`([^`]+)`", value)
    if values:
        return [clean_cell(item) for item in values]
    cleaned = clean_cell(value)
    return [cleaned] if cleaned else []


def split_markdown_row(line: str) -> list[str]:
    return [cell.strip() for cell in line.strip().strip("|").split("|")]


def parse_permission_groups() -> dict[tuple[str, str], list[dict[str, str]]]:
    if not PERMISSION_GROUPS.exists():
        return {}

    groups: dict[tuple[str, str], list[dict[str, str]]] = {}
    for line in PERMISSION_GROUPS.read_text(encoding="utf-8").splitlines():
        if not line.startswith("|") or re.match(r"^\|\s*-", line):
            continue
        cells = split_markdown_row(line)
        if len(cells) < 5 or clean_cell(cells[0]) in {"Subgroup", "Primary group"}:
            continue

        subgroup = clean_cell(cells[0])
        sender_policies = code_values(cells[2])
        grant_timings = code_values(cells[3])
        messages_cell = cells[4]
        chunks = re.split(r"(?=;?\s*`os/)", messages_cell)
        for chunk in chunks:
            match = re.match(r";?\s*`([^`]+)`:\s*(.*)", chunk)
            if not match:
                continue
            server_name = match.group(1)
            for message_name in re.findall(r"`([^`]+)`", match.group(2)):
                groups.setdefault((server_name, message_name), []).append(
                    {
                        "group": subgroup,
                        "sender_policies": sender_policies,
                        "grant_timings": grant_timings,
                    }
                )
    return groups


def strip_tags(fragment: str) -> str:
    return html.unescape(re.sub(r"<[^>]+>", "", fragment))


def extract_message_allowed(fragment: str) -> list[str]:
    seen: set[str] = set()
    messages: list[str] = []
    text = strip_tags(fragment)
    for message in MESSAGE_ALLOWED_RE.findall(text):
        if message not in seen:
            seen.add(message)
            messages.append(message)
    return messages


def is_local_source(fragment: str, crate: str) -> bool:
    return bool(re.search(rf"""href=["'][^"']*src/{re.escape(crate)}/""", fragment))


def impl_kind(header: str) -> str:
    code_header_match = re.search(r'<h3 class="code-header">(?P<header>[\s\S]*?)</h3>', header)
    if not code_header_match:
        return "unknown"
    code_header = code_header_match.group("header")
    return "trait" if " for " in strip_tags(code_header) else "inherent"


def is_keyos_trait_impl(header: str) -> bool:
    code_header_match = re.search(r'<h3 class="code-header">(?P<header>[\s\S]*?)</h3>', header)
    if not code_header_match:
        return False
    code_header = code_header_match.group("header")
    if " for " not in strip_tags(code_header):
        return False
    impl_header = code_header.split('<div class="where">', 1)[0]
    for_index = impl_header.rfind(" for ")
    if for_index == -1:
        return False
    trait_matches = [match for match in TRAIT_HREF_RE.finditer(impl_header) if match.start() < for_index]
    if not trait_matches:
        return False
    href = html.unescape(trait_matches[-1].group("href"))
    return not (href.startswith("http://") or href.startswith("https://"))


def resolve_permissions(
    crate: str,
    messages: list[str],
    message_servers: dict[str, dict[str, str]],
    permission_groups: dict[tuple[str, str], list[dict[str, str]]],
) -> list[dict[str, object]]:
    resolved: list[dict[str, object]] = []
    for message in messages:
        server = message_servers.get(crate, {}).get(message)
        if not server:
            resolved.append(
                {
                    "message": message,
                    "server": None,
                    "groups": [],
                    "sender_policies": [],
                    "grant_timings": [],
                    "status": "unknown-server",
                }
            )
            continue

        groups = permission_groups.get((server, message), [])
        resolved.append(
                {
                    "message": message,
                    "server": server,
                    "groups": sorted({group["group"] for group in groups}) if groups else [],
                    "sender_policies": sorted({policy for group in groups for policy in group["sender_policies"]})
                    if groups
                    else [],
                    "grant_timings": sorted({timing for group in groups for timing in group["grant_timings"]})
                    if groups
                    else [],
                    "status": "known" if groups else "unknown-groups",
                }
            )
    return resolved


def code_list(values: list[str]) -> str:
    if not values:
        return "TBD"
    return ", ".join(f"<code>{html.escape(value)}</code>" for value in values)


def render_permission_block(permissions: list[dict[str, object]]) -> str:
    if not permissions:
        return '<div class="docblock keyos-permissions"><p><strong>Permission:</strong> none</p></div>'

    if len(permissions) == 1:
        permission = permissions[0]
        message = str(permission["message"])
        server = permission["server"]
        if server:
            permission_text = f"<code>{html.escape(str(server))} / {html.escape(message)}</code>"
        else:
            permission_text = f"TBD (<code>MessageAllowed&lt;{html.escape(message)}&gt;</code>)"
        return (
            '<div class="docblock keyos-permissions">'
            f"<p><strong>Permission:</strong> {permission_text}</p>"
            f"<p><strong>Groups:</strong> {code_list(permission['groups'])}</p>"
            f"<p><strong>Sender policy:</strong> {code_list(permission['sender_policies'])}</p>"
            f"<p><strong>Grant timing:</strong> {code_list(permission['grant_timings'])}</p>"
            "</div>"
        )

    items = []
    for permission in permissions:
        message = str(permission["message"])
        server = permission["server"]
        if server:
            permission_text = f"<code>{html.escape(str(server))} / {html.escape(message)}</code>"
        else:
            permission_text = f"TBD (<code>MessageAllowed&lt;{html.escape(message)}&gt;</code>)"
        items.append(
            "<li>"
            f"<p><strong>Permission:</strong> {permission_text}</p>"
            f"<p><strong>Groups:</strong> {code_list(permission['groups'])}</p>"
            f"<p><strong>Sender policy:</strong> {code_list(permission['sender_policies'])}</p>"
            f"<p><strong>Grant timing:</strong> {code_list(permission['grant_timings'])}</p>"
            "</li>"
        )
    return '<div class="docblock keyos-permissions"><ul>' + "".join(items) + "</ul></div>"


def permission_record(
    crate: str,
    path: Path,
    kind: str,
    item_name: str,
    item_id: str,
    permissions: list[dict[str, object]],
) -> dict[str, object]:
    return {
        "crate": crate,
        "html_path": str(path.relative_to(DOC_DIR)),
        "kind": kind,
        "item": item_name,
        "item_id": item_id,
        "permissions": permissions,
    }


def unique(messages: list[str]) -> list[str]:
    seen: set[str] = set()
    output: list[str] = []
    for message in messages:
        if message not in seen:
            seen.add(message)
            output.append(message)
    return output


def annotate_impl_blocks(
    text: str,
    crate: str,
    path: Path,
    message_servers: dict[str, dict[str, str]],
    permission_groups: dict[tuple[str, str], list[dict[str, str]]],
    records: list[dict[str, object]],
) -> str:
    def replace_impl(match: re.Match[str]) -> str:
        block = match.group(0)
        header = block.split('<div class="impl-items">', 1)[0]
        impl_messages = extract_message_allowed(header)
        kind = impl_kind(header)
        if kind == "trait" and not is_keyos_trait_impl(header):
            return block
        impl_is_local = kind == "inherent" or is_keyos_trait_impl(header)

        def replace_method(method_match: re.Match[str]) -> str:
            prefix = method_match.group("prefix") or method_match.group("bare_prefix")
            method_name = html.unescape(method_match.group("name") or method_match.group("bare_name"))
            method_messages = unique(impl_messages + extract_message_allowed(prefix))
            method_is_local = impl_is_local and is_local_source(prefix, crate)
            if not method_is_local:
                return prefix

            permissions = resolve_permissions(crate, method_messages, message_servers, permission_groups)
            records.append(
                permission_record(
                    crate,
                    path,
                    "method",
                    method_name,
                    f"method.{method_name}",
                    permissions,
                )
            )
            return prefix + render_permission_block(permissions)

        return METHOD_RE.sub(replace_method, block)

    return IMPLEMENTOR_RE.sub(replace_impl, text)


def annotate_function_page(
    text: str,
    crate: str,
    path: Path,
    message_servers: dict[str, dict[str, str]],
    permission_groups: dict[tuple[str, str], list[dict[str, str]]],
    records: list[dict[str, object]],
) -> str:
    if not path.name.startswith("fn."):
        return text
    if not is_local_source(text, crate):
        return text
    match = ITEM_DECL_RE.search(text)
    if not match:
        return text
    item_name = path.name.removeprefix("fn.").removesuffix(".html")
    messages = extract_message_allowed(match.group("decl"))
    permissions = resolve_permissions(crate, messages, message_servers, permission_groups)
    records.append(permission_record(crate, path, "function", item_name, f"fn.{item_name}", permissions))
    return text[: match.end()] + render_permission_block(permissions) + text[match.end() :]


def annotate_html(
    crates: list[CrateDoc],
    message_servers: dict[str, dict[str, str]],
    permission_groups: dict[tuple[str, str], list[dict[str, str]]],
) -> list[dict[str, object]]:
    crate_names = {crate.crate for crate in crates}
    records: list[dict[str, object]] = []

    for crate in sorted(crate_names):
        crate_dir = DOC_DIR / crate
        if not crate_dir.exists():
            continue
        for path in sorted(crate_dir.rglob("*.html")):
            text = path.read_text(encoding="utf-8")
            updated = annotate_impl_blocks(text, crate, path, message_servers, permission_groups, records)
            updated = annotate_function_page(updated, crate, path, message_servers, permission_groups, records)
            if updated != text:
                path.write_text(updated, encoding="utf-8")

    (DOC_DIR / "keyos-permissions.json").write_text(json.dumps(records, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return records


def scrub_forbidden_search_paths() -> None:
    search_index = DOC_DIR / "search.index"
    if not search_index.exists():
        return

    crates = sorted(FORBIDDEN_CRATES - {"keyos_api_docs"})
    for file in search_index.rglob("*"):
        if not file.is_file():
            continue
        content = file.read_text(encoding="utf-8", errors="ignore")
        updated = content
        for crate in crates:
            updated = re.sub(rf'"{re.escape(crate)}(?:::[^"]*)?"', '"internal"', updated)
        if updated != content:
            file.write_text(updated, encoding="utf-8")


def assert_no_forbidden_artifacts() -> None:
    failures: list[str] = []
    for path in DOC_DIR.rglob("*"):
        rel = path.relative_to(DOC_DIR)
        if not rel.parts:
            continue
        if rel.parts[0] in FORBIDDEN_CRATES:
            failures.append(str(rel))
        if "keyos_api_docs" in str(rel) or "keyos-api-docs" in str(rel):
            failures.append(str(rel))

    crates_js = DOC_DIR / "crates.js"
    if crates_js.exists():
        content = crates_js.read_text(encoding="utf-8", errors="ignore")
        for crate in FORBIDDEN_CRATES:
            if f'"{crate}"' in content:
                failures.append(f"crates.js references {crate}")

    search_index = DOC_DIR / "search.index"
    if search_index.exists():
        for file in search_index.rglob("*"):
            if not file.is_file():
                continue
            content = file.read_text(encoding="utf-8", errors="ignore")
            for crate in FORBIDDEN_CRATES:
                pattern = re.compile(rf'"{re.escape(crate)}(?:::[^"]*)?"')
                if pattern.search(content):
                    failures.append(f"{file.relative_to(DOC_DIR)} references {crate}")

    if failures:
        raise RuntimeError("Forbidden rustdoc artifacts found:\n" + "\n".join(sorted(set(failures))))


def verify_crate_outputs(crates: list[CrateDoc]) -> None:
    missing = [crate.crate for crate in crates if not (DOC_DIR / crate.crate / "index.html").exists()]
    if missing:
        raise RuntimeError(f"Missing rustdoc crate output(s): {', '.join(missing)}")


def main() -> int:
    crates = parse_args()
    if not crates:
        return 2
    try:
        run_rustdoc(crates)
        verify_crate_outputs(crates)
        write_index(crates)
        message_servers = build_message_server_map(crates)
        permission_groups = parse_permission_groups()
        records = annotate_html(crates, message_servers, permission_groups)
        scrub_forbidden_search_paths()
        assert_no_forbidden_artifacts()
    except Exception as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 1

    print(f"Built KeyOS API docs at {DOC_DIR / 'index.html'}")
    print(f"Annotated {len(records)} rustdoc function/method entries")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
