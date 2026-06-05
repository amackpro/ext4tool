# ext4tool

Extracts files from ext4 filesystem images and builds ext4 images from directories. Written in Rust.

## Build

```sh
cargo build --release
```

No dependencies beyond Rust. Binary ends up at `target/release/ext4tool`.

## Usage

```
ext4tool <command> [options]

Commands:
  extract    Extract files from an ext4 image
  build      Build an ext4 image from a source directory
```

### Extract

```
ext4tool extract -i <image> -o <output_dir>

  -i, --input      ext4 image file (.img or .simg)
  -o, --output     output directory
  -k, --keep-raw   keep the converted raw file (sparse images only)
  -t, --threads    number of worker threads (default: 4)
```

Extracts all files from an ext4 image, preserving permissions, SELinux labels, capabilities, symlinks, and xattrs where supported. Output includes Android-compatible `fs_config` and `file_contexts` files for rebuilding images.

```sh
ext4tool extract -i system.img -o out/
ext4tool extract -i vendor.simg -o out/ --keep-raw -t 8
```

### Build

```
ext4tool build -i <src_dir> -o <image> -s <size_mb>

  -i, --input     source directory
  -o, --output    ext4 image file (.img)
  -s, --size      target image size in MB
```

Builds an ext4 filesystem image from a source directory. Supports regular files, directories, and symlinks. Permissions (mode bits) are preserved. The resulting image passes `e2fsck` cleanly and is mountable via `mount -o loop`.

```sh
ext4tool build -i my_rootfs/ -o rootfs.img -s 256
```

## Caveats

- Hard links are extracted as separate files.
- Special files (device nodes, FIFOs, sockets) are skipped.
- `chown` requires root on Linux.
- Windows symlink creation uses MSYS2-compatible reparse points (`!<symlink>` + `attrib +s`). Requires Windows 10+ with Developer Mode for native NTFS symlinks; otherwise falls back to MSYS2-style reparse files which work with MSYS2 `mkfs.ext4`.

## How extraction works

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

## How building works

Two-phase build:

1. **Walk + assign**: scan the source directory, build a file tree, and assign inode numbers.
2. **BFS write**: write inodes, directory blocks, and file data level-by-level (parents before children) so that directory entries always reference existing inodes.

Internal details:
- 4096-byte blocks, 256-byte inodes, 2048 inodes/group, 32768 blocks/group
- Extent-based file storage (single contiguous extent per file)
- Inline symlinks (< 60 bytes stored directly in i_block)
- No journal, 64-bit addressing, metadata_csum, or resize_inode features

## Sparse images

Android .simg files are detected by scanning the first 4 MB for the sparse magic (0xED26FF3A). The tool converts them to raw before extraction. Three chunk types are handled: RAW (copy data), FILL (repeat pattern), DONT_CARE (write zeros).

## Python version

There's also an original Python implementation (`ext4.py` + `imgextractor.py`). It does the same thing but slower and uses more memory. The Rust version is a direct rewrite, producing identical output.

## License

Rewrite of the original Python ext4 extractor. Refer to original licensing.
