# rust-ffmpeg-cli

Batch video encoder for NVIDIA GPUs that transcodes videos to HEVC (H.265) using **NVENC**, fully **in-process** — it links directly against FFmpeg 8.1's `libav*` libraries via the [`ffmpeg-next`](https://crates.io/crates/ffmpeg-next) bindings and **never shells out** to external `ffmpeg`/`ffprobe` binaries.

Per input file it:

1. demuxes with `libavformat`,
2. decodes the video stream with **CUDA hardware acceleration** (NVDEC),
3. runs a `scale_cuda` filter graph so the format conversion stays on the GPU,
4. encodes with `hevc_nvenc` (`-rc constqp -cq 22 -preset slow`, `main`/`main10`), and
5. **stream-copies** audio/subtitles (and any extra video streams) without re-encoding.

The encoding profile adapts to the source bit depth:

| Source | `scale_cuda` format | NVENC profile |
|--------|---------------------|---------------|
| 8-bit (`yuv420p`, …)  | `nv12`   | `main`   |
| 10-bit (`yuv420p10le`, …) | `p010le` | `main10` |

## Requirements

### Build

- Rust (edition 2024)
- **FFmpeg 8.1 development files** (headers + import libraries). `ffmpeg-next` is pinned to **8.1.0** and the crate compiles against FFmpeg 8.1's API, so the dev package must match.
  - Example (Windows): the [BtbN](https://github.com/BtbN/FFmpeg-Builds/releases) `ffmpeg-n8.1.3-win64-gpl-shared-8.1` zip, unzipped somewhere, with the path exported as `FFMPEG_DIR` (points at the folder containing `include/`, `lib/`, `bin/`).
- **libclang** for `bindgen` (used by `ffmpeg-sys-next`), exported as `LIBCLANG_PATH` (e.g. the `clang\native` folder inside a Python `clang` package).
- A CUDA-capable NVIDIA GPU with current drivers + NVENC.

Windows example:

```powershell
$env:FFMPEG_DIR   = 'C:\path\to\ffmpeg-n8.1.3-win64-gpl-shared-8.1'
$env:LIBCLANG_PATH = 'C:\path\to\clang\native'
cargo build --release
```

### Runtime

When using the shared-library build of FFmpeg, the accompanying `bin/` directory (containing `avcodec-62.dll`, `avformat-62.dll`, `avfilter-11.dll`, `avutil-60.dll`, `swresample-6.dll`, `swscale-9.dll`, …) must be on `PATH` or next to the executable, so the DLLs can be loaded at startup.

```powershell
$env:PATH = 'C:\path\to\ffmpeg-n8.1.3-win64-gpl-shared-8.1\bin;' + $env:PATH
```

Or, to bundle the libraries with the executable, run:
- Windows: `copy-dlls.ps1` (copies the required DLLs from `$env:FFMPEG_DIR\bin` into `target\release`)
- macOS / Linux: `copy-dlls.sh` (run as `bash copy-dlls.sh`, or `chmod +x` it first) — copies `libavcodec`/`libavformat`/`libavfilter`/`libavutil`/`libswresample`/`libswscale` from `$FFMPEG_DIR\lib`; on macOS it also re-points the dylibs' install names to `@loader_path` so no original install is needed)

Both accept `-SourceDir`/`-TargetDir` (or `--source`/`--target`) to override.

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