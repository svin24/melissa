#!/bin/sh
# Build whisper.cpp's CLI for the image (Containerfile, stage `whisper`; version and hash in whisper.lock).
#
#   whisper-build.sh CACHE OUT LICENSES
#
# CACHE is a build cache mount. A build is kept there under a key made of whisper.lock and this script, so
# the same pinned source and recipe is compiled once (about 3 minutes) and reused by later builds, locally
# and in CI (.github/workflows/container.yml keeps the cache mounts between runs). Any change to either
# file is a new key, so it builds again; other keys are removed.
#
# A separate program, not linked into melissa-server: the server stays lean and C++-free, and a crash or
# the model's memory ends with each run. CPU only; GGML_CPU_ALL_VARIANTS builds the x86-64 variants
# (SSE4.2 ... AVX-512) and the best one for the host's CPU is loaded at runtime, so the image runs on any
# x86-64 box. No SDL, ffmpeg, curl or server: Melissa sends it plain 16 kHz WAV files.
set -eu
cache=$1 out=$2 licenses=$3
here=$(dirname "$0")
key=$(cat "$here/whisper.lock" "$0" | sha256sum | cut -c1-16)
# shellcheck source=packaging/container/whisper.lock
. "$here/whisper.lock"
kept="$cache/$key"

if [ ! -f "$kept/done" ]; then
    echo "whisper.cpp $WHISPER_VERSION: building (cache key $key)"
    dnf -y install --setopt=install_weak_deps=False cmake gcc-c++ make
    dnf clean all
    src=$(mktemp -d)
    cd "$src"
    curl -fsSL --retry 3 -o whisper.tar.gz \
        "https://github.com/ggml-org/whisper.cpp/archive/refs/tags/v${WHISPER_VERSION}.tar.gz"
    printf '%s  whisper.tar.gz\n' "$WHISPER_SHA256" | sha256sum -c -
    tar xzf whisper.tar.gz --strip-components=1
    # shellcheck disable=SC2016 # $ORIGIN is for the dynamic linker, not the shell.
    cmake -B build -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=ON \
        -DGGML_NATIVE=OFF -DGGML_BACKEND_DL=ON -DGGML_CPU_ALL_VARIANTS=ON -DGGML_OPENMP=OFF \
        -DWHISPER_BUILD_TESTS=OFF -DWHISPER_BUILD_SERVER=OFF -DWHISPER_SDL2=OFF \
        -DWHISPER_FFMPEG=OFF -DWHISPER_CURL=OFF \
        -DCMAKE_BUILD_WITH_INSTALL_RPATH=ON -DCMAKE_INSTALL_RPATH='$ORIGIN'
    cmake --build build -j"$(nproc)"
    rm -rf "$kept"
    mkdir -p "$kept/out" "$kept/licenses"
    install -m 0755 build/bin/whisper-cli "$kept/out/"
    find build \( -name 'libwhisper.so*' -o -name 'libggml.so*' -o -name 'libggml-base.so*' \
        -o -name 'libggml-cpu-*.so' \) -exec cp -P {} "$kept/out/" \;
    install -m 0644 LICENSE "$kept/licenses/LICENSE"
    cd /
    rm -rf "$src"
    touch "$kept/done"
else
    echo "whisper.cpp $WHISPER_VERSION: from the build cache (key $key)"
fi
# Only the current key stays in the cache.
find "$cache" -mindepth 1 -maxdepth 1 ! -name "$key" -exec rm -rf {} +

mkdir -p "$out" "$licenses"
cp -a "$kept/out/." "$out/"
cp -a "$kept/licenses/." "$licenses/"
"$out/whisper-cli" --help >/dev/null 2>&1
