# rust-ffmpeg-cli

Batch video encoder CLI tool that wraps FFmpeg for hardware-accelerated HEVC encoding using NVIDIA NVENC.

## Requirements

- [FFmpeg](https://ffmpeg.org/) installed and available on `$PATH`
- NVIDIA GPU with NVENC support and proprietary driver
- CUDA toolkit

## Installation

```bash
cargo install --path .
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
| `-h` | `--help` | | Print help |
| `-V` | `--version` | | Print version |

### Examples

Encode a single file (output goes to `output/` with a subdirectory named after the parent):

```bash
# input:  /home/user/videos/video.mp4
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

## Encoding Profile

The tool uses a fixed encoding profile:

- **Decoder:** CUDA hardware acceleration (`-hwaccel cuda`)
- **Pixel format:** P010LE 10-bit (`scale_cuda=format=p010le`)
- **Codec:** H.265/HEVC via NVENC (`hevc_nvenc`)
- **Profile:** Main 10-bit
- **Preset:** Slow
- **Rate control:** Constant QP at 22
- **Audio:** Copied without re-encoding

To change encoding settings, modify the FFmpeg arguments in `src/main.rs`.
