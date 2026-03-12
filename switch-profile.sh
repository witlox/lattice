#!/bin/bash

# Profile switcher for Claude Code workflow
# Usage: ./switch-profile.sh <profile-name> [feature-scope]
#
# Examples:
#   ./switch-profile.sh analyst
#   ./switch-profile.sh architect
#   ./switch-profile.sh adversary
#   ./switch-profile.sh contract-gen
#   ./switch-profile.sh implementer "user-authentication"
#   ./switch-profile.sh integrator

set -euo pipefail

PROFILES_DIR=".claude"
TARGET=".claude/CLAUDE.md"

VALID_PROFILES=("analyst" "architect" "adversary" "contract-gen" "implementer" "integrator")

usage() {
    echo "Usage: $0 <profile-name> [feature-scope]"
    echo ""
    echo "Available profiles:"
    for p in "${VALID_PROFILES[@]}"; do
        echo "  - $p"
    done
    echo ""
    echo "The optional [feature-scope] argument is used with the implementer profile"
    echo "to set the current feature being implemented."
    exit 1
}

if [ $# -lt 1 ]; then
    usage
fi

PROFILE="$1"
FEATURE_SCOPE="${2:-}"

# Validate profile name
VALID=false
for p in "${VALID_PROFILES[@]}"; do
    if [ "$PROFILE" = "$p" ]; then
        VALID=true
        break
    fi
done

if [ "$VALID" = false ]; then
    echo "Error: Unknown profile '$PROFILE'"
    usage
fi

SOURCE="${PROFILES_DIR}/${PROFILE}.md"

if [ ! -f "$SOURCE" ]; then
    echo "Error: Profile file not found: $SOURCE"
    exit 1
fi

# Back up current CLAUDE.md if it exists
if [ -f "$TARGET" ]; then
    # Detect what profile is currently active
    CURRENT=$(head -1 "$TARGET" | sed 's/# Role: //' | tr '[:upper:]' '[:lower:]' | tr ' ' '-')
    echo "Deactivating current profile: $CURRENT"
fi

# Copy profile
cp "$SOURCE" "$TARGET"

# If implementer and feature scope provided, prepend scope
if [ "$PROFILE" = "implementer" ] && [ -n "$FEATURE_SCOPE" ]; then
    SCOPE_LINE="# CURRENT SCOPE: Feature ${FEATURE_SCOPE} — specs/features/${FEATURE_SCOPE}.feature"
    # Prepend scope line
    echo -e "${SCOPE_LINE}\n\n$(cat "$TARGET")" > "$TARGET"
    echo "Activated profile: $PROFILE (scope: $FEATURE_SCOPE)"
else
    echo "Activated profile: $PROFILE"
fi

echo ".claude/CLAUDE.md updated. Start a new Claude Code session to use the new profile."
