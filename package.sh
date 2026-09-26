#!/usr/bin/env bash
# Stages a self-contained rust-ffmpeg-cli bundle, checks that it actually runs
# with a scrubbed library path, and archives it into dist/.
#
# The bundle is what the tarball contains:
#
#   rust-ffmpeg-cli.sh    launcher that sets the library path and execs .bin
#   rust-ffmpeg-cli.bin   the real compiled binary
#   libav*.so* / *.dylib  the FFmpeg shared libraries, from copy-dlls.sh
#
# The staging directory (target/package) is wiped on every run, so packaging
# twice in a row is idempotent. Nothing cargo owns is touched: the release
# binary is *copied* out of target/, never renamed or moved.
#
# Usage:
#   ./package.sh [options]
#
# Options:
#   --binary <path>   compiled release binary
#                     (default: target/<triple>/release/rust-ffmpeg-cli, with
#                      $TARGET honoured the same way cargo honours it)
#   --out-dir <dir>   where the archive is written (default: ./dist)
#   --source <dir>    dir holding the FFmpeg shared libraries
#                     (default: $FFMPEG_DIR/lib, as in copy-dlls.sh)
#   --no-verify       skip the smoke test
#   --verify-only     re-run the smoke test on the existing staged bundle
#   --clean           remove the staging directory and dist/, then exit
#
# `just package` runs this with no arguments; it is equally usable on its own.

set -euo pipefail

ROOT="$(cd -P "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STAGE="$ROOT/target/package"

BINARY=""
OUT_DIR=""
SOURCE=""
VERIFY=1
MODE="package"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --binary)      BINARY="$2"; shift 2 ;;
        --out-dir)     OUT_DIR="$2"; shift 2 ;;
        --source)      SOURCE="$2"; shift 2 ;;
        --no-verify)   VERIFY=0; shift ;;
        --verify-only) MODE="verify"; shift ;;
        --clean)       MODE="clean"; shift ;;
        *)
            echo "unknown option: $1" >&2
            echo "usage: $0 [--binary <path>] [--out-dir <dir>] [--source <dir>] [--no-verify] [--verify-only] [--clean]" >&2
            exit 1
            ;;
    esac
done

# ---- Platform detection -----------------------------------------------------
case "$(uname -s)" in
    Darwin) PLATFORM="macos" ;;
    Linux)  PLATFORM="linux" ;;
    *)
        echo "error: unsupported platform: $(uname -s)" >&2
        exit 1
        ;;
esac

case "$(uname -m)" in
    x86_64|amd64)  ARCH="x64" ;;
    aarch64|arm64) ARCH="arm64" ;;
    *)             ARCH="$(uname -m)" ;;
esac

# ---- The smoke test ---------------------------------------------------------
# Runs the staged launcher with both library-path variables scrubbed, so an
# FFmpeg installed system-wide cannot stand in for a library that is missing
# from the bundle. A pass here means the archive is self-contained.
verify_bundle() {
    local launcher="$STAGE/rust-ffmpeg-cli.sh"
    if [[ ! -x "$launcher" ]]; then
        echo "error: no staged bundle at $STAGE (run 'just package' first)" >&2
        return 1
    fi
    if LD_LIBRARY_PATH= DYLD_LIBRARY_PATH= "$launcher" --version; then
        echo "OK: bundle starts and reports its version (library path scrubbed)"
    else
        echo "error: the staged bundle failed to start" >&2
        return 1
    fi
}

case "$MODE" in
    clean)
        rm -rf "$STAGE" "$ROOT/dist"
        echo "Removed $(basename "$STAGE")/ and dist/"
        exit 0
        ;;
    verify)
        verify_bundle
        exit 0
        ;;
esac

# ---- Resolve the compiled binary --------------------------------------------
if [[ -z "$BINARY" ]]; then
    if [[ -n "${TARGET:-}" ]]; then
        BINARY="$ROOT/target/$TARGET/release/rust-ffmpeg-cli"
    else
        BINARY="$ROOT/target/release/rust-ffmpeg-cli"
    fi
fi
if [[ ! -x "$BINARY" ]]; then
    echo "error: release binary not found: $BINARY (run 'just build' first)" >&2
    exit 1
fi

# ---- Version (Cargo.toml is the single source of truth) ---------------------
VERSION="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$ROOT/Cargo.toml" | head -n 1)"
if [[ -z "$VERSION" ]]; then
    echo "error: could not read a version from $ROOT/Cargo.toml" >&2
    exit 1
fi

# ---- Output directory -------------------------------------------------------
[[ -n "$OUT_DIR" ]] || OUT_DIR="$ROOT/dist"
mkdir -p "$OUT_DIR"
OUT_DIR="$(cd -P "$OUT_DIR" && pwd)"

# ---- Stage ------------------------------------------------------------------
rm -rf "$STAGE"
mkdir -p "$STAGE"

# The launcher looks for `rust-ffmpeg-cli.bin` first, so the staged copy gets
# that name; cargo's own output keeps its name and stays where it is.
cp "$BINARY" "$STAGE/rust-ffmpeg-cli.bin"
cp "$ROOT/rust-ffmpeg-cli.sh" "$STAGE/rust-ffmpeg-cli.sh"
chmod +x "$STAGE/rust-ffmpeg-cli.sh"

# Library copying (and the macOS @loader_path install-name fixups) is already
# implemented in copy-dlls.sh — reuse it rather than duplicating the list.
if [[ -n "$SOURCE" ]]; then
    "$ROOT/copy-dlls.sh" --source "$SOURCE" --target "$STAGE"
else
    "$ROOT/copy-dlls.sh" --target "$STAGE"
fi

if [[ "$VERIFY" == "1" ]]; then
    verify_bundle
fi

# ---- Archive ----------------------------------------------------------------
# Flat: every entry sits at the archive root, so extracting needs no cleanup.
# The glob has to be expanded inside the staging dir, hence the subshell.
ARCHIVE="$OUT_DIR/rust-ffmpeg-cli-$VERSION-$PLATFORM-$ARCH.tar.gz"
rm -f "$ARCHIVE"
( cd "$STAGE" && tar -czf "$ARCHIVE" * )

echo
echo "Packaged $ARCHIVE"
echo "  $(find "$STAGE" -maxdepth 1 \( -type f -o -type l \) | wc -l | tr -d ' ') files, $(du -sh "$STAGE" | cut -f1) staged"
echo "  entry point: ./rust-ffmpeg-cli.sh -i <input>"
