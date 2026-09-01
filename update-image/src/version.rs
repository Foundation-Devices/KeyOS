// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

/// Beta number standing for a final release.
const RELEASE: u8 = 0xFF;

/// Firmware version, as spelled in a release manifest and encoded in an updiff
/// header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub major: u8,
    pub minor: u8,
    pub patch: u8,
    pub beta: u8,
}

impl Version {
    pub const SIZE: usize = 4;

    /// Parse `vMAJOR.MINOR.PATCH`, or `vMAJOR.MINOR.PATCHbBETA` for a beta.
    pub fn parse(s: &str) -> Result<Self, &'static str> {
        let s = s.strip_prefix('v').ok_or("version must start with 'v'")?;
        let (major, rest) = s.split_once('.').ok_or("missing major version")?;
        let (minor, patch_and_beta) = rest.split_once('.').ok_or("missing minor version")?;
        let (patch, beta) = patch_and_beta.split_once('b').unwrap_or((patch_and_beta, ""));

        let major = major.parse().map_err(|_| "major version invalid or out of range")?;
        let minor = minor.parse().map_err(|_| "minor version invalid or out of range")?;
        let patch = patch.parse().map_err(|_| "patch version invalid or out of range")?;
        let beta = if beta.is_empty() {
            RELEASE
        } else {
            let beta = beta.parse().map_err(|_| "beta version invalid or out of range")?;
            if beta == RELEASE {
                return Err("beta version may not be 0xFF");
            }
            beta
        };

        Ok(Self { major, minor, patch, beta })
    }

    pub fn to_bytes(self) -> [u8; Self::SIZE] { [self.major, self.minor, self.patch, self.beta] }

    pub fn from_bytes(bytes: [u8; Self::SIZE]) -> Self {
        Self { major: bytes[0], minor: bytes[1], patch: bytes[2], beta: bytes[3] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_release() {
        let version = Version::parse("v1.4.0").unwrap();
        assert_eq!(version, Version { major: 1, minor: 4, patch: 0, beta: RELEASE });
        assert_eq!(version.to_bytes(), [1, 4, 0, 0xFF]);
    }

    #[test]
    fn parses_a_beta() {
        let version = Version::parse("v1.4.0b3").unwrap();
        assert_eq!(version, Version { major: 1, minor: 4, patch: 0, beta: 3 });
    }

    #[test]
    fn round_trips_through_bytes() {
        for s in ["v1.4.0", "v1.4.0b3", "v0.0.0", "v255.255.255b254"] {
            let version = Version::parse(s).unwrap();
            assert_eq!(Version::from_bytes(version.to_bytes()), version);
        }
    }

    #[test]
    fn rejects_bad_spellings() {
        for s in ["1.4.0", "v1.4", "v1.4.0b255", "v1.4.256", "vx.4.0"] {
            assert!(Version::parse(s).is_err(), "{s} should not parse");
        }
    }
}
