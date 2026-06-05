mod ext4;
mod sparse;
mod extractor;
mod build;

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

        /// Image size in MB (minimum 64)
        #[arg(short = 's', long = "size", default_value = "64")]
        size_mb: u64,
    },
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
        Command::Build { input, output, size_mb } => {
            if !input.is_dir() {
                eprintln!("Error: Source is not a directory: {}", input.display());
                std::process::exit(1);
            }
            println!("Building ext4 image from: {}", input.display());
            println!("Output: {}", output.display());
            println!("Target size: {} MB\n", size_mb);

            build::build_image(&input, &output, size_mb)?;

            println!("\n=====================================");
            println!("Image built: {}", output.display());
            println!("Build successful! ({:.2}s)", start.elapsed().as_secs_f64());
        }
    }

    Ok(())
}
