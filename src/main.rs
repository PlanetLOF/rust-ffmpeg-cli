use clap::Parser;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

#[derive(Parser, Debug)]
#[command(author, version, about = "Rust FFmpeg NVENC Batch Encoder", long_about = None)]
struct Args {
    /// Input video file paths (supports multiple files from different directories)
    #[arg(short, long, required = true, num_args = 1..)]
    inputs: Vec<PathBuf>,

    /// Destination directory for output files (Defaults to the input file's directory if not set)
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

    println!(
        "🚀 Starting batch processing of {} files...",
        args.inputs.len()
    );

    for input_path in args.inputs {
        if !input_path.exists() {
            eprintln!("⚠️ File not found: {:?}, skipping.", input_path);
            continue;
        }

        // Determine the output directory dynamically:
        // Use the explicit output_dir if provided, otherwise fallback to the input file's parent folder.
        let target_dir = match &args.output_dir {
            Some(dir) => dir.clone(), // If you set -o, use that exact folder for everything
            None => input_path.parent().unwrap_or(Path::new(".")).to_path_buf(), // If not, use the input file's folder
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
