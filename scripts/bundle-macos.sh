#!/bin/bash
# Builds SimplyMail.app (desktop) into dist/.
set -euo pipefail
cd "$(dirname "$0")/.."

[[ "$(uname -s)" == Darwin ]] || { echo 'Bundling the macOS app needs macOS.' >&2; exit 1; }

# By default this builds for the current Mac. `--universal` builds for both Apple
# Silicon and Intel Macs, as releases ship.
case "${1:-}" in
  '') targets=("$(rustc -vV | sed -n 's/^host: //p')") ;;
  --universal) targets=(aarch64-apple-darwin x86_64-apple-darwin) ;;
  *) echo "Usage: $0 [--universal]" >&2; exit 2 ;;
esac

# Keep artifact paths deterministic even with a user-configured target directory.
export MACOSX_DEPLOYMENT_TARGET=13.0
desktop=()
for target in "${targets[@]}"; do
  cargo build --release --locked --target "$target" --target-dir "$PWD/target" -p simplymail
  desktop+=("target/$target/release/simplymail")
done

bundle="$PWD/dist/SimplyMail.app"
rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
lipo -create "${desktop[@]}" -output "$bundle/Contents/MacOS/simplymail"
python3 scripts/release.py plist --output "$bundle/Contents/Info.plist"
cp LICENSE "$bundle/Contents/Resources/"
codesign --force --sign - "$bundle"
echo "Ready: $bundle"
