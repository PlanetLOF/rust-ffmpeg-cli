# Copies the FFmpeg DLLs required at runtime next to the release binary.
#
# Usage:
#   .\copy-dlls.ps1 [-SourceDir <ffmpeg bin dir>] [-TargetDir <target dir>]
#
# Defaults:
#   SourceDir = $env:FFMPEG_DIR\bin   (requires FFMPEG_DIR to be set)
#   TargetDir = .\target\release
#
# Example:
#   $env:FFMPEG_DIR = 'C:\path\to\ffmpeg-n8.1.3-win64-gpl-shared-8.1'
#   .\copy-dlls.ps1

param(
    [string]$SourceDir,
    [string]$TargetDir
)

$ErrorActionPreference = 'Stop'

# ---- Resolve source directory (FFmpeg bin/) --------------------------------
if (-not $SourceDir) {
    if ($env:FFMPEG_DIR) {
        $SourceDir = Join-Path $env:FFMPEG_DIR 'bin'
    } else {
        Write-Error "No source directory. Set FFMPEG_DIR (pointing at the FFmpeg dev package) or pass -SourceDir."
        exit 1
    }
}
if (-not (Test-Path $SourceDir)) {
    Write-Error "FFmpeg bin directory not found: $SourceDir`nPass -SourceDir or set FFMPEG_DIR."
    exit 1
}

# ---- Resolve target directory ------------------------------------------------
if (-not $TargetDir) {
    $TargetDir = Join-Path $PSScriptRoot 'target\release'
}
if (-not (Test-Path $TargetDir)) {
    New-Item -ItemType Directory -Path $TargetDir -Force | Out-Null
}

# ---- DLLs the encoder needs at runtime (ffmpeg-next is configured with the
#      filter/format/software-resampling/software-scaling features only, so
#      libavdevice is NOT linked and its DLL is NOT required) -----------------
$dlls = @(
    'avcodec-62.dll',
    'avformat-62.dll',
    'avfilter-11.dll',
    'avutil-60.dll',
    'swresample-6.dll',
    'swscale-9.dll'
)

$copied = 0
foreach ($dll in $dlls) {
    $src = Join-Path $SourceDir $dll
    if (-not (Test-Path $src)) {
        Write-Warning "Missing in $SourceDir : $dll (skipped)"
        continue
    }
    Copy-Item $src -Destination (Join-Path $TargetDir $dll) -Force
    $copied++
}

Write-Host "Copied $copied DLL(s) from '$SourceDir' to '$TargetDir'"