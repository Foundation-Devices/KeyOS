// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

fn main() {
    // app-manager owns the permission subgroup labels, so it resolves them from its own i18n
    // catalog (registered in localizer.json). It has no Slint UI, so it uses the standalone
    // (Rust-only) translation emit.
    localizer_codegen::compile_service_translations();
    emit_dev_signer();
}

/// Bake the public key this build signs with, so development firmware can classify a sideload
/// signed by it as Foundation. Production firmware trusts the official signer roster and host
/// builds verify nothing, so both emit an empty file.
fn emit_dev_signer() {
    const COSIGN2_TOML: &str = "../../cosign2.toml";
    println!("cargo::rerun-if-changed={COSIGN2_TOML}");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("dev_signer.rs");
    if std::env::var_os("CARGO_FEATURE_PRODUCTION").is_some() || std::env::var_os("CARGO_CFG_KEYOS").is_none()
    {
        std::fs::write(&out, "").unwrap();
        return;
    }

    let config = std::fs::read_to_string(COSIGN2_TOML).unwrap_or_else(|_| {
        panic!(
            "cosign2.toml is missing at the repo root: run scripts/generate-cosign2-dev-key.sh. \
             Development firmware trusts sideload bundles signed by exactly this key, so the \
             image and the bundles must be built from the same cosign2.toml; regenerating it, or \
             signing bundles with another --cosign2 file, yields bundles the image classifies as \
             third-party."
        )
    });
    let config: toml::Table = config.parse().expect("cosign2.toml is not valid TOML");
    let pubkey =
        config.get("pubkey").and_then(toml::Value::as_str).expect("cosign2.toml has no pubkey entry");
    let key = hex::decode(pubkey).expect("cosign2.toml pubkey is not valid hex");
    assert_eq!(key.len(), 33, "cosign2.toml pubkey is not a 33-byte compressed secp256k1 key");
    let bytes: Vec<String> = key.iter().map(|byte| format!("0x{byte:02x}")).collect();
    std::fs::write(
        &out,
        format!(
            "/// The key this firmware build was signed with; a sideload bundle carrying its\n\
             /// developer signature is Foundation-classified.\n\
             pub(crate) const DEV_SIGNER: [u8; 33] = [{}];\n",
            bytes.join(", ")
        ),
    )
    .unwrap();
}
