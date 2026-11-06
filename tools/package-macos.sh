#!/bin/bash
# Build a single-executable app bundle and a DMG using only macOS/Xcode tools.
set -euo pipefail

if [[ "$(uname -s)" != Darwin || "$(uname -m)" != arm64 ]]; then
    printf 'This script requires macOS on Apple Silicon (run without Rosetta).\n' >&2
    exit 1
fi

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
target=aarch64-apple-darwin
# MLProgram requires Core ML 5 (macOS 12); use the same minimum for the app.
export MACOSX_DEPLOYMENT_TARGET=12.0
cargo build --locked --release -p stem-split --target "$target"
package_id="$(cargo pkgid -p stem-split)"
version="${package_id##*@}"
name="stem-split-$version-macos-arm64"
mkdir -p "$root/dist"
work="$(mktemp -d "$root/dist/.macos-package.XXXXXX")"
trap 'rm -rf "$work"' EXIT

stage="$work/image"
app="$stage/stem-split.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "${CARGO_TARGET_DIR:-$root/target}/$target/release/stem-split" "$app/Contents/MacOS/stem-split"
chmod 755 "$app/Contents/MacOS/stem-split"

# Reject accidental dependencies on Homebrew or build-machine libraries.
while IFS= read -r dependency; do
    case "$dependency" in
        /System/Library/*|/usr/lib/*) ;;
        *)
            printf 'Non-system dependency cannot be shipped alone: %s\n' "$dependency" >&2
            exit 1
            ;;
    esac
done < <(otool -L "$app/Contents/MacOS/stem-split" | awk 'NR > 1 { print $1 }')

iconset="$work/AppIcon.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$root/crates/app/assets/icon-preview.png" \
        --out "$iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" "$root/crates/app/assets/icon-preview.png" \
        --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"

cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>stem-split</string>
    <key>CFBundleDisplayName</key><string>stem-split</string>
    <key>CFBundleIdentifier</key><string>io.github.bromzh.stem-split</string>
    <key>CFBundleExecutable</key><string>stem-split</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF
plutil -lint "$app/Contents/Info.plist"
# Ad-hoc signing makes the bundle internally consistent, but is not an Apple
# Developer ID signature and does not satisfy Gatekeeper on downloaded apps.
codesign --force --sign - "$app"
codesign --verify --deep --strict "$app"
ln -s /Applications "$stage/Applications"
sed "s/@VERSION@/$version/g" "$root/tools/release-readme-macos.txt" > "$stage/README.txt"
cp "$root/LICENSE" "$stage/LICENSE.txt"
hdiutil create -volname "stem-split $version" -srcfolder "$stage" \
    -format UDZO "$work/$name.dmg"
hdiutil verify "$work/$name.dmg"
mv -f "$work/$name.dmg" "$root/dist/$name.dmg"
(
    cd "$root/dist"
    shasum -a 256 "$name.dmg" > "$name.dmg.sha256"
)
printf 'DMG: %s\nSHA-256: %s\n' "$root/dist/$name.dmg" "$root/dist/$name.dmg.sha256"
