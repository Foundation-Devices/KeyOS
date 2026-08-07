// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared BIP-39 word helpers for seed entry and backup verification.

/// Number of candidate words offered per challenge (one correct, the rest decoys).
pub const NUM_OPTIONS: usize = 4;

/// How far a seed validates: `prefix_valid` covers every word but the last,
/// `seed_valid` the whole mnemonic including its checksum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeedStatus {
    pub prefix_valid: bool,
    pub seed_valid: bool,
}

/// True for a word in the BIP-39 English list.
pub fn validate_word(word: &str) -> bool { bip39::Language::English.find_word(word).is_some() }

/// Validate a 12- or 24-word seed. Any other length is invalid throughout.
pub fn seed_status<S: AsRef<str>>(words: &[S]) -> SeedStatus {
    let prefix_valid = matches!(words.len(), 12 | 24)
        && words[..words.len() - 1].iter().all(|word| validate_word(word.as_ref()));
    let seed_valid = prefix_valid && {
        // parse_normalized rejects an empty or checksum-invalid final word, so this
        // accepts only a complete seed.
        let phrase = words.iter().map(|word| word.as_ref()).collect::<Vec<_>>().join(" ");
        bip39::Mnemonic::parse_normalized(&phrase).is_ok()
    };
    SeedStatus { prefix_valid, seed_valid }
}

/// Suggest a checksum-valid final word for a 12- or 24-word entry.
///
/// A random filler word supplies the remaining entropy bits. `from_entropy`
/// recomputes the checksum, and retrying rejects the word already displayed so
/// the result always changes without touching the user's prefix words.
pub fn random_last_word<S: AsRef<str>>(words: &[S]) -> Option<String> {
    let (current, prefix) = words.split_last()?;
    let current: &str = current.as_ref();
    let (candidate_count, checksum_bits) = match prefix.len() {
        11 => (128, 4),
        23 => (8, 8),
        _ => return None,
    };
    let word_list = bip39::Language::English.word_list();
    let current_variant = (!current.is_empty())
        .then(|| words.iter().map(|word| word.as_ref()).collect::<Vec<_>>().join(" "))
        .filter(|phrase| bip39::Mnemonic::parse_normalized(phrase).is_ok())
        .and_then(|_| word_list.iter().position(|candidate| *candidate == current))
        .map(|word_index| word_index >> checksum_bits);
    let variant = match current_variant {
        Some(current) => {
            let random = rand_range(candidate_count - 1);
            random + usize::from(random >= current)
        }
        None => rand_range(candidate_count),
    };

    let filler = word_list[variant << checksum_bits];
    let phrase = prefix.iter().map(|word| word.as_ref()).chain([filler]).collect::<Vec<_>>().join(" ");
    let entropy =
        bip39::Mnemonic::parse_in_normalized_without_checksum_check(bip39::Language::English, &phrase)
            .ok()?
            .to_entropy();
    bip39::Mnemonic::from_entropy(&entropy).ok()?.words().last().map(str::to_owned)
}

/// Bind the `SeedCallbacks` global of `$ui` to this crate's BIP-39 helpers.
///
/// Expands where the app's generated `SeedCallbacks` and `SeedStatus` are in
/// scope, since this crate cannot name them itself. Callers must depend on
/// `slint-keyos-platform`.
#[macro_export]
macro_rules! init_seed_callbacks {
    ($ui:expr) => {{
        use slint_keyos_platform::slint::Model;

        let global = $ui.global::<SeedCallbacks>();
        global.on_validate_seed_word(|word| $crate::validate_word(word.as_str()));
        global.on_validate_seed(|words| {
            let words = words.iter().map(|word| word.to_string()).collect::<Vec<_>>();
            let $crate::SeedStatus { prefix_valid, seed_valid } = $crate::seed_status(&words);
            SeedStatus { prefix_valid, seed_valid }
        });
        global.on_suggest_last_word(|words| {
            let words = words.iter().map(|word| word.to_string()).collect::<Vec<_>>();
            $crate::random_last_word(&words).unwrap_or_default().into()
        });
    }};
}

/// One word-position challenge: the correct word plus `NUM_OPTIONS - 1` distinct
/// random BIP-39 decoys, in a randomized order.
///
/// No `Debug`: `options` holds the real mnemonic word being verified, so a diagnostic dump would
/// leak seed words.
#[derive(Clone, PartialEq, Eq)]
pub struct SeedWordChallenge {
    /// 0-based position in the mnemonic being tested.
    pub word_index: usize,
    /// `NUM_OPTIONS` candidate words, exactly one correct.
    pub options: [String; NUM_OPTIONS],
    /// Index into `options` of the correct answer.
    pub correct_option_index: usize,
}

/// Build the challenge for `word_index`, or `None` if it is out of range for the mnemonic.
pub fn word_challenge(mnemonic: &bip39::Mnemonic, word_index: usize) -> Option<SeedWordChallenge> {
    let correct = mnemonic.words().nth(word_index)?;
    let word_list = bip39::Language::English.word_list();

    let correct_option_index = rand_range(NUM_OPTIONS);
    let mut options: Vec<String> = Vec::with_capacity(NUM_OPTIONS);
    while options.len() < NUM_OPTIONS {
        if options.len() == correct_option_index {
            options.push(correct.to_string());
            continue;
        }
        let candidate = word_list[rand_range(word_list.len())];
        if candidate != correct && !options.iter().any(|o| o == candidate) {
            options.push(candidate.to_string());
        }
    }
    // `while` loop above pushes exactly NUM_OPTIONS, so this cannot fail.
    let options: [String; NUM_OPTIONS] = options.try_into().ok()?;
    Some(SeedWordChallenge { word_index, options, correct_option_index })
}

/// Return the mnemonic word positions in a randomised order.
pub fn shuffled_word_indices(mnemonic: &bip39::Mnemonic) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..mnemonic.word_count()).collect();
    for i in (1..indices.len()).rev() {
        let j = rand_range(i + 1);
        indices.swap(i, j);
    }
    indices
}

/// One challenge per word, the positions shuffled into a quiz order.
pub fn shuffled_challenges(mnemonic: &bip39::Mnemonic) -> Vec<SeedWordChallenge> {
    shuffled_word_indices(mnemonic)
        .into_iter()
        .map(|i| word_challenge(mnemonic, i).expect("shuffled word index is in range"))
        .collect()
}

/// return an index in `0..upper`.
fn rand_range(upper: usize) -> usize {
    #[cfg(not(test))]
    fn execute(upper: u32) -> u32 {
        let limit = u32::MAX - u32::MAX % upper;

        // reject values that would bias the modulo result
        loop {
            let mut bytes = [0; 4];
            getrandom::getrandom(&mut bytes).unwrap();
            let value = u32::from_ne_bytes(bytes);
            if value < limit {
                return value % upper;
            }
        }
    }

    #[cfg(test)]
    fn execute(upper: u32) -> u32 {
        use std::cell::Cell;

        std::thread_local! {
            static NEXT: Cell<u32> = const { Cell::new(0) };
        }

        NEXT.with(|next| {
            let value = next.get();
            next.set(value.wrapping_add(1));
            value % upper
        })
    }

    assert!(upper > 0);
    let upper = u32::try_from(upper).unwrap();
    execute(upper) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    // Any valid 12-word mnemonic; the quiz logic doesn't depend on the words.
    fn test_mnemonic() -> bip39::Mnemonic { bip39::Mnemonic::from_entropy(&[0x42u8; 16]).unwrap() }

    #[test]
    fn challenge_is_valid_for_every_word() {
        let mnemonic = test_mnemonic();
        let words: Vec<&str> = mnemonic.words().collect();
        for i in 0..words.len() {
            let c = word_challenge(&mnemonic, i).unwrap();
            assert_eq!(c.word_index, i);
            assert!(c.correct_option_index < NUM_OPTIONS);
            assert_eq!(c.options[c.correct_option_index], words[i]);
        }
    }

    #[test]
    fn options_are_distinct() {
        let mnemonic = test_mnemonic();
        for i in 0..mnemonic.word_count() {
            let c = word_challenge(&mnemonic, i).unwrap();
            for a in 0..NUM_OPTIONS {
                for b in (a + 1)..NUM_OPTIONS {
                    assert_ne!(c.options[a], c.options[b], "duplicate option {}", c.options[a]);
                }
            }
        }
    }

    #[test]
    fn out_of_range_word_index_is_none() {
        let mnemonic = test_mnemonic();
        assert!(word_challenge(&mnemonic, mnemonic.word_count()).is_none());
    }

    #[test]
    fn shuffled_covers_every_word_once() {
        let mnemonic = test_mnemonic();
        let challenges = shuffled_challenges(&mnemonic);
        let mut indices: Vec<usize> = challenges.iter().map(|c| c.word_index).collect();
        indices.sort_unstable();
        assert_eq!(indices, (0..mnemonic.word_count()).collect::<Vec<_>>());
    }

    #[test]
    fn generated_last_word_completes_12_word_seed() {
        let mut words = ["abandon"; 12];
        words[11] = "";
        let last_word = random_last_word(&words).unwrap();
        let phrase = words[..11].iter().copied().chain([last_word.as_str()]).collect::<Vec<_>>().join(" ");

        assert!(bip39::Mnemonic::parse_normalized(&phrase).is_ok());
    }

    #[test]
    fn generated_last_word_completes_24_word_seed() {
        let mut words = ["abandon"; 24];
        words[23] = "";
        let last_word = random_last_word(&words).unwrap();
        let phrase = words[..23].iter().copied().chain([last_word.as_str()]).collect::<Vec<_>>().join(" ");

        assert!(bip39::Mnemonic::parse_normalized(&phrase).is_ok());
    }

    #[test]
    fn different_entropy_can_reroll_the_last_word() {
        let mut words = vec!["abandon".to_owned(); 12];
        words[11].clear();
        let first = random_last_word(&words).unwrap();
        words[11] = first.clone();
        let second = random_last_word(&words).unwrap();

        assert_ne!(first, second);
    }

    #[test]
    fn invalid_current_word_does_not_exclude_a_completion() {
        let mut words = ["abandon"; 12];
        words[11] = "zoo";
        assert!(random_last_word(&words).is_some());
    }

    #[test]
    fn rejects_invalid_or_unsupported_prefixes() {
        assert!(random_last_word(&["not-a-bip39-word"; 12]).is_none());
        assert!(random_last_word(&["abandon"; 11]).is_none());
    }
}
