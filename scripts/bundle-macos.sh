#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

PROFILE="release"
SKIP_BUILD=false
MAKE_ZIP=false
OFFLINE=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --debug)
      PROFILE="debug"
      shift
      ;;
    --release)
      PROFILE="release"
      shift
      ;;
    --skip-build)
      SKIP_BUILD=true
      shift
      ;;
    --zip)
      MAKE_ZIP=true
      shift
      ;;
    --offline)
      OFFLINE=true
      shift
      ;;
    -h|--help)
      cat <<'EOF'
Usage: scripts/bundle-macos.sh [options]

Options:
  --release      Build release binary (default)
  --debug        Build debug binary
  --skip-build   Skip cargo build and use existing target/<profile>/tent
  --zip          Also package dist/TenT-v<version>-macos-<arch>.zip + .sha256
  --offline      Pass --offline to cargo build
  -h, --help     Show this help
EOF
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      exit 1
      ;;
  esac
done

VERSION="$(sed -nE 's/^version = "([^"]+)"/\1/p' Cargo.toml | head -n 1)"
if [[ -z "$VERSION" ]]; then
  echo "Failed to read version from Cargo.toml" >&2
  exit 1
fi

if [[ "$SKIP_BUILD" == false ]]; then
  BUILD_ARGS=(build --locked)
  if [[ "$PROFILE" == "release" ]]; then
    BUILD_ARGS+=(--release)
  fi
  if [[ "$OFFLINE" == true ]]; then
    BUILD_ARGS+=(--offline)
  fi
  cargo "${BUILD_ARGS[@]}"
fi

BIN_PATH="$ROOT_DIR/target/$PROFILE/tent"
if [[ ! -x "$BIN_PATH" ]]; then
  echo "Binary not found: $BIN_PATH" >&2
  exit 1
fi

ICON_PATH="$ROOT_DIR/assets/TenT.icns"
if [[ ! -f "$ICON_PATH" ]]; then
  echo "Icon not found: $ICON_PATH" >&2
  exit 1
fi

DIST_DIR="$ROOT_DIR/dist"
APP_DIR="$DIST_DIR/TenT.app"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"

cp "$BIN_PATH" "$APP_DIR/Contents/MacOS/tent"
chmod +x "$APP_DIR/Contents/MacOS/tent"
cp "$ICON_PATH" "$APP_DIR/Contents/Resources/TenT.icns"

cat > "$APP_DIR/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>ja</string>
  <key>CFBundleDisplayName</key>
  <string>TenT</string>
  <key>CFBundleExecutable</key>
  <string>tent</string>
  <key>CFBundleIconFile</key>
  <string>TenT</string>
  <key>CFBundleIdentifier</key>
  <string>com.tappoygk.tent</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>TenT</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>${VERSION}</string>
  <key>CFBundleVersion</key>
  <string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key>
  <string>13.0</string>
  <key>NSAppTransportSecurity</key>
  <dict>
    <key>NSAllowsArbitraryLoadsInWebContent</key>
    <true/>
  </dict>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key>
  <true/>
</dict>
</plist>
EOF

# Ad-hoc sign so macOS local launch, Dock, and TCC permissions work cleanly
codesign --force --deep --sign - "$APP_DIR"
echo "Created $APP_DIR (v${VERSION})"

if [[ "$MAKE_ZIP" == true ]]; then
  ARCH="$(uname -m)"
  ZIP_NAME="TenT-v${VERSION}-macos-${ARCH}.zip"
  ZIP_PATH="$DIST_DIR/$ZIP_NAME"
  rm -f "$ZIP_PATH" "${ZIP_PATH}.sha256"
  ditto -c -k --sequesterRsrc --keepParent "$APP_DIR" "$ZIP_PATH"
  (cd "$DIST_DIR" && shasum -a 256 "$ZIP_NAME" > "${ZIP_NAME}.sha256")
  echo "Packaged $ZIP_PATH"
fi
