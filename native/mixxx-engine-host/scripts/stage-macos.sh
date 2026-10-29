#!/usr/bin/env bash
set -euo pipefail
host_root="$(cd "$(dirname "$0")/.." && pwd)"
stage_root="$host_root/stage"
cmake -E make_directory "$stage_root"
staging_dir="$(mktemp -d "$stage_root/.staging.XXXXXX")"
bundle="$staging_dir/PlumdeckMixxxHost.app"
destination="$stage_root/PlumdeckMixxxHost.app"
export PATH="/opt/homebrew/bin:/opt/homebrew/sbin:$PATH"
qt_prefix="$(brew --prefix qtbase)"
cmake -E make_directory "$bundle/Contents/MacOS" "$bundle/Contents/Resources" "$bundle/Contents/PlugIns/platforms"
cmake -E copy_if_different "$host_root/build-upstream/plumdeck-mixxx-engine-host" "$bundle/Contents/MacOS/plumdeck-mixxx-engine-host"
cmake -E copy_if_different "$host_root/packaging/Info.plist" "$bundle/Contents/Info.plist"
# The host creates no window, skin, library or controller subsystem. Effects and
# the settings schema are the only upstream runtime resources it uses (same set
# as build-windows.mjs); skins, translations and controllers are ~100 MiB.
cmake -E copy_directory "$host_root/upstream/res/effects" "$bundle/Contents/Resources/effects"
cmake -E copy_if_different "$host_root/upstream/res/schema.xml" "$bundle/Contents/Resources/schema.xml"
cmake -E copy_if_different "$qt_prefix/share/qt/plugins/platforms/libqoffscreen.dylib" "$bundle/Contents/PlugIns/platforms/libqoffscreen.dylib"
# Static WebRTC components need notices even though no separate dylib is staged.
licenses="$bundle/Contents/Resources/licenses/junction"
cmake -E make_directory "$licenses"
ldc="$host_root/build-deps/libdatachannel"
cmake -E copy_if_different "$host_root/build-deps/soundtouch/source/COPYING.TXT" "$licenses/soundtouch-LGPL-2.1.txt"
cmake -E copy_if_different "$host_root/build-deps/rubberband/source/COPYING" "$licenses/rubberband-GPL-2.0.txt"
cmake -E copy_if_different "$ldc/LICENSE" "$licenses/libdatachannel-MPL-2.0.txt"
cmake -E copy_if_different "$ldc/deps/libsrtp/LICENSE" "$licenses/libsrtp.txt"
cmake -E copy_if_different "$(brew --prefix libnice)/COPYING.MPL" "$licenses/libnice-MPL-1.1.txt"
cmake -E copy_if_different "$(brew --prefix libnice)/COPYING.LGPL" "$licenses/libnice-LGPL-2.1.txt"
cmake -E copy_if_different "$ldc/deps/usrsctp/LICENSE.md" "$licenses/usrsctp.txt"
cmake -E copy_if_different "$ldc/deps/plog/LICENSE" "$licenses/plog.txt"
cmake -E copy_if_different "$ldc/deps/json/LICENSE.MIT" "$licenses/nlohmann-json.txt"
cmake -E copy_if_different "$(brew --prefix opus)/COPYING" "$licenses/opus.txt"
cmake -E copy_if_different "$host_root/dependency-versions.json" "$licenses/dependency-versions.json"
cmake -E copy_if_different "$host_root/upstream/LICENSE" "$bundle/Contents/Resources/licenses/LICENSE-Mixxx"
# This host never uses SQL/UI plugins. Only offscreen is needed; explicitly
# pass it so its complete dependency graph is rewritten and copied as well.
macdeployqt "$bundle" -no-plugins -always-overwrite -verbose=1 \
  -executable="$bundle/Contents/PlugIns/platforms/libqoffscreen.dylib"
node "$host_root/scripts/fix-stage-runtime.mjs" "$bundle"
codesign --force --deep --sign - "$bundle"
codesign --verify --deep --strict "$bundle"
otool -L "$bundle/Contents/MacOS/plumdeck-mixxx-engine-host"
# macOS directory exchange atomically promotes the complete, verified bundle.
# An existing bundle is preserved at the generated staging path, never deleted.
xcrun clang -Wall -Wextra -Werror "$host_root/scripts/promote-stage.c" -o "$staging_dir/promote-stage"
"$staging_dir/promote-stage" "$bundle" "$destination"
cmake -E copy_if_different "$staging_dir/runtime-audit.json" "$stage_root/runtime-audit.json"
echo "Staged executable: $destination/Contents/MacOS/plumdeck-mixxx-engine-host"
echo "Previous bundle (if any) preserved under: $staging_dir"
