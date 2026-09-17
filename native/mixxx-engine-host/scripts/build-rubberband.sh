#!/usr/bin/env bash
set -euo pipefail
host_root="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="/opt/homebrew/bin:/opt/homebrew/sbin:$PATH"
dep_root="$host_root/build-deps/rubberband"
mkdir -p "$dep_root/build-config"
if [[ ! -f "$dep_root/source.tar.bz2" ]]; then
 curl --fail --location --proto '=https' --tlsv1.2 https://breakfastquay.com/files/releases/rubberband-4.0.0.tar.bz2 -o "$dep_root/source.tar.bz2"
fi
python3 "$host_root/scripts/prepare-rubberband.py" "$dep_root"
cp "$host_root/cmake/rubberband-source.cmake" "$dep_root/build-config/CMakeLists.txt"
cmake -S "$dep_root/build-config" -B "$dep_root/build" -G Ninja -DCMAKE_BUILD_TYPE=Release -DCMAKE_OSX_ARCHITECTURES=arm64 \
 -DRB_SOURCE="$dep_root/source"
cmake --build "$dep_root/build" --parallel "${PLUMDECK_BUILD_JOBS:-4}"
