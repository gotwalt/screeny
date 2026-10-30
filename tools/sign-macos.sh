#!/usr/bin/env bash
# Build and code-sign a sender for macOS: `screeny`, or `screeny-art` (whose
# `play` streams patches straight to a panel).
#
# macOS keys Local Network permission to a binary's code-signing identity. An
# unsigned or ad-hoc Rust binary gets a new identity on every rebuild, so the
# permission never sticks; a Developer ID signature with a fixed identifier does.
# The Info.plist each crate's build.rs embeds (crates/screeny, crates/art)
# carries the usage string and the Bonjour service type.
#
# Usage: tools/sign-macos.sh [--debug] [screeny|screeny-art]   (default screeny)
#   SCREENY_SIGN_IDENTITY sets the identity (name, team id or "-" for ad hoc) -
#   required, no default.
# The first run makes macOS ask for keychain access to the signing key: choose
# "Always Allow".
set -euo pipefail
cd "$(dirname "$0")/.."

PROFILE=release; FLAG=--release; NAME=screeny
for arg in "$@"; do
  case "$arg" in
    --debug) PROFILE=debug; FLAG= ;;
    screeny|screeny-art) NAME=$arg ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done
if [[ -z "${SCREENY_SIGN_IDENTITY:-}" ]]; then
  echo "SCREENY_SIGN_IDENTITY is not set: it names the code-signing identity macOS ties Local Network permission to." >&2
  echo "List yours with: security find-identity -v -p codesigning   (or use \"-\" for ad hoc, which does not make Local Network permission stick)" >&2
  exit 1
fi
IDENTITY=$SCREENY_SIGN_IDENTITY
BIN=target/$PROFILE/$NAME

if [[ $NAME == screeny-art ]]; then cargo build $FLAG -p screeny-art --bin screeny-art; else cargo build $FLAG -p screeny; fi
codesign --force --sign "$IDENTITY" --identifier "com.gotwalt.$NAME" \
  --options runtime --timestamp=none "$BIN"
codesign --verify --strict --verbose=2 "$BIN"
codesign -dv "$BIN" 2>&1 | grep -E "Identifier|Authority|TeamIdentifier|Info.plist|flags" | head -8
echo "signed: $BIN"
