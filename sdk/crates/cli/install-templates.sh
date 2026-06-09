#!/bin/bash
# SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: MIT

# Install foundation templates to user data directory

set -e

# Determine template installation directory
if [[ "$OSTYPE" == "darwin"* ]]; then
    # macOS
    TEMPLATE_DIR="$HOME/Library/Application Support/foundation/templates"
elif [[ "$OSTYPE" == "linux-gnu"* ]]; then
    # Linux
    TEMPLATE_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/foundation/templates"
else
    # Fallback
    TEMPLATE_DIR="$HOME/.config/foundation/templates"
fi

echo "Installing templates to: $TEMPLATE_DIR"

# Create directory if it doesn't exist
mkdir -p "$TEMPLATE_DIR"

# Copy templates
cp -r templates/* "$TEMPLATE_DIR/"

echo "✓ Templates installed successfully!"
echo ""
echo "Available templates:"
for template in "$TEMPLATE_DIR"/*; do
    if [ -d "$template" ]; then
        basename "$template"
    fi
done
