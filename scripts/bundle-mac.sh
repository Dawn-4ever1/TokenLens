#!/usr/bin/env bash
# Builds the ad-hoc-signed Mac app and dmg under
# ${CARGO_TARGET_DIR:-target}/release/bundle/.
# Build only: never open or install the result on a development machine,
# because a release build uses the real keychain and autostart.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
target="${CARGO_TARGET_DIR:-$repo/target}"
case "$target" in /*) ;; *) target="$repo/$target" ;; esac
export CARGO_TARGET_DIR="$target"
export PATH="$HOME/.cargo/bin:$PATH"
# Panic locations would otherwise embed this machine's home path (and user name).
# The last matching prefix wins, so the repo maps to `tokenlens`, the rest to `~`.
export RUSTFLAGS="--remap-path-prefix=$HOME=~ --remap-path-prefix=$repo=tokenlens"
# Skips the Finder window styling, which needs an Automation permission prompt.
export CI=true

cd "$repo/app"
updater=false
if [ "${TOKENLENS_UPDATER_ARTIFACTS:-0}" = "1" ]; then
  [ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ] || { echo 'Updater artifacts require TAURI_SIGNING_PRIVATE_KEY.' >&2; exit 1; }
  updater=true
fi
# Only the release workflow opts into signing. Local builds need no private key.
override_dir="$(mktemp -d)"
override="$override_dir/updater.json"
trap 'rm -f "$override"; rmdir "$override_dir"' EXIT
printf '{"bundle":{"createUpdaterArtifacts":%s}}\n' "$updater" > "$override"
npm run tauri build -- --bundles app,dmg --config "$override" -- -j 4

bundle="$target/release/bundle"
binary="$bundle/macos/TokenLens.app/Contents/MacOS/tokenlens-app"
leaks="$(strings -a "$binary" | grep -c -F "$HOME" || true)"
if [ "$leaks" != "0" ]; then
  echo "The app binary still contains this machine's home path ($leaks places)." >&2
  exit 1
fi
# A broken ad-hoc signature makes macOS call a downloaded copy "damaged".
codesign --verify --deep --strict "$bundle/macos/TokenLens.app"
hdiutil verify "$bundle"/dmg/*.dmg >/dev/null
if [ "$updater" = true ]; then
  [ -s "$bundle/macos/TokenLens.app.tar.gz" ] && [ -s "$bundle/macos/TokenLens.app.tar.gz.sig" ] || {
    echo 'Mac updater archive or signature is missing.' >&2; exit 1;
  }
fi
du -sh "$bundle/macos/TokenLens.app" "$bundle"/dmg/*.dmg
echo "Bundle ready; no home path inside the app binary."
