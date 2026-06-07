mod config;
mod ext4;
mod sparse;
mod extractor;
mod builder;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::fs;

#[derive(Parser)]
#[command(name = "ext4tool")]
#[command(about = "Extract and build ext4/sparse Android images", long_about = None)]
#[command(version)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Extract an ext4/sparse image
    Extract {
        /// Input image file (.img or .simg)
        #[arg(short = 'i', long = "input", value_name = "IMAGE")]
        input: PathBuf,

        /// Output directory
        #[arg(short = 'o', long = "output", value_name = "DIR")]
        output: PathBuf,

        /// Keep converted raw image (don't delete)
        #[arg(short = 'k', long = "keep-raw")]
        keep_raw: bool,

        /// Number of threads (default: 4)
        #[arg(short = 't', long = "threads", default_value = "4")]
        threads: usize,
    },
    /// Build an ext4 image from a directory
    Build {
        /// Source directory
        #[arg(short = 'i', long = "input", value_name = "DIR")]
        input: PathBuf,

        /// Output image file
        #[arg(short = 'o', long = "output", value_name = "IMAGE")]
        output: PathBuf,

        /// Image size (e.g. 4096M, 4G, 4294967296; default MB)
        #[arg(short = 's', long = "size", default_value = "64M")]
        size: String,

        /// Block size in bytes (1024, 2048, or 4096)
        #[arg(long = "block-size", default_value = "4096")]
        block_size: u64,

        /// Percentage of reserved blocks (0-50)
        #[arg(long = "reserved-percent", default_value = "0")]
        reserved_percent: u32,

        /// fs_config file for uid/gid/mode overrides
        #[arg(long = "fs-config")]
        fs_config: Option<PathBuf>,

        /// file_contexts file for SELinux labels
        #[arg(long = "fs-contexts")]
        file_contexts: Option<PathBuf>,

        /// Mount point prefix for file_contexts matching (e.g., "vendor" for /vendor partition)
        #[arg(long = "fs-contexts-prefix")]
        fs_contexts_prefix: Option<String>,

        /// Output sparse image (Android sparse format)
        #[arg(long = "sparse")]
        sparse: bool,
    },
}

/// Parse a size string with optional suffix (K, M, G). No suffix = bytes.
fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty string".into());
    }
    let (num_str, mult) = if let Some((n, sfx)) = s.split_at_checked(s.len() - 1) {
        match sfx.to_uppercase().as_str() {
            "K" => (n, 1024u64),
            "M" => (n, 1024u64 * 1024),
            "G" => (n, 1024u64 * 1024 * 1024),
            _ => (s, 1u64), // no suffix or "B" — raw bytes
        }
    } else {
        (s, 1u64)
    };
    let num: u64 = num_str.parse().map_err(|_| format!("invalid number '{}'", num_str))?;
    Ok(num * mult)
}

fn main() -> Result<()> {
    let start = std::time::Instant::now();
    let args = Args::parse();

    println!("ext4tool v{}", env!("CARGO_PKG_VERSION"));
    println!("=====================================\n");

    match args.command {
        Command::Extract { input, output, keep_raw, threads } => {
            // Check if input exists
            if !input.exists() {
                eprintln!("Error: Input file does not exist: {}", input.display());
                std::process::exit(1);
            }

            // Determine if we need to convert from sparse format
            let is_sparse = sparse::is_sparse_image(&input)?;

            let image_path = if is_sparse {
                println!("Detected sparse Android image format\n");

                // Create temporary raw image path
                let raw_path = input.with_extension("img.raw");

                println!("Converting sparse image to raw format...");
                sparse::convert_sparse_to_raw(&input, &raw_path)?;

                println!("\nSparse conversion complete!\n");
                raw_path
            } else {
                println!("Detected raw ext4 image format\n");
                input
            };

            // Get partition name from image filename
            let partition_name = image_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("image")
                .to_string();

            // Extract the filesystem
            let extractor = extractor::Extractor::new(&output, partition_name, threads);
            extractor.extract(&image_path)?;

            // Cleanup temporary raw image if needed
            if is_sparse && !keep_raw {
                if let Err(e) = fs::remove_file(&image_path) {
                    eprintln!("Warning: Failed to remove temporary raw image: {}", e);
                }
            }

            println!("\n=====================================");
            println!("Output directory: {}", output.display());
            println!("Extraction successful! ({:.2}s)", start.elapsed().as_secs_f64());
        }
        Command::Build { input, output, size, block_size, reserved_percent, fs_config, file_contexts, fs_contexts_prefix, sparse } => {
            if !input.is_dir() {
                eprintln!("Error: Source is not a directory: {}", input.display());
                std::process::exit(1);
            }

            let size_bytes = parse_size(&size)
                .unwrap_or_else(|e| { eprintln!("Error: invalid size '{}': {}", size, e); std::process::exit(1); });

            // Parse fs_config if provided
            let fs_config_map = if let Some(ref path) = fs_config {
                Some(config::parse_fs_config(path)?)
            } else {
                None
            };

            // Parse file_contexts if provided
            let file_contexts_map = if let Some(ref path) = file_contexts {
                Some(config::parse_file_contexts(path)?)
            } else {
                None
            };

            println!("Building ext4 image from: {}", input.display());
            println!("Output: {}", output.display());
            println!("Target size: {} bytes\n", size_bytes);

            builder::build_image(
                &input, &output, size_bytes, block_size, reserved_percent,
                fs_config_map, file_contexts_map, fs_contexts_prefix, sparse,
            )?;

            println!("\n=====================================");
            println!("Image built: {}", output.display());
            println!("Build successful! ({:.2}s)", start.elapsed().as_secs_f64());
        }
    }

    Ok(())
}
