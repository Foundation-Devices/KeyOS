// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    super::AppState,
    anyhow::Context,
    rand::{seq::SliceRandom, Rng},
    security::Seed,
};

const NUM_CHALLENGE_OPTIONS: usize = 4;

/// Represents a seed word verification challenge
#[derive(Clone, Debug, PartialEq)]
pub struct SeedWordChallenge {
    /// Which word position we're verifying (0-based)
    pub mnemonic_index: usize,
    /// The multiple choice options
    pub options: [String; NUM_CHALLENGE_OPTIONS],
    /// Which option is correct (0-3)
    pub correct_option_index: usize,
}

impl AppState {
    /// Return the mnemonic word positions in a randomised order for use as a quiz sequence.
    pub fn mnemonic_order(&self) -> anyhow::Result<Vec<usize>> {
        let word_count = self.try_get_seed()?.to_mnemonic()?.word_count();
        let mut indices: Vec<usize> = (0..word_count).collect();
        indices.shuffle(&mut rand::thread_rng());
        Ok(indices)
    }

    /// Generate a fresh challenge for the word at `mnemonic_index` with randomised distractor options.
    pub fn get_seed_word_challenge(&self, mnemonic_index: usize) -> anyhow::Result<SeedWordChallenge> {
        get_seed_word_challenge(&self.try_get_seed()?, mnemonic_index)
    }
}

fn get_seed_word_challenge(seed: &Seed, mnemonic_index: usize) -> anyhow::Result<SeedWordChallenge> {
    let mnemonic = seed.to_mnemonic()?;
    let mnemonic_words: Vec<&str> = mnemonic.words().collect();

    let correct_word = mnemonic_words
        .get(mnemonic_index)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "mnemonic_index {mnemonic_index} out of range (seed has {} words)",
                mnemonic_words.len()
            )
        })?
        .to_string();

    let bip39_words = bip39::Language::English.word_list();
    let mut rng = rand::thread_rng();
    let correct_option_index = rng.gen_range(0..NUM_CHALLENGE_OPTIONS);
    let mut option_list = Vec::<String>::new();

    while option_list.len() < NUM_CHALLENGE_OPTIONS {
        if option_list.len() == correct_option_index {
            option_list.push(correct_word.clone());
            continue;
        }

        let bip39_word = bip39_words[rng.gen_range(0..bip39_words.len())].to_string();
        if !option_list.contains(&bip39_word) && bip39_word != correct_word {
            option_list.push(bip39_word);
        }
    }

    let options: [String; NUM_CHALLENGE_OPTIONS] =
        option_list.try_into().ok().context("bug: wrong number of challenge options")?;

    Ok(SeedWordChallenge { mnemonic_index, options, correct_option_index })
}

#[cfg(test)]
mod tests {
    use rand::RngCore;

    use super::*;

    fn create_test_seed() -> Seed {
        let mut seed_bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut seed_bytes);
        Seed::Twelve(seed_bytes)
    }

    #[test]
    fn test_challenge_validity() {
        let seed = create_test_seed();
        let seed_words = seed.to_mnemonic_words().unwrap();

        for mnemonic_index in 0..seed_words.len() {
            let challenge = get_seed_word_challenge(&seed, mnemonic_index).unwrap();

            assert_eq!(challenge.options.len(), NUM_CHALLENGE_OPTIONS);
            assert!(
                challenge.correct_option_index < NUM_CHALLENGE_OPTIONS,
                "Invalid correct option index: {}",
                challenge.correct_option_index
            );
            assert_eq!(challenge.mnemonic_index, mnemonic_index);

            let correct_option = &challenge.options[challenge.correct_option_index];
            assert_eq!(correct_option, &seed_words[mnemonic_index], "Correct option doesn't match seed word");
        }
    }

    #[test]
    fn test_no_duplicate_options() {
        let seed = create_test_seed();
        let seed_words = seed.to_mnemonic_words().unwrap();

        for mnemonic_index in 0..seed_words.len() {
            let challenge = get_seed_word_challenge(&seed, mnemonic_index).unwrap();

            for i in 0..NUM_CHALLENGE_OPTIONS {
                for j in (i + 1)..NUM_CHALLENGE_OPTIONS {
                    assert_ne!(
                        challenge.options[i], challenge.options[j],
                        "Duplicate option found: {}",
                        challenge.options[i]
                    );
                }
            }
        }
    }

    #[test]
    fn test_get_seed_word_challenge() {
        let seed = create_test_seed();
        let seed_words = seed.to_mnemonic_words().unwrap();
        let mnemonic_index = 0;

        let challenge = get_seed_word_challenge(&seed, mnemonic_index).unwrap();

        assert_eq!(challenge.mnemonic_index, mnemonic_index);
        assert_eq!(challenge.options[challenge.correct_option_index], seed_words[mnemonic_index]);
        assert!(challenge.correct_option_index < NUM_CHALLENGE_OPTIONS);

        for i in 0..NUM_CHALLENGE_OPTIONS {
            for j in (i + 1)..NUM_CHALLENGE_OPTIONS {
                assert_ne!(
                    challenge.options[i], challenge.options[j],
                    "Duplicate option: {}",
                    challenge.options[i]
                );
            }
        }
    }
}
