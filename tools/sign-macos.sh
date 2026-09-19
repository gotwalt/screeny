#!/usr/bin/env bash
# Build and code-sign the `screeny` sender for macOS.
#
# macOS keys Local Network permission to a binary's code-signing identity. An
# unsigned or ad-hoc Rust binary gets a new identity on every rebuild, so the
# permission never sticks; a Developer ID signature with a fixed identifier does.
# The Info.plist embedded by crates/screeny/build.rs carries the usage string and
# the Bonjour service type.
#
# Usage: tools/sign-macos.sh [--debug]
#   SCREENY_SIGN_IDENTITY overrides the identity (name, team id or "-" for ad hoc).
# The first run makes macOS ask for keychain access to the signing key: choose
# "Always Allow".
set -euo pipefail
cd "$(dirname "$0")/.."

PROFILE=release; FLAG=--release
if [[ "${1:-}" == "--debug" ]]; then PROFILE=debug; FLAG=; fi
IDENTITY=${SCREENY_SIGN_IDENTITY:-"Developer ID Application: Aaron Gotwalt (L2EG537FL9)"}
BIN=target/$PROFILE/screeny

cargo build $FLAG -p screeny
codesign --force --sign "$IDENTITY" --identifier com.gotwalt.screeny \
  --options runtime --timestamp=none "$BIN"
codesign --verify --strict --verbose=2 "$BIN"
codesign -dv "$BIN" 2>&1 | grep -E "Identifier|Authority|TeamIdentifier|Info.plist|flags" | head -8
echo "signed: $BIN"
