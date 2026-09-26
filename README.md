# rust-ffmpeg-cli

Batch video encoder that transcodes videos **in-process** — it links directly against FFmpeg 8.1's `libav*` libraries via the [`ffmpeg-next`](https://crates.io/crates/ffmpeg-next) bindings and **never shells out** to external `ffmpeg`/`ffprobe` binaries.

On NVIDIA hardware it uses CUDA decode + **NVENC** (HEVC). If no CUDA-capable GPU is detected, it automatically falls back to a **software pipeline** (`libx265` HEVC or `libsvtav1` AV1).

Per input file it:

1. demuxes with `libavformat`,
2. decodes the video stream — **CUDA hardware decode** (NVDEC) on the GPU path, software decode in CPU fallback mode,
3. runs a filter graph — `scale_cuda` on GPU (conversion stays on the GPU), `format` on CPU,
4. encodes — `hevc_nvenc` (`-rc constqp -cq 22 -preset slow`, `main`/`main10`) on GPU, or `libx265` (`crf 22`, `preset slow`) / `libsvtav1` (`crf 32`, `preset 6`) on CPU, and
5. **stream-copies** audio/subtitles (and any extra video streams) without re-encoding.

The encoding profile adapts to the source bit depth:

| Source | GPU format (`scale_cuda`) | CPU format (`format`) | Codec profile |
|--------|---------------------------|-----------------------|---------------|
| 8-bit (`yuv420p`, …) | `nv12` | `yuv420p` | `main` |
| 10-bit (`yuv420p10le`, …) | `p010le` | `yuv420p10le` | `main10` |

## Requirements

### Build

- Rust (edition 2024)
- **FFmpeg 8.1 development files** (headers + import libraries). `ffmpeg-next` is pinned to **8.1.0** and the crate compiles against FFmpeg 8.1's API, so the dev package must match.
  - Example (Windows): the [BtbN](https://github.com/BtbN/FFmpeg-Builds/releases) `ffmpeg-n8.1.3-win64-gpl-shared-8.1` zip, unzipped somewhere, with the path exported as `FFMPEG_DIR` (points at the folder containing `include/`, `lib/`, `bin/`).
- **libclang** for `bindgen` (used by `ffmpeg-sys-next`), exported as `LIBCLANG_PATH` (e.g. the `clang\native` folder inside a Python `clang` package).
- A CUDA-capable NVIDIA GPU with current drivers (used when present; without one the tool falls back to software encoding).
- [`just`](https://github.com/casey/just) — optional, only for the [`just build` / `just package`](#packaging) shortcuts. `cargo build --release` works on its own.

Windows example:

```powershell
$env:FFMPEG_DIR   = 'C:\path\to\ffmpeg-n8.1.3-win64-gpl-shared-8.1'
$env:LIBCLANG_PATH = 'C:\path\to\clang\native'
just build        # or: cargo build --release
```

### Runtime

When using the shared-library build of FFmpeg, the accompanying `bin/` directory (containing `avcodec-62.dll`, `avformat-62.dll`, `avfilter-11.dll`, `avutil-60.dll`, `swresample-6.dll`, `swscale-9.dll`, …) must be on `PATH` or next to the executable, so the DLLs can be loaded at startup.

```powershell
$env:PATH = 'C:\path\to\ffmpeg-n8.1.3-win64-gpl-shared-8.1\bin;' + $env:PATH
```

Or, to bundle the libraries with the executable so it runs without any
environment set up, run:
- Windows: `copy-dlls.ps1` (copies the required DLLs from `$env:FFMPEG_DIR\bin` into `target\release`)
- macOS / Linux: `copy-dlls.sh` (run as `bash copy-dlls.sh`, or `chmod +x` it first) — copies `libavcodec`/`libavformat`/`libavfilter`/`libavutil`/`libswresample`/`libswscale` from `$FFMPEG_DIR\lib`; on macOS it also re-points the dylibs' install names to `@loader_path` so no original install is needed)

Both accept `-SourceDir`/`-TargetDir` (or `--source`/`--target`) to override.

See [Packaging](#packaging) for turning that into a distributable archive.

## Packaging

`just` drives the build and the packaging; [`just`](https://github.com/casey/just)
is the only extra tool required (`cargo install just`, `brew install just`, or
`winget install Casey.Just` on Windows).

| Command | What it does |
|---------|--------------|
| `just` | list the recipes |
| `just build` | `cargo build --release` |
| `just libs` | copy the FFmpeg shared libraries next to the dev binary in `target/` |
| `just package` | build, stage a bundle, verify it runs, archive it into `dist/` |
| `just smoke` | re-verify the bundle that is already staged, rebuild nothing |
| `just fmt` | format the Rust sources (`cargo fmt`) |
| `just clean` | remove the staging directory and `dist/` |

```bash
FFMPEG_DIR=/path/to/ffmpeg-dev just package
# dist/rust-ffmpeg-cli-0.1.0-linux-x64.tar.gz
```

`just package` runs entirely on the host it is invoked on — there is no
cross-compilation, since the bundle has to pick up FFmpeg libraries and (on the
GPU path) a CUDA stack built for that platform. Build a macOS bundle on macOS
and a Windows one on Windows. To pick the target triple explicitly, pass it the
way cargo does:

```bash
TARGET=aarch64-unknown-linux-gnu just package
```

### Bundle layout

The archive is **flat** — every entry sits at its root, so extracting needs no
cleanup:

| Linux / macOS | Windows | What it is |
|---------------|---------|------------|
| `rust-ffmpeg-cli.sh` | — | launcher; sets the library path and execs `.bin` |
| `rust-ffmpeg-cli.bin` | `rust-ffmpeg-cli.exe` | the real compiled binary |
| `libav*.so*` / `*.dylib` | `av*-62.dll`, … | the FFmpeg shared libraries |

On Windows no launcher is needed: the loader always searches the exe's own
directory before `PATH`, so the binary and the DLLs beside it are enough. On
Linux and macOS there is no such default, hence `rust-ffmpeg-cli.sh`, which
exports `LD_LIBRARY_PATH` (Linux) or `DYLD_LIBRARY_PATH` (macOS) for its own
directory before exec'ing the binary. This keeps the compiled binary free of a
baked-in rpath, and the tarball needs no system-wide setup to run:

```bash
tar -xzf rust-ffmpeg-cli-0.1.0-linux-x64.tar.gz
./rust-ffmpeg-cli.sh -i video.mp4
```

The same launcher also works in the development tree, where the binary is
still called `rust-ffmpeg-cli` (it falls back to that name), so no renaming is
needed:

```bash
just libs
./target/release/rust-ffmpeg-cli.sh -i video.mp4
```

### Verification

`just package` refuses to produce an archive until the staged bundle has
actually started, with both library-path variables scrubbed so that an FFmpeg
installed system-wide cannot stand in for a library missing from the bundle:

```
$ just package
cargo build --release
    Finished `release` profile [optimized] target(s) in 0.09s
bash package.sh
Copied 6 library set(s) from '/path/to/ffmpeg/lib' to '.../target/package'
rust-ffmpeg-cli 0.1.0
OK: bundle starts and reports its version (library path scrubbed)

Packaged .../dist/rust-ffmpeg-cli-0.1.0-linux-x64.tar.gz
  20 files, 199M staged
  entry point: ./rust-ffmpeg-cli.sh -i <input>
```

Note that the archives are not byte-reproducible: `tar` and `gzip` record file
modification times, which differ per run. The *contents* are identical.

`package.sh` / `package.ps1` can also be run directly, without `just`:

```bash
bash package.sh --source /path/to/ffmpeg/lib --out-dir /tmp/out
pwsh ./package.ps1 -SourceDir C:\path\to\ffmpeg\bin
```

## Usage

```
rust-ffmpeg-cli [OPTIONS] --inputs <INPUTS>...
```

### Options

| Flag | Long | Default | Description |
|------|------|---------|-------------|
| `-i` | `--inputs` | *(required)* | Input video file paths or folder paths |
| `-o` | `--output-dir` | `output/<parent_name>/` | Base destination directory for output files |
| `-e` | `--extension` | `mkv` | Custom output file extension (e.g., `mkv`, `mp4`) |
| `-s` | `--suffix` | `_enc` | Custom suffix appended to the input file name |
| `--mode` | `--mode` | `auto` | Encoding mode: `auto` (probe for NVIDIA/CUDA), `cuda` (force GPU), `cpu` (force software) |
| `--cpu-codec` | `--cpu-codec` | `hevc` | Software encoder used in CPU mode: `hevc` (libx265) or `av1` (libsvtav1) |
| `-h` | `--help` | | Print help |
| `-V` | `--version` | | Print version |

### Examples

Encode a single file (output goes to `output/` with a subdirectory named after the parent):

```bash
# input:  videos/video.mp4
# output: output/videos/video_enc.mkv
rust-ffmpeg-cli -i video.mp4
```

Encode multiple files:

```bash
rust-ffmpeg-cli -i video1.mp4 video2.mkv video3.mov
```

Encode all videos in a folder (recursively scanned):

```bash
rust-ffmpeg-cli -i /path/to/folder/
```

Specify output directory and extension:

```bash
rust-ffmpeg-cli -i video.mp4 -o /output/ -e mp4
# output: /output/videos/video_enc.mp4
```

Custom suffix:

```bash
rust-ffmpeg-cli -i video.mp4 -s "_h265"
# output: video_h265.mkv
```

### Output Naming

Output files follow the pattern:

```
{base_dir}/{parent_dir_name}/{original_stem}{suffix}.{extension}
```

Where `{base_dir}` defaults to `output` (relative to the current working directory) unless overridden with `-o`. For example, with defaults: `movies/movie.mp4` becomes `output/movies/movie_enc.mkv`.

## How it works

The GPU parts of the chain are not exposed by the safe `ffmpeg-next` API, so the code mixes the safe API (demuxing, decoding loop, encoding, muxing, stream copy) with targeted raw FFI through `ffmpeg::ffi`:

- `av_hwdevice_ctx_create` creates the CUDA device; the decoder gets `hw_device_ctx` plus a `get_format` callback so its frames are allocated in GPU memory.
- The filter graph (`buffer → scale_cuda → buffersink`) is built only once the first frame is decoded, because that is when the decoder creates its `hw_frames_ctx`. The `buffer` source is configured with `av_buffersrc_parameters_set` (carrying the decoder's `hw_frames_ctx` + `format=cuda`) **before** `avfilter_init_dict`, then the graph is linked and configured.
- The `hevc_nvenc` encoder is fed with the filter sink's hardware frames context and opened with `preset=slow`, `rc=constqp`, `cq=22`, and `profile=main`/`main10`.
- The output header is written on the first decoded frame; audio/subtitle packets that arrive before that are buffered and replayed afterwards.

### CPU fallback (no NVIDIA GPU)

With `--mode auto` (the default), a CUDA device probe (`av_hwdevice_ctx_create`) at startup decides between the GPU and CPU pipelines **for the whole batch**. `--mode cuda`/`--mode cpu` force either path.

The CPU pipeline reuses the same carrier logic (output-stream reservation, header replay, stream copy, flush) and only swaps the core:

- a plain software decoder (no `hw_device_ctx`, no `get_format` callback),
- a `buffer → format → buffersink` graph — the *actual* pixel format emitted by the decoder is passed to the `buffer` source, and the `format` filter converts to `yuv420p`/`yuv420p10le`,
- `libx265` (`preset=slow`, `crf=22`, `profile=main`/`main10`) or `libsvtav1` (`preset=6`, `crf=32`) as the encoder, selected with `--cpu-codec`.