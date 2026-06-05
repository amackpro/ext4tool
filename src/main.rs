mod ext4;
mod sparse;
mod extractor;

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use std::fs;

#[derive(Parser)]
#[command(name = "ext4tool")]
#[command(about = "Extract ext4/sparse Android images", long_about = None)]
#[command(version)]
struct Args {
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
}

fn main() -> Result<()> {
    let start = std::time::Instant::now();

    let args = Args::parse();

    println!("ext4tool v{}", env!("CARGO_PKG_VERSION"));
    println!("=====================================\n");

    // Check if input exists
    if !args.input.exists() {
        eprintln!("Error: Input file does not exist: {}", args.input.display());
        std::process::exit(1);
    }

    // Determine if we need to convert from sparse format
    let is_sparse = sparse::is_sparse_image(&args.input)?;

    let image_path = if is_sparse {
        println!("Detected sparse Android image format\n");

        // Create temporary raw image path
        let raw_path = args.input.with_extension("img.raw");

        println!("Converting sparse image to raw format...");
        sparse::convert_sparse_to_raw(&args.input, &raw_path)?;

        println!("\nSparse conversion complete!\n");
        raw_path
    } else {
        println!("Detected raw ext4 image format\n");
        args.input.clone()
    };

    // Get partition name from image filename
    let partition_name = image_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("image")
        .to_string();

    // Extract the filesystem
    let extractor = extractor::Extractor::new(&args.output, partition_name, args.threads);
    extractor.extract(&image_path)?;

    // Cleanup temporary raw image if needed
    if is_sparse && !args.keep_raw {
        if let Err(e) = fs::remove_file(&image_path) {
            eprintln!("Warning: Failed to remove temporary raw image: {}", e);
        }
    }

    println!("\n=====================================");
    println!("Output directory: {}", args.output.display());
    println!("Extraction successful! ({:.2}s)", start.elapsed().as_secs_f64());

    Ok(())
}

