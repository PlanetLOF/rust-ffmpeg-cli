# Build and packaging automation for rust-ffmpeg-cli.
#
#   just                  list the recipes
#   just build            cargo build --release
#   just libs             copy the FFmpeg shared libraries next to the dev binary
#   just package          build, stage a bundle, verify it runs, archive it
#   just smoke            re-verify the last staged bundle, rebuild nothing
#   just fmt              format the Rust sources with rustfmt
#   just clean            remove the staging dir and dist/
#
# Override the target triple the way cargo does — as an environment variable:
#
#   TARGET=aarch64-unknown-linux-gnu just package
#
# Each recipe body is a single command line that is valid under both `sh -cu`
# (unix) and `cmd /c` (windows), so `just package` does the right thing on
# every host. All the real work lives in package.sh / package.ps1, which stay
# usable on their own if you would rather not have `just` installed.

pkg := 'rust-ffmpeg-cli'
root := justfile_directory()

# `TARGET=<triple>` selects the artifact directory; unset means a host build,
# which is what plain `cargo build --release` produces.
target := env('TARGET', '')
build_flag := if target == '' { '' } else { ' --target ' + target }
profile := root + if target == '' { '/target/release' } else { '/target/' + target + '/release' }

# Platform dispatch: run the native script with the native interpreter,
# whichever shell just itself happens to use on this host.
interp := if os_family() == 'windows' { 'powershell -NoProfile -ExecutionPolicy Bypass -File' } else { 'bash' }
package_script := if os_family() == 'windows' { 'package.ps1' } else { 'package.sh' }
dlls_script := if os_family() == 'windows' { 'copy-dlls.ps1' } else { 'copy-dlls.sh' }
dlls_flag := if os_family() == 'windows' { '-TargetDir' } else { '--target' }

# List the recipes.
default:
    @just --list

# Build the release binary.
build:
    cargo build --release{{ build_flag }}

# Copy the FFmpeg shared libraries next to the dev binary.
libs: build
    {{ interp }} {{ dlls_script }} {{ dlls_flag }} "{{ profile }}"

# Build a self-contained bundle and archive it into dist/.
package: build
    {{ interp }} {{ package_script }}

# Re-run the smoke test against the bundle that is already staged.
smoke:
    {{ interp }} {{ package_script }} --verify-only

# Format the Rust sources.
fmt:
    cargo fmt

# Remove the staging directory and dist/.
clean:
    {{ interp }} {{ package_script }} --clean
