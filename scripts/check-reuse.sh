#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: GPL-3.0-or-later

set -euo pipefail

# reuse is VCS-aware: it skips git-ignored files, so linting the working tree
# directly gives the same result as copying tracked files into a temp dir, but
# without the copy.
cd "$(git rev-parse --show-toplevel)"
reuse lint
