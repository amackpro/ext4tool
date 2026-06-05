# ext4tool

Extracts files from ext4 filesystem images. Written in Rust.

## What it does

Takes an ext4 image (raw .img or Android sparse .simg) and dumps all files to a directory. Preserves permissions, SELinux labels, capabilities, symlinks, and xattrs where the OS supports it.

Output includes Android-compatible `fs_config` and `file_contexts` files for rebuilding images.

## Build

```sh
cargo build --release
```

No dependencies beyond Rust. Binary ends up at `target/release/ext4tool`.

## Usage

```
ext4tool -i <image> -o <output_dir>

  -i, --input      ext4 image file (.img or .simg)
  -o, --output     output directory
  -k, --keep-raw   keep the converted raw file (sparse images only)
  -t, --threads    number of worker threads (default: 4)
```

Examples:

```sh
ext4tool -i system.img -o out/
ext4tool -i vendor.simg -o out/ --keep-raw -t 8
```

## Caveats

- Hard links are extracted as separate files.
- Special files (device nodes, FIFOs, sockets) are skipped.
- `chown` requires root on Linux.
- Windows symlink creation uses MSYS2-compatible reparse points (`!<symlink>` + `attrib +s`). Requires Windows 10+ with Developer Mode for native NTFS symlinks; otherwise falls back to MSYS2-style reparse files which work with MSYS2 `mkfs.ext4`.

## How it works

Single-pass traversal + concurrent extraction:

1. A single thread walks the directory tree, pushing regular file tasks to a shared queue and handling symlinks/directories inline.
2. Worker threads pop from the queue and extract file data in parallel, each opening the image volume once at startup.
3. Completion is signaled via `Condvar` — no busy-waiting.

Metadata files go in `<output_dir>/config/`:
- `<name>_fs_config` — per-file uid/gid/mode (includes symlink targets)
- `<name>_file_contexts` — SELinux labels
- `<name>_size.txt` — image size
- `<name>_name.txt` — partition name
- `<name>_space.txt` — files with spaces (if any)

## Sparse images

Android .simg files are detected by scanning the first 4 MB for the sparse magic (0xED26FF3A). The tool converts them to raw before extraction. Three chunk types are handled: RAW (copy data), FILL (repeat pattern), DONT_CARE (write zeros).

## Python version

There's also an original Python implementation (`ext4.py` + `imgextractor.py`). It does the same thing but slower and uses more memory. The Rust version is a direct rewrite, producing identical output.

## License

Rewrite of the original Python ext4 extractor. Refer to original licensing.
