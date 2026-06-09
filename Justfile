# SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: GPL-3.0-or-later

# NOTE:
# - Try to not add any more crates to the exclude list. Reduce it.
# - In the same order of thinking, try reducing the cargo audit ignore list.
# - Keep in sync. with CI.
# - Do not use just in CI. It increases CI times.

docker_image := 'keyos'
api_doc_packages := 'server app-manager backup camera crypto fs gui-server-api haptics keycard nfc power-manager quantum-link rgb-led security settings update usb fido'

# Format the codebase.
fmt:
    just slint-fmt
    cargo fmt
    just toml-fmt
    just nix-fmt

toml-fmt *args:
	#!/usr/bin/env bash
	if ! command -v taplo >/dev/null 2>&1; then
		echo "taplo not found, install it with 'cargo install taplo-cli'"
		exit 1
	fi
	taplo format {{args}} \
		apps/*/Cargo.toml \
		boot/*/Cargo.toml \
		os/*/*/Cargo.toml \
		Cargo.toml \
		os/*/Cargo.toml \
		server/Cargo.toml \
		slint-keyos-platform/*/Cargo.toml \
		test-apps/**/Cargo.toml \
		utils/*/Cargo.toml \
		xous/**/Cargo.toml \
		xtask/Cargo.toml \
		permissions_templates.toml \
		*/*/manifest.toml \

nix-fmt *args:
    nix fmt -- .

# Format the Slint files
slint-fmt:
    ./scripts/format-slint.sh

# Run tests.
test:
    cargo test --workspace \
            --exclude gui-app-control-center \
            --exclude gui-app-example-logo \
            --exclude gui-app-keyboard \
            --exclude gui-app-test \
            --exclude gui-server \
            --exclude loader \
            --exclude llio \
            --exclude perflib \
            --exclude skeleton \
            --exclude syscall-arg-invalid-addr-test \
            --exclude xous-api-names \
            --exclude xous-api-ticktimer

# Lint the codebase.
lint:
    ./scripts/check-reuse.sh
    ./scripts/format-slint.sh --check
    cargo fmt --all --check
    just toml-fmt --check
    cargo check --target armv7a-unknown-xous-elf --package atsama5d27
    cargo check --target armv7a-none-eabi --manifest-path boot/keyos-boot/Cargo.toml
    just check-workspace
    cargo xtask build --hosted --dont-sign
    cargo xtask build --dont-sign
    cargo xtask build --is-recovery
    cargo xtask build sys-benchmark
    cargo audit

check-workspace:
    cargo check --workspace \
      --exclude curve25519-dalek-loader \
      --exclude ed25519-dalek-loader \
      --exclude loader \
      --exclude sha2-loader \
      --exclude syscall-arg-invalid-addr-test \
      --exclude cryptoauthlib \
      --exclude atsama5d27 \
      --exclude keyos-boot \
      --exclude boot-common \
      --exclude charge-boot \
      --exclude crypto-client \
      --exclude log-serial \
      --exclude log-usb-serial \
      --exclude usb-debug \
      --exclude libblur \
      --exclude tar-rs \
      --exclude recovery-worker \
      --exclude gui-app-recovery

check-recovery:
    cargo check \
    -p gui-app-recovery --features "recovery-os" \
    -p recovery-worker  \
    --target armv7a-unknown-xous-elf

localize:
    # Make sure the latest translations are downloaded
    localazy download

    # Run the localizer tool
    cargo run --manifest-path utils/localizer/Cargo.toml -- -c localizer.json; \

# Check for missing translation keys in Slint files
localize-check:
    cargo run --manifest-path utils/localizer/Cargo.toml -- -c localizer.json --check

gen-icu-data:
    @if ! command -v localizer > /dev/null; then \
      echo "ERROR: The 'icu4x-datagen' command is not installed. Install it with: 'cargo install icu_datagen'"; \
      exit 0; \
    else \
        icu4x-datagen --locales en fr it de es --keys datetime/gregory/datelengths@1 datetime/gregory/datesymbols@1 decimal/symbols@1 --format blob --out i18n/icu4x_data.postcard --overwrite; \
    fi

sim: gen-themes
    cargo xtask run --hosted

update-preview:
    cargo run --bin update-slint-preview -- --images-folder ui/ui/images --icons-folder ui/ui/icons --template-file ui/ui/images.slint-template --output-file ui/ui/images.slint

fix-preview: slint-fmt
    cargo run --bin update-slint-preview -- --images-folder ui/ui/images --icons-folder ui/ui/icons --template-file ui/ui/images.slint-template --output-file ui/ui/images.slint
    cargo run --bin toggle-slint-preview -- --preview \
        ui/ui/images.slint \
        ui/ui/widgets/card-circle.slint \
        ui/ui/widgets/line-card.slint \
        ui/ui/widgets/arc.slint \
        slint-keyos-platform/runtime/src/lib.rs
    # ./scripts/format-slint.sh

unfix-preview:
    # Run with no parameters to revert to production common image loading
    cargo run --bin toggle-slint-preview -- \
        ui/ui/images.slint \
        ui/ui/widgets/card-circle.slint \
        ui/ui/widgets/line-card.slint \
        ui/ui/widgets/arc.slint \
        slint-keyos-platform/runtime/src/lib.rs
    # ./scripts/format-slint.sh

preview file locale="en" i18n_dir="":
    #!/usr/bin/env bash
    cd "{{invocation_directory()}}"

    # Validate that locale parameter contains a valid locale code, not a path
    if [[ "{{locale}}" == *"/"* || "{{locale}}" == *"."* ]]; then
        echo "Error: It looks like you provided a path where a locale code was expected."
        echo "If you need to provide an i18n directory, make sure to specify the locale first"
        echo "Usage: just preview <file> <locale> <i18n_dir>"
        echo "Valid locales: en, es"
        exit 1
    fi

    # Build slint-viewer arguments
    args=()
    args+=("-L" "ui={{justfile_directory()}}/ui/ui/")

    # Handle i18n directory - prioritize explicit parameter, then auto-detect
    if [[ -n "{{i18n_dir}}" ]]; then
        # Use explicitly provided i18n directory
        i18n_path="{{i18n_dir}}"
        # Make path absolute if it's relative
        if [[ ! "$i18n_path" =~ ^/ ]]; then
            i18n_path="{{justfile_directory()}}/$i18n_path"
        fi
        if [[ -d "$i18n_path" ]]; then
            args+=("--i18n-dir" "$i18n_path")
            args+=("--locale" "{{locale}}")
        else
            echo "Warning: Specified i18n directory does not exist: $i18n_path"
        fi
    else
        # Auto-detect i18n directory by finding nearest Cargo.toml
        current_dir="$(pwd)"
        crate_root=""
        while [[ "$current_dir" != "/" ]]; do
            if [[ -f "$current_dir/Cargo.toml" ]]; then
                crate_root="$current_dir"
                break
            fi
            current_dir="$(dirname "$current_dir")"
        done

        # Add i18n args if we found a crate root with i18n directory
        if [[ -n "$crate_root" && -d "$crate_root/i18n" ]]; then
            args+=("--i18n-dir" "$crate_root/i18n")
            args+=("--locale" "{{locale}}")  # Use provided locale or default to "en"
        fi
    fi

    # Add the target file
    args+=("{{invocation_directory()}}/{{file}}")

    # Run the streamlined preview script
    "{{justfile_directory()}}/scripts/slint-preview.sh" "${args[@]}"

# Regenerate the per-component theme Slint files (ui2/components/ui/<c>_theme.slint)
# from the plugin schemas (defaults/plugins/*.json). Idempotent; run by build/sim.
gen-themes:
    cargo run --quiet --manifest-path ui2/theme-editor/Cargo.toml --bin slintthemegen -- --out-dir ui2/components/ui

# Verify the committed per-component theme files match the plugin schemas (CI guard;
# fails if a plugin was edited without re-running gen-themes).
check-themes:
    cargo run --quiet --manifest-path ui2/theme-editor/Cargo.toml --bin slintthemegen -- --check

build args="": gen-themes
    cargo xtask build {{args}}
    cargo xtask build-firmware-image

build-bl:
    cargo xtask build-bootloader
    cargo xtask build-firmware-image

build-bl-unsigned:
    #!/usr/bin/env bash
    # Check to ensure EXTRA_ENTROPY is set
    if [ -z "${EXTRA_ENTROPY}" ]; then
        echo "ERROR: EXTRA_ENTROPY environment variable is not set"
        exit 1
    fi
    cargo xtask build-bootloader --extra-entropy $(echo $EXTRA_ENTROPY)

build-all args="":
    cargo xtask build-all {{args}}

build-repro:
    cargo xtask build-all --production-bootloader --production-firmware
    cargo xtask print-hashes

# Prepare a KeyOS release (builds, signs, and pushes in one step)
# Usage: just prepare-release 1.0.0 ~/secrets/SAM-BA [--log-serial] [--log-usb-serial] [--log-usb-file]
#
# This will:
#   1. Validate EXTRA_ENTROPY environment variable
#   2. Build all firmware (bootloader, recovery, main OS)
#   3. Sign the bootloader with SAM-BA cipher (creates boot.cip, removes boot.bin)
#   4. Push to KeyOS-Releases and create a PR
#
# Required environment variables:
#   EXTRA_ENTROPY            - 64-character hex string for bootloader entropy
#   SECURE_SAMBA_CIPHER_PATH - Path to secure-sam-ba-cipher.py
# Optional arguments:
#   --log-serial        - Builds with xtask's UART serial logging/debug kernel feature
#   --log-usb-serial         - Enables USB CDC serial logging service in production firmware
#   --log-usb-file       - Saves logs to files on an attached external USB drive
prepare-release VERSION SECRETS_DIR *args:
  cargo run --bin prepare-release -- {{VERSION}} {{SECRETS_DIR}} {{args}}

clean:
    cargo clean

# Build the local KeyOS API rustdoc site.
docs-api:
    python3 scripts/generate-api-docs.py {{api_doc_packages}}

# Alias for the singular form.
doc-api: docs-api

# Package the generated KeyOS API rustdoc site for hosting.
docs-api-package: docs-api
    tar -czf target/keyos-api-rustdoc.tar.gz -C target/doc .
    printf 'Packaged KeyOS API docs at %s\n' 'target/keyos-api-rustdoc.tar.gz'

# Serve the generated KeyOS API rustdoc site locally.
docs-api-serve port="8765":
    #!/usr/bin/env bash
    set -euo pipefail
    just docs-api
    selected_port="{{port}}"
    while lsof -nP -iTCP:"$selected_port" -sTCP:LISTEN >/dev/null 2>&1; do
        selected_port=$((selected_port + 1))
    done
    printf 'Serving KeyOS API docs at http://127.0.0.1:%s/\n' "$selected_port"
    python3 -m http.server "$selected_port" --bind 127.0.0.1 --directory target/doc

list-subtrees:
    git log | grep git-subtree-dir | tr -d ' ' | cut -d ":" -f2 | sort | uniq | xargs -I {} bash -c 'if [ -d $(git rev-parse --show-toplevel)/{} ] ; then echo {}; fi'

# Run the quick lint checks.
short-lint:
    #!/usr/bin/env bash
    failed=0

    if ! ./scripts/check-reuse.sh; then
        echo -e "\nREUSE lint failed: one or more files are missing SPDX copyright/license headers." >&2
        failed=1
    fi

    if ! cargo fmt --all --check; then
        echo -e "\nRust formatting check failed: run 'just fmt' (or 'cargo fmt') and commit the result." >&2
        failed=1
    fi

    if ! just toml-fmt --check; then
        echo -e "\nTOML formatting check failed: run 'just fmt' (or 'just toml-fmt') and commit the result." >&2
        failed=1
    fi

    exit $failed

# Lint for panics
panic-lint:
    cargo clippy -- -W clippy::unwrap_used -W clippy::expect_used

flash *args:
    cargo xtask flash {{args}}

unit-test:
    cargo test \
        -p ordered-table \
        -p worker \
        -p gui-app-authenticator \
        -p slint-keyos-platform-build \
        -p slint-keyos-platform-common \
        -p slint-keyos-platform \
        -p gui-app-control-center \
        -p quantum-link-server \
        -p backup-server \
        -p update-server \
        -p update \
        -p emmc

one-int-test +args:
    cargo xtask run --hosted --integration-test {{args}}
    -rm -f xous/kernel/disk.dat

integration-test:
    #!/usr/bin/env bash
    had_disk=0
    if [ -f xous/kernel/disk.dat ]; then
        mv xous/kernel/disk.dat xous/kernel/disk-test-temp.dat
        had_disk=1
    fi

    tests=(
        "fs-server ordered-table-test"
        "fs-server settings-server settings-test-client"
        "worker-test worker-test-disposable"
    )

    failures=0

    # Run each test and track failures
    for test in "${tests[@]}"; do
        echo "Running test: $test"
        just one-int-test --ci $test
        if [ $? -ne 0 ]; then
            echo "❌ Test failed: $test"
            ((failures++))
        else
            echo "✅ Test passed: $test"
        fi
    done

    if [ "$had_disk" -eq 1 ] && [ -f xous/kernel/disk-test-temp.dat ]; then
        mv xous/kernel/disk-test-temp.dat xous/kernel/disk.dat
    fi

    echo "------------------------------"
    if [ $failures -eq 0 ]; then
        echo "✅ All tests passed!"
    else
        echo "❌  $failures tests failed"
    fi

    if [ $failures -gt 0 ]; then
        exit 1
    fi
    exit 0

# Check if an ID is already in use
check-id id="":
    #!/usr/bin/env bash
    ID="{{id}}"

    matches=$(grep -r -l --include="manifest.json" "$ID" . || true)

    if [ -n "$matches" ]; then
        echo "This ID is already in use. It was found here:"
        echo "$matches"
        exit 1
    else
        echo "This ID does not conflict with any others in this repo."
    fi

    exit 0

# Convert App and Service names to a standard format before creating IDs
id-preprocess name="":
    #!/usr/bin/env bash
    echo {{name}} | tr '[:upper:]' '[:lower:]' | sed 's/[ -]/_/'
    # Convert to lowercase, replace all spaces and dashes with underscores

# Standardize IDs after type-specific processing
id-postprocess id="":
    #!/usr/bin/env bash
    echo {{id}} | cut -c1-32 | sed 's/^/0x/'
    # Cut down to 32 characters, and prepend "0x"

# Create App IDs with the largest possible namespace. Make sure to replace any spaces with underscores.
app-id name="":
    #!/usr/bin/env bash
    ID_RAW=$(just id-preprocess {{name}} | shasum -b -a 256 | sed -rn 's/^(.*) .*$/\1/p')
    ID=$(just id-postprocess "$ID_RAW")

    echo "New ID:"
    echo "$ID"
    if ! (just check-id "$ID"); then
        echo "Please modify the app name, and try again. Make sure to replace any spaces with underscores."
    fi

# Create readable hex IDs for core services. Make sure to replace any spaces with underscores.
service-id name="":
    #!/usr/bin/env bash
    ID_RAW=$(just id-preprocess {{name}} | xxd -p | sed 's/$/00000000000000000000000000000000/')
    ID=$(just id-postprocess "$ID_RAW")

    echo "New ID:"
    echo "$ID"
    if ! (just check-id "$ID"); then
        echo "Please change something in the first 32 characters of the service name, and try again. Make sure to replace any spaces with underscores."
    fi

clean-package package="":
    cargo clean -p {{package}} --profile release
    cargo clean --target armv7a-unknown-xous-elf -p {{package}} --profile release
    cargo clean --target armv7a-none-eabi -p {{package}} --profile release

check package="":
    cargo xtask check {{package}}

logs:
    cargo run --release --manifest-path utils/keyos-log-viewer/Cargo.toml


# Build the cosign2 binary for x86_64 Linux (Chromebook)
# Produces a static musl binary that runs on any x86_64 Linux
build-chromebook-cosign2:
    #!/usr/bin/env bash
    set -eu

    ROOT="{{justfile_directory()}}"
    TARGET="x86_64-unknown-linux-musl"
    LINKER="x86_64-linux-musl-gcc"

    echo "Building cosign2 for Chromebook (x86_64 Linux)..."
    echo ""

    # Check for rustup target
    if ! rustup target list --installed | grep -q "$TARGET"; then
        echo "ERROR: Rust target '$TARGET' is not installed." >&2
        echo "" >&2
        echo "Install it with:" >&2
        echo "  rustup target add $TARGET" >&2
        echo "" >&2
        exit 1
    fi

    # Check for musl-cross linker
    if ! command -v "$LINKER" &>/dev/null; then
        echo "ERROR: musl-cross linker '$LINKER' is not installed." >&2
        echo "" >&2
        echo "Install it with:" >&2
        echo "  brew install filosottile/musl-cross/musl-cross" >&2
        echo "" >&2
        exit 1
    fi

    echo "✓ Rust target '$TARGET' is installed"
    echo "✓ musl-cross linker '$LINKER' is installed"
    echo ""

    # Build cosign2
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$LINKER" \
        cargo build --release --manifest-path "$ROOT/imports/cosign2/Cargo.toml" -p cosign2-bin --target "$TARGET"

    OUTPUT="$ROOT/imports/cosign2/target/$TARGET/release/cosign2"
    if [ -f "$OUTPUT" ]; then
        SIZE=$(du -h "$OUTPUT" | cut -f1)
        echo ""
        echo "✅ Build complete!"
        echo ""
        echo "Binary: $OUTPUT"
        echo "Size:   $SIZE"
        echo ""
        echo "Copy this file to your Chromebook."
    else
        echo "ERROR: Build failed - output binary not found" >&2
        exit 1
    fi

# Generate dependency graph for a given module
# Usage: just generate-dep-graph os/fido
generate-dep-graph module_path:
    cargo run --quiet -p keyos-dep-graph -- generate {{module_path}}

# Render and display dependency graph for a given module
# Usage: just draw-dep-graph os/fido
draw-dep-graph module_path:
    cargo run --quiet -p keyos-dep-graph -- draw {{module_path}}
