#!/usr/bin/env bash
# Copies the FFmpeg shared libraries required at runtime next to the release
# binary. Works on macOS and Linux (detects the platform automatically).
#
# Usage:
#   ./copy-dlls.sh [--source <source dir>] [--target <target dir>]
#
# Defaults:
#   source = $FFMPEG_DIR/lib   (requires FFMPEG_DIR to be set)
#   target = ./target/release
#
# Example:
#   FFMPEG_DIR=/path/to/ffmpeg-dev ./copy-dlls.sh
#
# ffmpeg-next is configured with the filter/format/software-resampling/
# software-scaling features only, so libavdevice is NOT linked and its library
# is NOT required.

set -euo pipefail

SOURCE=""
TARGET=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --source) SOURCE="$2"; shift 2 ;;
        --target) TARGET="$2"; shift 2 ;;
        *)
            echo "unknown option: $1" >&2
            exit 1
            ;;
    esac
done

# ---- Resolve source directory (FFmpeg dev package lib/) --------------------
if [[ -z "$SOURCE" ]]; then
    if [[ -n "${FFMPEG_DIR:-}" ]]; then
        SOURCE="${FFMPEG_DIR}/lib"
    else
        echo "error: no source directory. Set FFMPEG_DIR (pointing at the FFmpeg dev package) or pass --source." >&2
        exit 1
    fi
fi
if [[ ! -d "$SOURCE" ]]; then
    echo "error: source directory not found: $SOURCE" >&2
    exit 1
fi

# ---- Resolve target directory ----------------------------------------------
if [[ -z "$TARGET" ]]; then
    TARGET="$(cd "$(dirname "$0")" && pwd)/target/release"
fi
mkdir -p "$TARGET"

# ---- Platform detection -----------------------------------------------------
case "$(uname -s)" in
    Darwin) PLATFORM="darwin" ;;
    Linux)  PLATFORM="linux"  ;;
    *)
        echo "error: unsupported platform: $(uname -s)" >&2
        exit 1
        ;;
esac

# ---- Library names (version-agnostic globs) --------------------------------
LIBS=(avcodec avformat avfilter avutil swresample swscale)

copied=0
for name in "${LIBS[@]}"; do
    case "$PLATFORM" in
        darwin) pattern="lib${name}*.dylib" ;;
        linux)  pattern="lib${name}.so*"   ;;
    esac

    # shellcheck disable=SC2086
    matches=("$SOURCE"/$pattern)
    if [[ ! -e "${matches[0]}" ]]; then
        echo "warning: missing in $SOURCE: $pattern (skipped)"
        continue
    fi

    # Copy everything matching (real files + the version symlinks that the
    # dynamic loader follows at runtime).
    cp -a "$SOURCE"/$pattern "$TARGET"/
    copied=$((copied + 1))
done

# ---- macOS: re-point install names (best effort) ---------------------------
# Homebrew/other installs embed absolute paths in the dylibs. Re-point them at
# @loader_path so the bundle works without the original FFmpeg install.
# Requires Xcode command line tools (install_name_tool/otool).
if [[ "$PLATFORM" == "darwin" ]]; then
    for name in "${LIBS[@]}"; do
        for lib in "$TARGET"/lib"${name}"*.dylib; do
            [[ -e "$lib" && ! -L "$lib" ]] || continue

            for dep in "${LIBS[@]}"; do
                for dep_lib in "$SOURCE"/lib"${dep}"*.dylib; do
                    [[ -e "$dep_lib" ]] || continue
                    abs_dep="$(cd "$SOURCE" && pwd)/$(basename "$dep_lib")"
                    install_name_tool -change "$abs_dep" "@loader_path/$(basename "$dep_lib")" "$lib" 2>/dev/null || true
                done
            done
            install_name_tool -id "@loader_path/$(basename "$lib")" "$lib" 2>/dev/null || true
        done
    done
fi

echo "Copied $copied library set(s) from '$SOURCE' to '$TARGET'"