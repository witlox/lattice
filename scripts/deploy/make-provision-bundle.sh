#!/usr/bin/env bash
# make-provision-bundle.sh — Create a single tarball for lattice node provisioning.
#
# Bundles release binaries, deploy scripts, systemd units, and config templates
# into one file for easy distribution to nodes.
#
# Usage: ./make-provision-bundle.sh <release-dir> <output-path>
#   release-dir: directory containing lattice release binaries (lattice-server, lattice-agent, lattice)
#   output-path: where to write the bundle (e.g., /tmp/lattice-provision.tar.gz)
#
# Extract on target: tar xzf lattice-provision.tar.gz -C /opt/lattice

set -euo pipefail

RELEASE_DIR="${1:?Usage: make-provision-bundle.sh <release-dir> <output-path>}"
OUTPUT="${2:?}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

# Create bundle structure
mkdir -p "$TMPDIR/bin" "$TMPDIR/scripts" "$TMPDIR/systemd" "$TMPDIR/config"

# Copy binaries
for bin in lattice-server lattice-agent lattice; do
    if [ -f "$RELEASE_DIR/$bin" ]; then
        cp "$RELEASE_DIR/$bin" "$TMPDIR/bin/"
        chmod +x "$TMPDIR/bin/$bin"
    fi
done

# Copy deploy scripts
cp "$REPO_ROOT/scripts/deploy/"*.sh "$TMPDIR/scripts/" 2>/dev/null || true

# Copy systemd units
cp "$REPO_ROOT/infra/systemd/"*.service "$TMPDIR/systemd/"
cp "$REPO_ROOT/infra/systemd/"*.env "$TMPDIR/systemd/"

# Copy config templates
for f in "$REPO_ROOT/infra/docker/configs/"*.yaml; do
    [ -f "$f" ] && cp "$f" "$TMPDIR/config/"
done

# Create bundle
tar czf "$OUTPUT" -C "$TMPDIR" .

echo "Provisioning bundle created: $OUTPUT ($(du -h "$OUTPUT" | cut -f1))"
echo "Contents:"
tar tzf "$OUTPUT" | head -20
