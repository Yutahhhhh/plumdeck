#!/usr/bin/env bash
set -euo pipefail
host_root="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="/opt/homebrew/bin:/opt/homebrew/sbin:$PATH"
[[ "$(uname -s)" == Darwin && "$(uname -m)" == arm64 ]] || { echo 'Junction release dependencies require macOS arm64.' >&2; exit 1; }
[[ "$(brew --prefix)" == /opt/homebrew ]] || { echo 'Use ARM64 Homebrew at /opt/homebrew.' >&2; exit 1; }
revision=20bf658a60881854984f8ffb1586a4722bf590ec
source_dir="$host_root/build-deps/libdatachannel"
if [[ ! -d "$source_dir/.git" ]]; then
  git init "$source_dir"
  git -C "$source_dir" remote add origin https://github.com/paullouisageneau/libdatachannel.git
  git -C "$source_dir" fetch --depth 1 origin "$revision"
  git -C "$source_dir" switch --detach FETCH_HEAD
fi
[[ "$(git -C "$source_dir" rev-parse HEAD)" == "$revision" ]] || { echo 'libdatachannel revision mismatch; preserve and inspect the existing checkout.' >&2; exit 1; }
[[ -z "$(git -C "$source_dir" status --porcelain -- . ':(exclude)build-static')" ]] || { echo 'Modified libdatachannel checkout.' >&2; exit 1; }
git -C "$source_dir" submodule update --init --recursive --depth 1
# The parent commit pins all five submodule commits, including transitive deps.
if git -C "$source_dir" submodule status --recursive | /usr/bin/grep -Eq '^[+U-]'; then
  echo 'libdatachannel submodule revision mismatch.' >&2; exit 1
fi
git -C "$source_dir" submodule foreach --quiet --recursive 'test -z "$(git status --porcelain)"'
[[ "$(brew list --versions opus)" == 'opus 1.6.1' ]] || { echo 'The recorded Junction codec is opus 1.6.1; install that dependency before release.' >&2; exit 1; }
[[ "$(brew list --versions libnice)" == 'libnice 0.1.23' ]] || { echo 'Junction requires libnice 0.1.23 for TURN TCP/TLS fallback.' >&2; exit 1; }
cmake -S "$source_dir" -B "$source_dir/build-static" -G Ninja \
  -DCMAKE_BUILD_TYPE=Release -DCMAKE_POLICY_VERSION_MINIMUM=3.5 -DCMAKE_OSX_ARCHITECTURES=arm64 \
  -U '*OPENSSL*' -DCMAKE_IGNORE_PREFIX_PATH=/usr/local \
  -DCMAKE_PREFIX_PATH=/opt/homebrew -DPKG_CONFIG_EXECUTABLE=/opt/homebrew/bin/pkg-config \
  -DOPENSSL_ROOT_DIR=/opt/homebrew/opt/openssl@3 \
  -DOPENSSL_INCLUDE_DIR=/opt/homebrew/opt/openssl@3/include \
  -DOPENSSL_CRYPTO_LIBRARY=/opt/homebrew/opt/openssl@3/lib/libcrypto.dylib \
  -DOPENSSL_SSL_LIBRARY=/opt/homebrew/opt/openssl@3/lib/libssl.dylib \
  -DBUILD_SHARED_LIBS=OFF -DBUILD_SHARED_DEPS_LIBS=OFF \
  -DPREFER_SYSTEM_LIB=OFF -DUSE_SYSTEM_SRTP=OFF -DUSE_NICE=ON \
  -DUSE_SYSTEM_USRSCTP=OFF -DUSE_SYSTEM_PLOG=OFF -DUSE_SYSTEM_JSON=OFF \
  -DNO_MEDIA=OFF -DNO_WEBSOCKET=OFF -DNO_EXAMPLES=ON -DNO_TESTS=ON
cmake --build "$source_dir/build-static" --parallel "${PLUMDECK_BUILD_JOBS:-6}"
for library in libdatachannel.a deps/libsrtp/libsrtp2.a deps/usrsctp/usrsctplib/libusrsctp.a; do
  test -f "$source_dir/build-static/$library"
done

bash "$host_root/scripts/build-soundtouch.sh"

bash "$host_root/scripts/build-rubberband.sh"
