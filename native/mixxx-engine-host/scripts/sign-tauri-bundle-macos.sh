#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../../.." && pwd)"
bundle="${1:-$repo_root/src-tauri/target/release/bundle/macos/plumdeck.app}"
case "$bundle" in
  "$repo_root"/src-tauri/target/aarch64-apple-darwin/release/bundle/macos/plumdeck.app|"$repo_root"/src-tauri/target/x86_64-apple-darwin/release/bundle/macos/plumdeck.app|"$repo_root"/src-tauri/target/debug/bundle/macos/plumdeck.app|"$repo_root"/src-tauri/target/release/bundle/macos/plumdeck.app|"$repo_root"/src-tauri/target/release/bundle/macos/"plumdeck Preview.app") ;;
  *) echo "Refusing to sign an unexpected bundle path: $bundle" >&2; exit 2 ;;
esac

nested="$bundle/Contents/Resources/PlumdeckMixxxHost.app"
staged="$repo_root/native/mixxx-engine-host/stage/PlumdeckMixxxHost.app"
[[ -x "$staged/Contents/MacOS/plumdeck-mixxx-engine-host" ]] || {
  echo "Staged Mixxx host is missing: $staged" >&2
  exit 1
}

# Tauri's generic resource copier dereferences framework symlinks. Install the
# already-audited nested app after bundling with ditto, which preserves the
# framework topology and extended attributes. `nested` is an exact generated
# path beneath the validated plumdeck.app target above.
rm -rf -- "$nested"
/usr/bin/ditto "$staged" "$nested"

# Tauri's resource copier may materialize framework symlinks, invalidating the
# already-sealed nested app. Re-seal the nested app, then its containing app.
# '-' is local ad-hoc signing only; public distribution still requires the
# project's Developer ID + notarization workflow.
identity="${APPLE_SIGNING_IDENTITY:--}"
codesign --force --deep --sign "$identity" "$nested"

if [[ "$identity" == "-" ]]; then
  # Local builds use ad-hoc signing. Pin the app's designated requirement to
  # its bundle identifier so macOS TCC permissions survive code-hash changes
  # between local rebuilds. The nested host is already sealed above; avoid
  # --deep here so its independent signature remains intact.
  codesign --force --sign "$identity" \
    -r='designated => identifier "com.plumdeck.app"' "$bundle"
else
  codesign --force --deep --sign "$identity" "$bundle"
fi
codesign --verify --deep --strict "$nested"
codesign --verify --deep --strict "$bundle"
echo "Verified Performance bundle: $bundle"

if [[ "$bundle" == */release/bundle/macos/plumdeck.app ]]; then
  "$repo_root/native/mixxx-engine-host/scripts/repack-performance-dmg-macos.sh" "$bundle"
fi
