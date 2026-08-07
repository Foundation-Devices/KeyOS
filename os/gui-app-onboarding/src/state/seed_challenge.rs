// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use super::AppState;

impl AppState {
    /// Return the mnemonic word positions in a randomised order for use as a quiz sequence.
    pub fn mnemonic_order(&self) -> anyhow::Result<Vec<usize>> {
        let mnemonic = self.try_get_seed()?.to_mnemonic()?;
        Ok(seed_quiz::shuffled_word_indices(&mnemonic))
    }

    /// Generate a fresh challenge for the word at `mnemonic_index` with randomised distractor
    /// options.
    ///
    /// # Errors
    ///
    /// Errors if no seed is set, or if `mnemonic_index` is past the end of the mnemonic.
    pub fn get_seed_word_challenge(
        &self,
        mnemonic_index: usize,
    ) -> anyhow::Result<seed_quiz::SeedWordChallenge> {
        let mnemonic = self.try_get_seed()?.to_mnemonic()?;
        seed_quiz::word_challenge(&mnemonic, mnemonic_index).ok_or_else(|| {
            anyhow::anyhow!(
                "mnemonic_index {mnemonic_index} out of range (seed has {} words)",
                mnemonic.word_count()
            )
        })
    }
}
