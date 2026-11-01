#!/usr/bin/env bash
# Builds target/release/MonitorInputSwitcher.app (Apple Silicon): the release binary, an
# Info.plist that keeps the app out of the Dock (LSUIElement), the icon, and an ad-hoc signature.
#   ./scripts/build-macos.sh
# Then drag the app to /Applications (or `cp -R`) and open it.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release

app=target/release/MonitorInputSwitcher.app
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/monitor-input-switcher "$app/Contents/MacOS/"

# Icon: sips and iconutil ship with macOS.
iconset="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256; do
    sips -z "$size" "$size" assets/icon-256.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    if [ "$double" -le 256 ]; then
        sips -z "$double" "$double" assets/icon-256.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
    fi
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"
rm -rf "$(dirname "$iconset")"

cat >"$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>Monitor Input Switcher</string>
    <key>CFBundleDisplayName</key>
    <string>Monitor Input Switcher</string>
    <key>CFBundleIdentifier</key>
    <string>io.github.a-elkaim.monitor-input-switcher</string>
    <key>CFBundleExecutable</key>
    <string>monitor-input-switcher</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${version}</string>
    <key>CFBundleVersion</key>
    <string>${version}</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>LSUIElement</key>
    <true/>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
EOF

# Apple Silicon only runs signed code; an ad-hoc signature is enough on the machine that built it.
codesign --force --sign - "$app"
echo "Built $app"
