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

    /// Destination directory for output files (Defaults to a subdirectory inside the input file's parent directory, named after the parent)
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
        let target_dir = match &args.output_dir {
            Some(dir) => dir.clone(), 
            None => {
                let parent = input_path.parent().unwrap_or(Path::new("."));
                let dir_name = parent.file_name().unwrap();
                parent.join(dir_name)
            }
        };

        // Ensure the determined target directory exists
        if !target_dir.exists() {
            std::fs::create_dir_all(&target_dir)?;
        }

        // Generate output path based on user rules
        let output_path =
            build_output_path(&input_path, &target_dir, &args.suffix, &args.extension);

        println!("\n🎬 Processing: {:?}", input_path);
        println!("➡️ Saving to:   {:?}", output_path);

        // Construct the FFmpeg command
        let mut cmd = Command::new("ffmpeg");
        cmd.args(&[
            "-y",
            "-hwaccel",
            "cuda",
            "-hwaccel_output_format",
            "cuda",
            "-i",
            input_path.to_str().unwrap(),
            "-vf",
            "scale_cuda=format=p010le",
            "-c:v",
            "hevc_nvenc",
            "-profile:v",
            "main10",
            "-preset",
            "slow",
            "-rc",
            "constqp",
            "-cq",
            "22",
            "-c:a",
            "copy",
            "-map_metadata",
            "0",
            output_path.to_str().unwrap(),
        ]);

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