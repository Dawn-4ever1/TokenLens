#!/usr/bin/env bash
# Builds the unsigned Mac app and dmg into target/release/bundle/.
# Build only: never open or install the result on a development machine,
# because a release build uses the real keychain and autostart.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"
# Panic locations would otherwise embed this machine's home path (and user name).
# The last matching prefix wins, so the repo maps to `tokenlens`, the rest to `~`.
export RUSTFLAGS="--remap-path-prefix=$HOME=~ --remap-path-prefix=$repo=tokenlens"
# Skips the Finder window styling, which needs an Automation permission prompt.
export CI=true

cd "$repo/app"
npm run tauri build -- --bundles app,dmg

bundle="$repo/target/release/bundle"
binary="$bundle/macos/TokenLens.app/Contents/MacOS/tokenlens-app"
leaks="$(strings -a "$binary" | grep -c -F "$HOME" || true)"
if [ "$leaks" != "0" ]; then
  echo "The app binary still contains this machine's home path ($leaks places)." >&2
  exit 1
fi
# A broken ad-hoc signature makes macOS call a downloaded copy "damaged".
codesign --verify --deep --strict "$bundle/macos/TokenLens.app"
hdiutil verify "$bundle"/dmg/*.dmg >/dev/null
du -sh "$bundle/macos/TokenLens.app" "$bundle"/dmg/*.dmg
echo "Bundle ready; no home path inside the app binary."
