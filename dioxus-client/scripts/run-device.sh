#!/usr/bin/env bash
# Build, sign, install, and launch the standalone Dioxus Courtside client.
set -euo pipefail

: "${DEVICE_ID:?Set DEVICE_ID to your iOS device identifier}"
BUNDLE_ID="${BUNDLE_ID:-com.malakindustries.courtside}"
APP_VERSION="${APP_VERSION:-0.1.50}"
: "${SIGN_ID:?Set SIGN_ID to your Apple Development signing identity}"

DIOXUS_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_ROOT="$(cd "$DIOXUS_ROOT/.." && pwd)"
OUT_DIR="$DIOXUS_ROOT/target/device"
LOCAL_PROFILE="$DIOXUS_ROOT/.signing/embedded.mobileprovision"
LEGACY_APP="$REPO_ROOT/target/dd/Build/Products/Release-iphoneos/Courtside.app"
ENTITLEMENTS="$DIOXUS_ROOT/target/device.entitlements"
LAUNCH_SCREEN="$DIOXUS_ROOT/ios/LaunchScreen.storyboard"

export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
export PATH="$HOME/.cargo/bin:$PATH"

profile_expiration_epoch() {
    local candidate="$1"
    local decoded="$2"
    security cms -D -i "$candidate" > "$decoded" 2>/dev/null || return 1
    local application_id
    local expiration
    application_id="$(/usr/libexec/PlistBuddy -c 'Print :Entitlements:application-identifier' "$decoded" 2>/dev/null)"
    expiration="$(/usr/libexec/PlistBuddy -c 'Print :ExpirationDate' "$decoded" 2>/dev/null)"
    [[ "$application_id" == *."$BUNDLE_ID" ]] || return 1
    /usr/libexec/PlistBuddy -c 'Print :ProvisionedDevices' "$decoded" 2>/dev/null \
        | grep -Fq "$DEVICE_ID" || return 1
    date -j -f '%a %b %d %T %Z %Y' "$expiration" '+%s' 2>/dev/null
}

if [ -n "${COURTSIDE_PROVISIONING_PROFILE:-}" ]; then
    PROFILE="$COURTSIDE_PROVISIONING_PROFILE"
else
    PROFILE=""
    best_expiration=0
    profile_check_dir="$(mktemp -d /tmp/courtside-profile-check.XXXXXX)"
    profile_index=0
    while IFS= read -r candidate; do
        [ -f "$candidate" ] || continue
        profile_index=$((profile_index + 1))
        decoded="$profile_check_dir/profile-$profile_index.plist"
        expiration_epoch="$(profile_expiration_epoch "$candidate" "$decoded" || true)"
        if [ -n "$expiration_epoch" ] \
            && [ "$expiration_epoch" -gt "$(date '+%s')" ] \
            && [ "$expiration_epoch" -gt "$best_expiration" ]; then
            PROFILE="$candidate"
            best_expiration="$expiration_epoch"
        fi
    done < <(
        {
            printf '%s\n' "$LOCAL_PROFILE"
            printf '%s\n' "$LEGACY_APP/embedded.mobileprovision"
            find "$HOME/Library/Developer/Xcode/DerivedData" \
                -path '*.app/embedded.mobileprovision' -print 2>/dev/null
            find "$HOME/Library/Developer/Xcode/UserData/Provisioning Profiles" \
                -type f -name '*.mobileprovision' -print 2>/dev/null
            find "$HOME/Library/MobileDevice/Provisioning Profiles" \
                -type f -name '*.mobileprovision' -print 2>/dev/null
        }
    )
fi

if [ -n "${COURTSIDE_SERVER_URL:-}" ]; then
    SERVER_URL="$COURTSIDE_SERVER_URL"
else
    SERVER_URL="$(curl -fsS http://127.0.0.1:4040/api/tunnels \
        | jq -r '[.tunnels[] | select(.proto == "https") | .public_url][0] // empty')"
fi

if [ -z "$SERVER_URL" ]; then
    echo "No HTTPS ngrok tunnel is active. Start the server/tunnel first." >&2
    exit 1
fi

if ! curl -fsS "$SERVER_URL/health" >/dev/null; then
    echo "Courtside server health check failed at $SERVER_URL/health" >&2
    exit 1
fi

if [ ! -f "$PROFILE" ]; then
    echo "No unexpired development profile for $BUNDLE_ID and device $DEVICE_ID was found." >&2
    echo "Sign into Xcode → Settings → Accounts, then run this script again so Xcode can refresh it." >&2
    exit 1
fi

echo "▶ Building Dioxus for iPhone (server: $SERVER_URL)"
mkdir -p "$OUT_DIR"
find "$OUT_DIR" -mindepth 1 -delete
(
    cd "$DIOXUS_ROOT"
    COURTSIDE_SERVER_URL="$SERVER_URL" dx bundle \
        --ios \
        --release \
        --target aarch64-apple-ios \
        --package-types ios \
        --out-dir "$OUT_DIR"
)

APP="$(find "$OUT_DIR" -maxdepth 1 -type d -name '*.app' -print -quit)"
if [ -z "$APP" ]; then
    echo "Dioxus did not produce an app bundle in $OUT_DIR" >&2
    exit 1
fi

echo "▶ Signing Courtside"
cp -f "$PROFILE" "$APP/embedded.mobileprovision"
security cms -D -i "$PROFILE" > "$DIOXUS_ROOT/target/device-profile.plist"
plutil -extract Entitlements xml1 \
    -o "$ENTITLEMENTS" "$DIOXUS_ROOT/target/device-profile.plist"

/usr/libexec/PlistBuddy -c "Set :CFBundleDisplayName Courtside" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleName Courtside" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $BUNDLE_ID" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $APP_VERSION" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $APP_VERSION" "$APP/Info.plist"

# Dioxus generates landscape orientations by default. Replace the general and
# device-specific lists so iOS launches and keeps Courtside in portrait.
for orientation_key in UISupportedInterfaceOrientations \
    'UISupportedInterfaceOrientations~iphone' 'UISupportedInterfaceOrientations~ipad'; do
    /usr/libexec/PlistBuddy -c "Delete :$orientation_key" "$APP/Info.plist" 2>/dev/null || true
    /usr/libexec/PlistBuddy -c "Add :$orientation_key array" "$APP/Info.plist"
    /usr/libexec/PlistBuddy -c "Add :$orientation_key:0 string UIInterfaceOrientationPortrait" "$APP/Info.plist"
done

# Dioxus' generated plist names a LaunchScreen storyboard but does not package
# one. Without it, iOS briefly falls back to a light-gray launch surface before
# the dark webview renders. Compile a native screen matching the app background
# so the system launch animation and first Courtside frame are continuous.
xcrun ibtool --compile "$APP/LaunchScreen.storyboardc" "$LAUNCH_SCREEN" \
    --target-device iphone \
    --target-device ipad \
    --minimum-deployment-target 15.0
/usr/libexec/PlistBuddy -c "Delete :UILaunchStoryboardName" "$APP/Info.plist" 2>/dev/null || true
/usr/libexec/PlistBuddy -c "Add :UILaunchStoryboardName string LaunchScreen" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Delete :UIUserInterfaceStyle" "$APP/Info.plist" 2>/dev/null || true
/usr/libexec/PlistBuddy -c "Add :UIUserInterfaceStyle string Dark" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Delete :NSContactsUsageDescription" "$APP/Info.plist" 2>/dev/null || true
/usr/libexec/PlistBuddy -c "Add :NSContactsUsageDescription string 'Courtside uses contacts you search for to fill call recipient phone numbers.'" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Delete :NSMicrophoneUsageDescription" "$APP/Info.plist" 2>/dev/null || true
/usr/libexec/PlistBuddy -c "Add :NSMicrophoneUsageDescription string 'Courtside uses your microphone when you join a live call.'" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Delete :UIBackgroundModes" "$APP/Info.plist" 2>/dev/null || true
/usr/libexec/PlistBuddy -c "Add :UIBackgroundModes array" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Add :UIBackgroundModes:0 string audio" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Add :UIBackgroundModes:1 string voip" "$APP/Info.plist"
# CallKit requires VoIP background support, including calls started in the app.
plutil -extract UIBackgroundModes json -o - "$APP/Info.plist" \
    | jq -e 'index("audio") != null and index("voip") != null' >/dev/null

# Apps built with the iOS 27 SDK must use UIKit's scene lifecycle. Dioxus
# 0.7.10 still packages the legacy application lifecycle even though tao's
# current iOS runtime supports scenes, so declare its runtime scene delegate
# explicitly. Keep multiple windows disabled: Courtside is a single-window app.
/usr/libexec/PlistBuddy -c "Delete :UIApplicationSceneManifest" "$APP/Info.plist" 2>/dev/null || true
/usr/libexec/PlistBuddy -c "Add :UIApplicationSceneManifest dict" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Add :UIApplicationSceneManifest:UIApplicationSupportsMultipleScenes bool false" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Add :UIApplicationSceneManifest:UISceneConfigurations dict" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Add :UIApplicationSceneManifest:UISceneConfigurations:UIWindowSceneSessionRoleApplication array" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Add :UIApplicationSceneManifest:UISceneConfigurations:UIWindowSceneSessionRoleApplication:0 dict" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Add :UIApplicationSceneManifest:UISceneConfigurations:UIWindowSceneSessionRoleApplication:0:UISceneConfigurationName string 'Default Configuration'" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Add :UIApplicationSceneManifest:UISceneConfigurations:UIWindowSceneSessionRoleApplication:0:UISceneDelegateClassName string TaoSceneDelegate" "$APP/Info.plist"

# Dioxus 0.7's iOS bundler currently does not package `bundle.icon`, so stage
# the full legacy Courtside icon explicitly before signing.
make_icon() {
    local pixels="$1"
    local filename="$2"
    sips -z "$pixels" "$pixels" "$DIOXUS_ROOT/assets/Icon.png" \
        --out "$APP/$filename" >/dev/null
}

make_icon 40 "AppIcon20x20@2x.png"
make_icon 60 "AppIcon20x20@3x.png"
make_icon 58 "AppIcon29x29@2x.png"
make_icon 87 "AppIcon29x29@3x.png"
make_icon 80 "AppIcon40x40@2x.png"
make_icon 120 "AppIcon40x40@3x.png"
make_icon 120 "AppIcon60x60@2x.png"
make_icon 180 "AppIcon60x60@3x.png"
make_icon 152 "AppIcon76x76@2x~ipad.png"
make_icon 167 "AppIcon83.5x83.5@2x~ipad.png"

if [ -d "$APP/Frameworks" ]; then
    for framework in "$APP"/Frameworks/*.framework; do
        [ -d "$framework" ] || continue
        codesign --force --sign "$SIGN_ID" --timestamp=none "$framework"
    done
fi

/usr/libexec/PlistBuddy -c "Delete :CFBundleIcons" "$APP/Info.plist" 2>/dev/null || true
/usr/libexec/PlistBuddy -c "Add :CFBundleIcons dict" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Add :CFBundleIcons:CFBundlePrimaryIcon dict" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Add :CFBundleIcons:CFBundlePrimaryIcon:CFBundleIconFiles array" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Add :CFBundleIcons:CFBundlePrimaryIcon:CFBundleIconFiles:0 string AppIcon20x20" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Add :CFBundleIcons:CFBundlePrimaryIcon:CFBundleIconFiles:1 string AppIcon29x29" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Add :CFBundleIcons:CFBundlePrimaryIcon:CFBundleIconFiles:2 string AppIcon40x40" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Add :CFBundleIcons:CFBundlePrimaryIcon:CFBundleIconFiles:3 string AppIcon60x60" "$APP/Info.plist"

/usr/libexec/PlistBuddy -c "Delete :CFBundleIcons~ipad" "$APP/Info.plist" 2>/dev/null || true
/usr/libexec/PlistBuddy -c "Add :CFBundleIcons~ipad dict" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Add :CFBundleIcons~ipad:CFBundlePrimaryIcon dict" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Add :CFBundleIcons~ipad:CFBundlePrimaryIcon:CFBundleIconFiles array" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Add :CFBundleIcons~ipad:CFBundlePrimaryIcon:CFBundleIconFiles:0 string AppIcon20x20" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Add :CFBundleIcons~ipad:CFBundlePrimaryIcon:CFBundleIconFiles:1 string AppIcon29x29" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Add :CFBundleIcons~ipad:CFBundlePrimaryIcon:CFBundleIconFiles:2 string AppIcon40x40" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Add :CFBundleIcons~ipad:CFBundlePrimaryIcon:CFBundleIconFiles:3 string AppIcon76x76" "$APP/Info.plist"
/usr/libexec/PlistBuddy -c "Add :CFBundleIcons~ipad:CFBundlePrimaryIcon:CFBundleIconFiles:4 string AppIcon83.5x83.5" "$APP/Info.plist"

codesign --force --sign "$SIGN_ID" \
    --entitlements "$ENTITLEMENTS" \
    --timestamp=none "$APP"
codesign --verify --deep --strict "$APP"

if [ "${COURTSIDE_BUILD_ONLY:-0}" = "1" ]; then
    echo "Prepared signed app: $APP"
    exit 0
fi

echo "▶ Installing"
xcrun devicectl device install app --device "$DEVICE_ID" "$APP" \
    | grep -E "App installed|bundleID"

echo "▶ Launching"
LAUNCH_LOG="$DIOXUS_ROOT/target/device-launch.log"
if xcrun devicectl device process launch --terminate-existing --device "$DEVICE_ID" "$BUNDLE_ID" \
    > "$LAUNCH_LOG" 2>&1; then
    tail -1 "$LAUNCH_LOG"
elif grep -q "reason: Locked" "$LAUNCH_LOG"; then
    echo "Installed successfully. Unlock the iPhone and open Courtside."
else
    cat "$LAUNCH_LOG" >&2
    exit 1
fi
