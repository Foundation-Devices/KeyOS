# SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: GPL-3.0-or-later

"""Small format checks for the standalone Legacy release preparation helper."""

import importlib.util
from contextlib import redirect_stdout
import hashlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name("prepare-legacy-apps.py")
SPEC = importlib.util.spec_from_file_location("prepare_legacy_apps", SCRIPT)
HELPER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HELPER)


class ReleasePreparationTests(unittest.TestCase):
    def test_release_baseline_uses_published_tag_instead_of_local_tag(self):
        ref = "refs/tags/v1.4.1^{}"
        with patch.object(HELPER, "git", return_value=f"{HELPER.RELEASED_KEYOS_SOURCE}\t{ref}") as git:
            self.assertEqual(HELPER.released_keyos_commit("1.4.1"), HELPER.RELEASED_KEYOS_SOURCE)
        git.assert_called_once_with(HELPER.SOURCE_ROOT, "ls-remote", "--tags", "origin", ref)

    def test_release_baseline_rejects_missing_or_changed_remote_tag(self):
        ref = "refs/tags/v1.4.1^{}"
        for published in ("", f"{'0' * 40}\t{ref}"):
            with self.subTest(published=published), patch.object(HELPER, "git", return_value=published):
                with self.assertRaisesRegex(HELPER.PreparationError, "does not match"):
                    HELPER.released_keyos_commit("1.4.1")

    def test_pr_failure_after_push_prints_manual_link(self):
        output = io.StringIO()
        with patch.object(HELPER.shutil, "which", return_value="/usr/bin/gh"), \
             patch.object(HELPER, "run", side_effect=HELPER.PreparationError("gh unavailable")), \
             redirect_stdout(output):
            HELPER.create_release_pr(Path("/tmp/release"), "legacy-mode-v1.1.1", "abcd")
        self.assertIn("Branch pushed, but PR creation failed", output.getvalue())
        self.assertIn("compare/main...legacy-mode-v1.1.1", output.getvalue())

    def test_all_ten_unsigned_bundles_include_rebuilt_solana(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            bundles = root / HELPER.ARTIFACT_DIR
            for crate in HELPER.RELEASE_CRATES:
                bundle = HELPER.artifact_name(crate)
                path = bundles / bundle
                path.mkdir(parents=True)
                elf = b"\x7fELF" + bundle.encode()
                icon = b"icon" + bundle.encode()
                (path / "app.elf").write_bytes(elf)
                (path / "icon.bin").write_bytes(icon)
                version = "1.1.1+1.15.2" if crate == "app-flux-solana" else "1.1.1+2.0.0"
                manifest = {
                    "appName": {"en": bundle},
                    "version": version,
                    "appId": bundle,
                    "publisher": "Foundation",
                    "fileHashes": {
                        "app.elf": hashlib.sha256(elf).hexdigest(),
                        "icon.bin": hashlib.sha256(icon).hexdigest(),
                    },
                }
                (path / "manifest.json").write_text(json.dumps(manifest, separators=(",", ":")))

            with patch.object(HELPER, "SOURCE_ROOT", root), \
                 patch.object(HELPER, "crate_version", side_effect=lambda crate: "1.1.1+1.15.2" if crate == "app-flux-solana" else "1.1.1+2.0.0"), \
                 patch.object(HELPER, "source_manifest", side_effect=lambda crate: {"appId": HELPER.artifact_name(crate)}):
                receipt = HELPER.check_built_bundles()
                self.assertEqual(len(receipt), 10)
                self.assertEqual(receipt["app-flux-solana"]["version"], "1.1.1+1.15.2")
                self.assertEqual(receipt["app-flux-solana"]["manifestSha256"],
                                 hashlib.sha256((bundles / "app-flux-solana/manifest.json").read_bytes()).hexdigest())

                (bundles / "app-flux-solana/manifest.json").write_bytes(b"PRM1" + bytes(2044))
                with self.assertRaisesRegex(HELPER.PreparationError, "was signed"):
                    HELPER.check_built_bundles()


if __name__ == "__main__":
    unittest.main()
