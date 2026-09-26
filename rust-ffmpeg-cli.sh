#!/usr/bin/env bash
# Launcher for rust-ffmpeg-cli: makes the bundled FFmpeg shared libraries
# (copied in next to this script) discoverable at runtime, without needing an
# rpath baked into the binary or the system library path touched.
#
# Two layouts work:
#
#   Release bundle - `just package` (package.sh) stages the real binary as
#   `rust-ffmpeg-cli.bin` next to this script along with the shared libraries
#   and archives the lot:
#       ./rust-ffmpeg-cli.sh -i video.mp4
#
#   Development tree - run it from target/<triple>/release, where copy-dlls.sh
#   puts the libraries. The binary cargo built is still called
#   `rust-ffmpeg-cli`, so this script falls back to that name and no manual
#   renaming is needed:
#       cp rust-ffmpeg-cli.sh target/release/
#       ./target/release/rust-ffmpeg-cli.sh -i video.mp4
#
# Linux gets LD_LIBRARY_PATH, macOS gets DYLD_LIBRARY_PATH. On macOS
# copy-dlls.sh has already rewritten the dylibs' references to each other as
# @loader_path, so only the executable -> dylib edges need help here.

set -euo pipefail

# Resolve the directory this script lives in, following symlinks, so it
# works no matter where the tarball is extracted or how it's invoked.
SOURCE="${BASH_SOURCE[0]}"
while [[ -L "$SOURCE" ]]; do
    DIR="$(cd -P "$(dirname "$SOURCE")" >/dev/null 2>&1 && pwd)"
    SOURCE="$(readlink "$SOURCE")"
    [[ "$SOURCE" != /* ]] && SOURCE="$DIR/$SOURCE"
done
SCRIPT_DIR="$(cd -P "$(dirname "$SOURCE")" >/dev/null 2>&1 && pwd)"

BINARY="$SCRIPT_DIR/rust-ffmpeg-cli.bin"
if [[ ! -x "$BINARY" ]]; then
    # Development layout: cargo's binary carries no .bin suffix.
    BINARY="$SCRIPT_DIR/rust-ffmpeg-cli"
fi
if [[ ! -x "$BINARY" ]]; then
    echo "error: no rust-ffmpeg-cli binary next to this script ($SCRIPT_DIR)" >&2
    echo "       run 'just package' to build a bundle, or 'just libs' to populate target/" >&2
    exit 1
fi

if [[ "$(uname -s)" == "Darwin" ]]; then
    export DYLD_LIBRARY_PATH="$SCRIPT_DIR${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}"
else
    export LD_LIBRARY_PATH="$SCRIPT_DIR${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi

exec "$BINARY" "$@"
