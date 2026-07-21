// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared generator for the "pick the word at position N" seed backup quiz, used
//! by onboarding's backup verification and the Legacy remove-seed flow.

use rand::{seq::SliceRandom, Rng};

/// Number of candidate words offered per challenge (one correct, the rest decoys).
pub const NUM_OPTIONS: usize = 4;

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
pub fn word_challenge(
    mnemonic: &bip39::Mnemonic,
    word_index: usize,
    rng: &mut impl Rng,
) -> Option<SeedWordChallenge> {
    let words: Vec<&str> = mnemonic.words().collect();
    let correct = *words.get(word_index)?;
    let word_list = bip39::Language::English.word_list();

    let correct_option_index = rng.gen_range(0..NUM_OPTIONS);
    let mut options: Vec<String> = Vec::with_capacity(NUM_OPTIONS);
    while options.len() < NUM_OPTIONS {
        if options.len() == correct_option_index {
            options.push(correct.to_string());
            continue;
        }
        let candidate = word_list[rng.gen_range(0..word_list.len())];
        if candidate != correct && !options.iter().any(|o| o == candidate) {
            options.push(candidate.to_string());
        }
    }
    // `while` loop above pushes exactly NUM_OPTIONS, so this cannot fail.
    let options: [String; NUM_OPTIONS] = options.try_into().ok()?;
    Some(SeedWordChallenge { word_index, options, correct_option_index })
}

/// One challenge per word, the positions shuffled into a quiz order.
pub fn shuffled_challenges(mnemonic: &bip39::Mnemonic, rng: &mut impl Rng) -> Vec<SeedWordChallenge> {
    let mut order: Vec<usize> = (0..mnemonic.word_count()).collect();
    order.shuffle(rng);
    order.into_iter().filter_map(|i| word_challenge(mnemonic, i, rng)).collect()
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
        let mut rng = rand::thread_rng();
        for i in 0..words.len() {
            let c = word_challenge(&mnemonic, i, &mut rng).unwrap();
            assert_eq!(c.word_index, i);
            assert!(c.correct_option_index < NUM_OPTIONS);
            assert_eq!(c.options[c.correct_option_index], words[i]);
        }
    }

    #[test]
    fn options_are_distinct() {
        let mnemonic = test_mnemonic();
        let mut rng = rand::thread_rng();
        for i in 0..mnemonic.word_count() {
            let c = word_challenge(&mnemonic, i, &mut rng).unwrap();
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
        assert!(word_challenge(&mnemonic, mnemonic.word_count(), &mut rand::thread_rng()).is_none());
    }

    #[test]
    fn shuffled_covers_every_word_once() {
        let mnemonic = test_mnemonic();
        let challenges = shuffled_challenges(&mnemonic, &mut rand::thread_rng());
        let mut indices: Vec<usize> = challenges.iter().map(|c| c.word_index).collect();
        indices.sort_unstable();
        assert_eq!(indices, (0..mnemonic.word_count()).collect::<Vec<_>>());
    }
}
