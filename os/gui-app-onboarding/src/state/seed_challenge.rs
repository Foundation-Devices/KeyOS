// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use super::AppState;

impl AppState {
    /// One verification challenge per seed word, positions shuffled into quiz order.
    /// The quiz itself lives in `seed-quiz`, shared with the Legacy remove-seed flow.
    pub fn seed_verify_challenges(&self) -> anyhow::Result<Vec<seed_quiz::SeedWordChallenge>> {
        let mnemonic = self.try_get_seed()?.to_mnemonic()?;
        Ok(seed_quiz::shuffled_challenges(&mnemonic, &mut rand::thread_rng()))
    }

    /// A fresh challenge for a single `word_index` (new decoys, reshuffled order), for
    /// re-rolling a word the user missed. `None` if the index is out of range for the
    /// mnemonic.
    pub fn seed_verify_challenge(
        &self,
        word_index: usize,
    ) -> anyhow::Result<Option<seed_quiz::SeedWordChallenge>> {
        let mnemonic = self.try_get_seed()?.to_mnemonic()?;
        Ok(seed_quiz::word_challenge(&mnemonic, word_index, &mut rand::thread_rng()))
    }
}
