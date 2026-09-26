# Stages a self-contained rust-ffmpeg-cli bundle, checks that it actually runs
# with an empty PATH, and zips it into dist/.
#
# The bundle is what the zip contains:
#
#   rust-ffmpeg-cli.exe   the real compiled binary
#   avcodec-62.dll ...    the FFmpeg DLLs, from copy-dlls.ps1
#
# No launcher script is needed here: Windows resolves DLLs from the exe's own
# directory before searching PATH, so the binary and the DLLs sitting beside it
# are self-sufficient. (The unix bundle does ship rust-ffmpeg-cli.sh because
# the Linux/macOS dynamic loader has no such default.)
#
# The staging directory (target\package) is wiped on every run, so packaging
# twice in a row is idempotent. Nothing cargo owns is touched: the release
# binary is copied out of target\, never renamed or moved.
#
# Usage:
#   .\package.ps1 [-Binary <path>] [-OutDir <dir>] [-SourceDir <dir>]
#                 [-NoVerify] [-VerifyOnly] [-Clean]
#
# `just package` runs this with no arguments; it is equally usable on its own.

[CmdletBinding()]
param(
    [string]$Binary,
    [string]$OutDir,
    [string]$SourceDir,
    [switch]$NoVerify,
    [switch]$VerifyOnly,
    [switch]$Clean
)

$ErrorActionPreference = 'Stop'

$Root     = $PSScriptRoot
$Stage    = Join-Path $Root 'target\package'
$CopyDlls = Join-Path $Root 'copy-dlls.ps1'
$Manifest = Join-Path $Root 'Cargo.toml'

# ---- Platform detection -----------------------------------------------------
# PROCESSOR_ARCHITEW6432 takes over when a 32-bit PowerShell runs under WOW64
# on a 64-bit CPU, where PROCESSOR_ARCHITECTURE reports the emulated arch.
$rawArch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
$arch = switch -Regex ([string]$rawArch) {
    '^(AMD64|x86_64)$'  { 'x64' }
    '^(ARM64|aarch64)$' { 'arm64' }
    default             { ([string]$rawArch).ToLowerInvariant() }
}
if (-not $arch) { $arch = 'x64' }   # neither variable set; assume the common case

# ---- The smoke test ---------------------------------------------------------
# Runs the staged exe with PATH emptied, so an FFmpeg installed system-wide
# cannot stand in for a DLL that is missing from the bundle. A pass here means
# the archive is self-contained.
function Test-Bundle {
    param([string]$Dir)

    $exe = Join-Path $Dir 'rust-ffmpeg-cli.exe'
    if (-not (Test-Path $exe)) {
        throw "no staged bundle at $Dir (run 'just package' first)"
    }

    $savedPath = $env:PATH
    try {
        $env:PATH = ''
        & $exe --version
        if ($LASTEXITCODE -ne 0) {
            throw "the staged bundle exited with $LASTEXITCODE"
        }
    }
    finally {
        $env:PATH = $savedPath
    }
    Write-Host 'OK: bundle starts and reports its version (PATH scrubbed)'
}

if ($Clean) {
    foreach ($path in @($Stage, (Join-Path $Root 'dist'))) {
        if (Test-Path $path) { Remove-Item -Recurse -Force $path }
    }
    Write-Host 'Removed package\ and dist\'
    return
}

if ($VerifyOnly) {
    Test-Bundle -Dir $Stage
    return
}

# ---- Resolve the compiled binary ---------------------------------------------
if (-not $Binary) {
    $profile = if ($env:TARGET) { Join-Path $Root "target\$env:TARGET\release" } else { Join-Path $Root 'target\release' }
    $Binary  = Join-Path $profile 'rust-ffmpeg-cli.exe'
}
if (-not (Test-Path $Binary)) {
    throw "release binary not found: $Binary (run 'just build' first)"
}

# ---- Version (Cargo.toml is the single source of truth) ----------------------
if (-not (Select-String -Path $Manifest -Pattern '^version\s*=\s*"[^"]+"' -Quiet)) {
    throw "could not read a version from $Manifest"
}
$version = (Select-String -Path $Manifest -Pattern '^version\s*=\s*"([^"]+)"' |
            Select-Object -First 1).Matches[0].Groups[1].Value

# ---- Output directory --------------------------------------------------------
if (-not $OutDir) { $OutDir = Join-Path $Root 'dist' }
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path

# ---- Stage --------------------------------------------------------------------
if (Test-Path $Stage) { Remove-Item -Recurse -Force $Stage }
New-Item -ItemType Directory -Force -Path $Stage | Out-Null

Copy-Item $Binary (Join-Path $Stage 'rust-ffmpeg-cli.exe') -Force

# Library copying is already implemented in copy-dlls.ps1 - reuse it rather
# than duplicating the list.
$copyArgs = @{ TargetDir = $Stage }
if ($SourceDir) { $copyArgs.SourceDir = $SourceDir }
& $CopyDlls @copyArgs

if (-not $NoVerify) {
    Test-Bundle -Dir $Stage
}

# ---- Archive -------------------------------------------------------------------
# Flat: every entry sits at the archive root, so extracting needs no cleanup.
$archive = Join-Path $OutDir "rust-ffmpeg-cli-$version-windows-$arch.zip"
if (Test-Path $archive) { Remove-Item -Force $archive }
Compress-Archive -Path (Join-Path $Stage '*') -DestinationPath $archive -Force

$files = (Get-ChildItem -File $Stage).Count + (Get-ChildItem -Directory $Stage).Count
$bytes = (Get-ChildItem -File $Stage | Measure-Object -Property Length -Sum).Sum
Write-Host ''
Write-Host "Packaged $archive"
Write-Host ('  {0} files, {1:N1} MB staged' -f $files, ($bytes / 1MB))
Write-Host '  entry point: .\rust-ffmpeg-cli.exe -i <input>'
