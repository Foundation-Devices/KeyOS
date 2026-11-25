// SPDX-License-Identifier: GPL-3.0-or-later

use slint_keyos_platform::{gui_server_api::InputMessage, slint::Image};

use crate::{seed::SeedType, Callbacks};

// QrCode::new (used by the platform renderer) uses medium error correction.
// Its byte-mode capacity is 2331 bytes. Guard before its infallible render API,
// including for old/imported records larger than the current input UI permits.
const MAX_PASSWORD_QR_BYTES: usize = 2331;

pub fn payload(seed: &SeedType) -> Option<&[u8]> {
    let password = match seed {
        SeedType::Password { password, .. } | SeedType::PasswordGenerated { password, .. } => password,
        _ => return None,
    };
    let bytes = password.as_bytes();
    // No serialization, trimming, URI wrapper, or account/seed data belongs here.
    (!bytes.is_empty() && bytes.len() <= MAX_PASSWORD_QR_BYTES).then_some(bytes)
}

pub fn clear(callbacks: &Callbacks<'_>) {
    callbacks.set_password_qr_open(false);
    callbacks.set_password_qr_image(Image::default());
}

pub fn clears_on_input(message: InputMessage) -> bool {
    matches!(
        message,
        InputMessage::Hidden
            | InputMessage::Visible
            | InputMessage::NavigationFocused
            | InputMessage::NavigationCancelled
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(password: &str) -> [SeedType; 2] {
        [
            SeedType::Password { account: "not-in-the-qr".into(), password: password.into() },
            SeedType::PasswordGenerated {
                account: "also-not-in-the-qr".into(),
                password: password.into(),
                index: 42,
            },
        ]
    }

    #[test]
    fn exact_password_bytes_for_both_sources() {
        for password in
            [" x!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~ \r\n", "密碼🔑é e\u{301}", "https://dummy.invalid/?x=1"]
        {
            for entry in entries(password) {
                assert_eq!(payload(&entry), Some(password.as_bytes()));
            }
        }
    }

    #[test]
    fn byte_capacity_boundaries() {
        for password in ["x".to_string(), "x".repeat(MAX_PASSWORD_QR_BYTES), "é".repeat(1165) + "x"] {
            for entry in entries(&password) {
                let bytes = payload(&entry).unwrap();
                assert_eq!(bytes, password.as_bytes());
                // Exercise the actual shared renderer at the accepted boundary.
                let image = slint_keyos_platform::qrcode::render(
                    bytes,
                    slint_keyos_platform::slint::Color::from_rgb_u8(0, 0, 0),
                    slint_keyos_platform::slint::Color::from_rgb_u8(255, 255, 255),
                );
                assert!(image.size().width > 0);
            }
        }
        for password in [String::new(), "x".repeat(MAX_PASSWORD_QR_BYTES + 1), "é".repeat(1166)] {
            for entry in entries(&password) {
                assert!(payload(&entry).is_none());
            }
        }
    }

    #[test]
    fn rejects_non_password_entries() {
        for entry in [
            SeedType::Bitcoin12 { index: 0 },
            SeedType::Bitcoin24 { index: 0 },
            SeedType::NostrKey { index: 0 },
            SeedType::NostrKeyImport { nsec: "dummy-not-a-real-key".into() },
            SeedType::BitcoinImport {
                mnemonic: bip85_extended::bip39::Mnemonic::from_entropy(&[0; 16]).unwrap(),
            },
        ] {
            assert!(payload(&entry).is_none());
        }
    }

    #[test]
    fn visibility_and_navigation_invalidate_reveal_but_typing_does_not() {
        for message in [
            InputMessage::Hidden,
            InputMessage::Visible,
            InputMessage::NavigationFocused,
            InputMessage::NavigationCancelled,
        ] {
            assert!(clears_on_input(message));
        }
        for message in [
            InputMessage::Touch,
            InputMessage::KeyPress,
            InputMessage::KeyRelease,
            InputMessage::Noop,
            InputMessage::FrameBuffer,
        ] {
            assert!(!clears_on_input(message));
        }
    }

    // Source-contract checks complement the dummy payload tests without starting
    // Vault, connecting to services, or exposing a live simulator. They do not
    // substitute for rendered geometry and pointer-event verification.
    const DETAILS: &str = include_str!("../ui/pages/details/page.slint");
    const INPUT: &str = include_str!("../../../ui/ui/widgets/input.slint");
    const TITLED_INPUT: &str = include_str!("../../../ui/ui/widgets/titled-input.slint");

    #[test]
    fn password_field_owns_the_only_guarded_qr_action() {
        let (_, password_field) = DETAILS.split_once("title: TR2.lookup(TrId.CreateItemPassword);").unwrap();
        let (password_field, _) = password_field
            .split_once("if props.version != EditPageVersion.archive: CardColorPicker")
            .unwrap();
        assert!(password_field.contains("input-type: InputType.password;"));
        assert!(password_field.contains("show-password: false;"));
        assert!(password_field.contains("locked: seed.seed-type == SeedViewType.password-generated;"));
        assert!(password_field.contains("enabled: props.version != EditPageVersion.archive;"));
        assert!(password_field.contains("action-icon: \"scan-qr\";"));
        assert!(password_field.contains("action-icon-only: true;"));
        let guard = "!has-changes && !password-error && seed.password.password != \"\"";
        assert!(password_field.contains(&format!("action-enabled: {guard};")));
        assert!(password_field.contains(&format!("if ({guard}) {{")));
        assert!(password_field.contains("CB.reveal-password-qr(seed.index);"));
        assert_eq!(DETAILS.matches("CB.reveal-password-qr(").count(), 1);
        let (_, bottom_buttons) = DETAILS.split_once("ButtonSection {").unwrap();
        assert!(!bottom_buttons.contains("CB.reveal-password-qr("));
    }

    #[test]
    fn icon_action_has_a_separate_target_before_the_eye() {
        let (_, trailing) = INPUT.split_once("if (root.icon-only-action): VerticalLayout {").unwrap();
        let (action, eye) = trailing.split_once("// TODO: Talk to Edi").unwrap();
        for contract in [
            "size: Size.sz12;",
            "icon-size: Size.sz6;",
            "touch-factor: 1;",
            "padding-left: Size.sz1;",
            "padding-right: Size.sz1;",
            "enabled: root.action-enabled;",
            "if (root.action-enabled)",
            "root.action-clicked();",
        ] {
            assert!(action.contains(contract), "missing icon action contract: {contract}");
        }
        assert!(!action.contains("text-input"));
        assert!(!action.contains("show-password"));
        assert!(!action.contains("root.edited("));
        assert!(eye.contains("padding: root.icon-only-action ? Size.sz1 : Size.sz4;"));
        assert!(eye.contains("size: root.icon-only-action ? Size.sz12 : Size.sz6;"));
        assert!(eye.contains("touch-factor: root.icon-only-action ? 1 : 3;"));
        assert!(eye.contains("root.confirm-show-passsword()"));
    }

    #[test]
    fn shared_input_action_defaults_and_wrapper_preserve_editing_contracts() {
        for source in [INPUT, TITLED_INPUT] {
            assert!(source.contains("in property <bool> action-icon-only: false;"));
            assert!(source.contains(
                "in property <bool> action-enabled: root.enabled && !root.has-disabled && !root.locked;"
            ));
        }
        assert!(INPUT.contains("enabled: enabled && !locked;"));
        assert!(INPUT.contains(
            "root.action-icon-only && root.action-icon != \"\" && root.input-type != InputType.number"
        ));
        assert!(INPUT.contains(
            "if (!root.action-icon-only && root.action-label != \"\" && input-type != InputType.number)"
        ));
        for forwarding in [
            "action-label: root.action-label;",
            "action-icon: root.action-icon;",
            "action-icon-only: root.action-icon-only;",
            "action-enabled: root.action-enabled;",
            "root.action-clicked();",
            "locked: locked;",
            "enabled: enabled;",
        ] {
            assert!(TITLED_INPUT.contains(forwarding), "missing wrapper contract: {forwarding}");
        }
    }
}
