use clap::Parser;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

#[derive(Parser, Debug)]
#[command(author, version, about = "Rust FFmpeg NVENC Batch Encoder", long_about = None)]
struct Args {
    /// Input video file paths or folder paths
    #[arg(short, long, required = true, num_args = 1..)]
    inputs: Vec<PathBuf>,

    /// Base destination directory for output files (Defaults to `output` in the current directory)
    #[arg(short, long)]
    output_dir: Option<PathBuf>,

    /// Custom output file extension (e.g., mkv, mp4)
    #[arg(short, long, default_value = "mkv")]
    extension: String,

    /// Custom suffix to append to the input file name (e.g., '_enc')
    #[arg(short, long, default_value = "_enc")]
    suffix: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // 1. Discover all actual video files from the provided inputs (files or folders)
    let mut files_to_process = Vec::new();
    for input_path in args.inputs {
        collect_video_files(input_path, &mut files_to_process);
    }

    if files_to_process.is_empty() {
        eprintln!("⚠️ No valid video files found to process.");
        return Ok(());
    }

    println!(
        "🚀 Starting batch processing of {} files...",
        files_to_process.len()
    );

    // 2. Process the gathered files
    for input_path in files_to_process {
        // Determine the output directory dynamically:
        let base_dir = match &args.output_dir {
            Some(dir) => dir.clone(),
            None => PathBuf::from("output"),
        };
        let parent_name = input_path
            .parent()
            .and_then(|p| p.file_name())
            .unwrap_or(Path::new(".").as_os_str());
        let target_dir = base_dir.join(parent_name);

        // Ensure the determined target directory exists
        if !target_dir.exists() {
            std::fs::create_dir_all(&target_dir)?;
        }

        // Generate output path based on user rules
        let output_path =
            build_output_path(&input_path, &target_dir, &args.suffix, &args.extension);

        // Detect source bit depth and match the encoder to it, avoiding the
        // broken 8->10 bit conversion in scale_cuda (green output on NVENC)
        let is_10bit = match probe_pixel_format(&input_path).await {
            Some(pix_fmt) => is_10bit_pix_fmt(&pix_fmt),
            None => false,
        };
        let scale_format = if is_10bit { "p010le" } else { "nv12" };
        let profile = if is_10bit { "main10" } else { "main" };

        println!(
            "\n🎬 Processing: {:?} ({} bit)",
            input_path,
            if is_10bit { "10" } else { "8" }
        );
        println!("➡️ Saving to:   {:?}", output_path);

        // Construct the FFmpeg command
        let mut cmd = Command::new("ffmpeg");
        cmd.arg("-y")
            .arg("-hwaccel")
            .arg("cuda")
            .arg("-hwaccel_output_format")
            .arg("cuda")
            .arg("-i")
            .arg(input_path.to_str().unwrap())
            .arg("-vf")
            .arg(format!("scale_cuda=format={}", scale_format))
            .arg("-c:v")
            .arg("hevc_nvenc")
            .arg("-profile:v")
            .arg(profile)
            .arg("-preset")
            .arg("slow")
            .arg("-rc")
            .arg("constqp")
            .arg("-cq")
            .arg("22")
            .arg("-c:a")
            .arg("copy")
            .arg("-map_metadata")
            .arg("0")
            .arg(output_path.to_str().unwrap());

        // Inherit stdout/stderr for live FFmpeg progress
        cmd.stdout(Stdio::inherit());
        cmd.stderr(Stdio::inherit());

        let mut child = cmd.spawn()?;
        let status = child.wait().await?;

        if status.success() {
            println!("✅ Successfully encoded: {:?}", output_path);
        } else {
            eprintln!("❌ FFmpeg failed on file: {:?}", input_path);
        }
    }

    println!("\n🎉 All processes completed!");
    Ok(())
}

/// Helper function to construct the custom output file path
fn build_output_path(input: &Path, out_dir: &Path, suffix: &str, ext: &str) -> PathBuf {
    let stem = input.file_stem().unwrap().to_string_lossy();
    let new_filename = format!("{}{}.{}", stem, suffix, ext);
    out_dir.join(new_filename)
}

/// Recursively scans directories for files matching common video extensions
fn collect_video_files(path: PathBuf, files: &mut Vec<PathBuf>) {
    // List of standard video extensions to look for
    let valid_extensions = ["mp4", "mkv", "mov", "avi", "flv", "webm", "m4v", "wmv"];

    if path.is_file() {
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if valid_extensions.contains(&ext.to_lowercase().as_str()) {
                files.push(path);
            }
        }
    } else if path.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                collect_video_files(entry.path(), files);
            }
        }
    }
}

/// Probes the pixel format of the first video stream using ffprobe
async fn probe_pixel_format(path: &Path) -> Option<String> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=pix_fmt",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
            path.to_str()?,
        ])
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let pix_fmt = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!pix_fmt.is_empty()).then_some(pix_fmt)
}

/// Returns true for 10-bit pixel formats (e.g. yuv420p10le, p010le)
fn is_10bit_pix_fmt(pix_fmt: &str) -> bool {
    pix_fmt.contains("p10") || pix_fmt.contains("10le") || pix_fmt.contains("10be")
}