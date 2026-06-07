# ext4tool

Extracts files from ext4/sparse Android images and builds ext4 images from directories. Written in Rust.

## Build

```sh
cargo build --release
```

Binary at `target/release/ext4tool`. Works on Linux, Windows, and macOS.

## Usage

```sh
ext4tool <command> [options]

Commands:
  extract    Extract files from an ext4/sparse image
  build      Build an ext4 image from a source directory
```

Set `DEBUG=1` for verbose diagnostic output (superblock details, extent trees, xattrs, file fragmentation).

### Extract

```sh
ext4tool extract -i <image> -o <output_dir>

  -i, --input      ext4 image file (.img or .simg)
  -o, --output     output directory
  -k, --keep-raw   keep the converted raw file (sparse images only)
  -t, --threads    number of worker threads (default: 4)
```

Extracts all files preserving permissions, SELinux labels, capabilities, symlinks, and xattrs. Generates Android-compatible `fs_config` and `file_contexts` in `<output_dir>/config/`.

```sh
ext4tool extract -i system.img -o out/
ext4tool extract -i vendor.simg -o out/ --keep-raw -t 8
```

### Build

```sh
ext4tool build -i <src_dir> -o <image> -s <size>

  -i, --input            source directory
  -o, --output           ext4 image file
  -s, --size             image size (e.g. 4096M, 4G)
  --block-size           block size in bytes (1024, 2048, 4096; default: 4096)
  --reserved-percent     percentage of reserved blocks (default: 0)
  --fs-config            fs_config file for uid/gid/mode overrides
  --fs-contexts          file_contexts file for SELinux labels
  --fs-contexts-prefix   mount point prefix (e.g. "vendor" for /vendor)
  --sparse               output Android sparse format
```

Builds an ext4 image from a source directory. Supports regular files, directories, and symlinks. Permissions are preserved. The image passes `e2fsck` and is mountable.

**fs_config / file_contexts validation**: when `--fs-config` or `--fs-contexts` is provided, every entry in the source directory **must** have a matching entry. Missing entries fail the build with a list of unmatched paths. Set `--fs-contexts-prefix` to match paths under a mount point (e.g. `vendor`).

The volume label and `last mounted` field are set to the `--fs-contexts-prefix` value for partition identification.

```sh
ext4tool build -i my_rootfs/ -o rootfs.img -s 256M
ext4tool build -i vendor/ -o vendor.img -s 1024M \
  --fs-config config/vendor_fs_config \
  --fs-contexts config/vendor_file_contexts \
  --fs-contexts-prefix vendor \
  --sparse
```

## Features

| RoCompat | Incompat |
|----------|----------|
| SPARSE_SUPER | FILETYPE |
| HUGE_FILE | EXTENTS |
| DIR_NLINK | |
| EXTRA_ISIZE | |

- 4096-byte blocks, 256-byte inodes, extent-based file storage
- Inline symlinks (< 60 bytes stored in i_block)
- 64-bit addressing, no journal, no metadata_csum

## Sparse images

Android `.simg` files are auto-detected by scanning for sparse magic `0xED26FF3A`. Three chunk types handled: RAW, FILL, DONT_CARE. Sparse output via `--sparse` flag.

## Caveats

- Hard links are extracted as separate files
- Special files (device nodes, FIFOs, sockets) are skipped on Windows
- `chown` requires root on Linux
- Windows symlink creation uses MSYS2-compatible reparse points

## Python version

Original Python implementation at `ext4.py` + `imgextractor.py` for reference. The Rust version produces identical output.
