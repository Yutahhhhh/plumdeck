#!/usr/bin/env bash
set -euo pipefail
host_root="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="/opt/homebrew/bin:/opt/homebrew/sbin:$PATH"
dep_root="$host_root/build-deps/soundtouch"
mkdir -p "$dep_root"
if [[ ! -f "$dep_root/source.tar.gz" ]]; then
  curl --fail --location --proto '=https' --tlsv1.2 https://codeberg.org/soundtouch/soundtouch/archive/2.4.1.tar.gz -o "$dep_root/source.tar.gz"
fi
python3 "$host_root/scripts/prepare-soundtouch.py" "$dep_root"
cmake -S "$dep_root/source" -B "$dep_root/build" -G Ninja \
 -DCMAKE_BUILD_TYPE=Release -DCMAKE_OSX_ARCHITECTURES=arm64 -DCMAKE_POLICY_VERSION_MINIMUM=3.5 \
 -DBUILD_SHARED_LIBS=OFF -DSOUNDSTRETCH=OFF -DSOUNDTOUCH_DLL=OFF -DOPENMP=OFF -DNEON=OFF
cmake --build "$dep_root/build" --parallel "${PLUMDECK_BUILD_JOBS:-4}"
