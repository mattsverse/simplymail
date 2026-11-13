#!/bin/bash
# Archives the release binary for one Linux or Windows target into dist/; on Linux, also builds
# .deb and .rpm packages (with nfpm, from mise).
set -euo pipefail
cd "$(dirname "$0")/.."

target="${1:?Usage: $0 <rust target triple>}"
# CI passes the validated version; locally, read it from Cargo.toml.
version="${VERSION:-$(python3 scripts/release.py version)}"
cargo build --release --locked --target "$target" --target-dir "$PWD/target" -p simplymail

name="simplymail-$version-$target"
staging="$PWD/target/package/$name"
rm -rf "$staging"
mkdir -p "$staging" dist
cp README.md LICENSE "$staging/"
case "$target" in
  *-windows-*)
    cp "target/$target/release/simplymail.exe" "$staging/"
    rm -f "dist/$name.zip"
    (cd "$staging/.." && pwsh -NoProfile -Command "Compress-Archive -Path '$name' -DestinationPath '../../dist/$name.zip'")
    echo "Ready: dist/$name.zip"
    ;;
  *)
    cp "target/$target/release/simplymail" "$staging/"
    tar -C "$staging/.." -czf "dist/$name.tar.gz" "$name"
    echo "Ready: dist/$name.tar.gz"
    ;;
esac

case "$target" in
  x86_64-unknown-linux-gnu) arch=amd64 ;;
  aarch64-unknown-linux-gnu) arch=arm64 ;;
  *) exit 0 ;;
esac
# assets/linux/nfpm.yaml reads the binary from here, and VERSION and NFPM_ARCH from the environment.
mkdir -p target/package/linux
cp "target/$target/release/simplymail" target/package/linux/simplymail
for packager in deb rpm; do
  VERSION="$version" NFPM_ARCH="$arch" nfpm pkg -f assets/linux/nfpm.yaml -p "$packager" -t dist/
done
