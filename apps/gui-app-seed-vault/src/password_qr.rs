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
}
