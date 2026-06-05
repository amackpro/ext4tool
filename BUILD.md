# ext4tool — Build Mode Implementation Plan

## Overview

Add a `build` subcommand to `ext4tool` that constructs a valid ext4 filesystem image
from a source directory, preserving permissions, ownership, symlinks, and metadata.

## Architecture

```
Phase 1: Collect        Phase 2: Compute          Phase 3: Write
┌──────────────┐       ┌──────────────────┐       ┌─────────────────┐
│ walk src dir  │ ──►  │ geometry calc     │ ──►  │ superblock       │
│ build tree    │       │ size estimation   │       │ group descriptors│
│ collect attrs │       │ output file alloc │       │ bitmaps          │
└──────────────┘       └──────────────────┘       │ inode table      │
                                                   │ extent trees     │
                                                   │ directory blocks │
                                                   │ file data        │
                                                   └─────────────────┘
```

## Data Flow

### Phase 1 — Collect

```
src_dir/
├── file.txt         → FileNode { name, path, inode, size, mode, uid, gid, mtime }
├── subdir/          → FileNode { ..., file_type: Directory, children: [...] }
│   └── link         → FileNode { ..., file_type: Symlink, symlink_target: "../file.txt" }
└── script.sh        → FileNode { ..., mode: 0o100755 }
```

- Walk source directory via `walkdir` or manual `fs::read_dir` recursion
- Collect: name, relative path, file type, size, mode, uid, gid, mtime, symlink target
- Build a recursive `FileNode` tree
- Assign inode numbers during walk (sequential from 12)
- Sort so parents precede children (BFS order)

### Phase 2 — Compute Geometry

Input: total data size + file count from Phase 1.

| Parameter | Formula | Default |
|-----------|---------|---------|
| Block size | fixed | 4096 |
| Inode size | fixed | 256 |
| Blocks/group | fixed | 32768 (128 MiB) |
| Inodes/group | fixed | 2048 |
| # groups | `ceil(total_blocks / blocks_per_group)` | ≥1 |
| Inode table blocks | `ceil(inodes_per_group × inode_size / block_size)` | 128 |
| Metadata blocks/group | 4 + inode_table_blocks | 132 |
| First data block | metadata_blocks | 132 |
| Image size | `max(metadata + data, 64 MiB)` rounded up | user-specified |

Block group 0 layout:

```
Block  0: superblock (at byte 1024) + boot padding
Block  1: group descriptor table (32 bytes × num_groups, padded to block)
Block  2: block bitmap (1 block)
Block  3: inode bitmap (1 block)
Blocks 4..131: inode table (128 blocks at 256-byte inodes × 2048)
Blocks 132+:  data blocks
```

Allocated block address = `block_num × block_size` (simple linear layout).

### Phase 3 — Write

#### 3a. Superblock (byte offset 1024)

Essential fields (all others zeroed):

| Offset | Field | Value |
|--------|-------|-------|
| 0x00 | s_inodes_count | total inodes |
| 0x04 | s_blocks_count_lo | total blocks (lo 32) |
| 0x0C | s_free_blocks_count_lo | (lo 32, updated at end) |
| 0x10 | s_free_inodes_count_lo | (updated at end) |
| 0x18 | s_log_block_size | 2 (=4096) |
| 0x20 | s_blocks_per_group | 32768 |
| 0x28 | s_inodes_per_group | 2048 |
| 0x30 | s_wtime | current unix time |
| 0x38 | s_magic | 0xEF53 |
| 0x3A | s_state | 1 (clean) |
| 0x4C | s_rev_level | 1 (dynamic) |
| 0x60 | s_feature_incompat | FILETYPE(0x02) | EXTENTS(0x40) |
| 0x64 | s_feature_ro_compat | SPARSE_SUPER(0x01) | LARGE_FILE(0x08) | GDT_CSUM(0x10) | DIR_NLINK(0x20) | EXTRA_ISIZE(0x40) |
| 0x68 | s_feature_compat | DIR_PREALLOC(0x01) | EXT_ATTR(0x08) | RESIZE_INODE(0x10) |
| 0xD8 | s_uuid | random UUID |
| 0xFE | s_desc_size | 32 (no 64bit) |
| 0x100 | s_first_ino | 12 |
| 0x104 | s_inode_size | 256 |
| 0x12C | s_min_extra_isize | 28 |
| 0x12E | s_want_extra_isize | 28 |
| 0x150 | s_blocks_count_hi | total blocks (hi 32) |
| 0x15C | s_first_error_time | 0 |
| ... | (rest zeroed) | |

No 64bit incompat flag — keeps group descriptors at 32 bytes.

#### 3b. Group Descriptors (block 1)

32 bytes per group:

| Offset | Field | Value |
|--------|-------|-------|
| 0x00 | bg_block_bitmap_lo | 2 |
| 0x04 | bg_inode_bitmap_lo | 3 |
| 0x08 | bg_inode_table_lo | 4 |
| 0x0C | bg_free_blocks_count_lo | (updated at end) |
| 0x0E | bg_free_inodes_count_lo | (updated at end) |
| 0x10 | bg_used_dirs_count_lo | (counted) |
| 0x12 | bg_flags | 0 |
| 0x14–0x1F | zeros / checksums | 0 |

For >1 group, each group gets its own bitmap/inode table at
`block_bitmap = 2 + g × blocks_per_group` (same relative layout).

#### 3c. Bitmaps (blocks 2–3)

- Block bitmap: `blocks_per_group / 8` bytes. Bit N = 1 means block N is used.
  Metadata blocks (0..metadata_blocks-1) are pre-set to 1.
- Inode bitmap: `inodes_per_group / 8` bytes. Bit N = 1 means inode N+1 is used.
  Reserved inodes 0, 1 are pre-set to 1. Others set during allocation.

#### 3d. Inode Table (blocks 4..131)

Each inode is 256 bytes on disk. Written sequentially starting at block 4.

**Inode format (256 bytes):**

| Offset | Size | Field | Notes |
|--------|------|-------|-------|
| 0x00 | 2 | i_mode | file type | permissions |
| 0x02 | 2 | i_uid | low 16 bits |
| 0x04 | 4 | i_size_lo | file size low 32 |
| 0x08 | 4 | i_atime | |
| 0x0C | 4 | i_ctime | |
| 0x10 | 4 | i_mtime | |
| 0x14 | 4 | i_dtime | deleted time = 0 |
| 0x18 | 2 | i_gid | low 16 bits |
| 0x1A | 2 | i_links_count | 1 for files, 2+. for dirs |
| 0x1C | 4 | i_blocks_lo | in 512-byte units |
| 0x20 | 4 | i_flags | EXT4_EXTENTS_FL (0x80000) |
| 0x24 | 4 | i_osd1 | 0 |
| 0x28 | 60 | i_block | extent root (12) + extents (48) |
| 0x64 | 4 | i_generation | 0 |
| 0x68 | 4 | i_file_acl_lo | 0 |
| 0x6C | 4 | i_size_hi | file size high 32 |
| 0x70 | 4 | i_faddr | 0 |
| 0x74 | 12 | i_osd2 | 0 |
| 0x80 | 2 | i_extra_isize | 28 |
| 0x82 | 2 | i_checksum_hi | 0 |
| 0x84 | 4 | i_ctime_extra | 0 |
| 0x88 | 4 | i_mtime_extra | 0 |
| 0x8C | 4 | i_atime_extra | 0 |
| 0x90 | 4 | i_crtime | creation time |
| 0x94 | 4 | i_crtime_extra | 0 |
| 0x98 | 4 | i_version_hi | 0 |
| 0x9C | 4 | i_projid | 0 |
| 0xA0–0xFF | 96 | zero | padding |

**Extent root in i_block (first 12 bytes):**

```
+0: eh_magic    u16 = 0xF30A
+2: eh_entries  u16 = N
+4: eh_max      u16 = 4 (root limit)
+6: eh_depth    u16 = 0
+8: eh_generation u32 = 0
```

**Extent entries (12 bytes each, starting at i_block+12):**

```
+0: ee_block    u32 = logical block number
+4: ee_len      u16 = # of blocks (can be 0x8000 for unwritten)
+6: ee_start_hi u16 = physical block >> 32
+8: ee_start_lo u32 = physical block & 0xFFFFFFFF
```

#### 3e. Directory Blocks

Each directory is a single block with a linked list of `ext4_dir_entry_2`:

```
+0: inode     u32
+4: rec_len   u16 (total entry size, last entry fills to end of block)
+6: name_len  u8
+7: file_type u8
+8: name      variable (padded to 4-byte boundary)
```

Each directory gets two special entries:
- `.` → parent's own inode
- `..` → parent's parent inode (root's `..` is itself)

For a directory /a/b with children c, d:
```
[0]  inode=inode_of_b   rec_len=12 name="."  type=2
[12] inode=inode_of_a   rec_len=12 name=".." type=2
[24] inode=inode_of_c   rec_len=N  name="c"  type=N
[24+N] ... more children ...
[last] fills to 4096
```

#### 3f. Symlinks

If target length ≤ 60 bytes: stored inline in `i_block[0..target_len]`.
If target length > 60 bytes: stored as data blocks with extent tree in i_block.

Symlinks use `i_mode = 0o120777` (S_IFLNK | 0777).
`i_size` = target length.
`i_blocks` = 0 for inline, or data_blocks × (block_size/512) for data-block symlinks.

#### 3g. Regular Files

- Allocate contiguous data blocks from `first_data_block`
- Write file content padded to block boundary
- Create single-extent tree pointing to allocated blocks
- Set `i_mode = source_mode | S_IFREG`
- Set `i_size` = file size
- Set `i_blocks = extent_blocks × (block_size / 512)`

## CLI Integration

```diff
 #[derive(Parser)]
 #[command(name = "ext4tool")]
-enum Command {
+enum Command {
     Extract(ExtractArgs),
+    Build(BuildArgs),
 }

+struct BuildArgs {
+    /// Source directory
+    #[arg(short = 'i', long = "input")]
+    input: PathBuf,
+
+    /// Output image
+    #[arg(short = 'o', long = "output")]
+    output: PathBuf,
+
+    /// Image size in MB (minimum 64)
+    #[arg(short = 's', long = "size", default_value = "64")]
+    size_mb: u64,
+}
```

Example usage:
```
ext4tool build -i ./my_rom/system/ -o system.img -s 512
```

## Edge Cases

| Case | Handling |
|------|----------|
| Empty directory | Single block with `.` and `..` entries |
| Empty file | Extent covering 1 zero-filled block |
| Zero-length symlink | Valid — inline with 0 bytes |
| Deeply nested dirs | BFS ensures parent inodes exist before children |
| Unicode filenames | Stored as raw UTF-8 bytes in dir entries |
| >32-bit file sizes | `i_size_hi` field, `LARGE_FILE` feature |
| >4 extents per file | Depth-1 index nodes (future enhancement; initially single extent) |
| >1 block group | Descriptors at block 1, per-group bitmaps/itable |
| Device files | Not supported initially (mkfs.ext4 often skips them) |
| Hard links | Not supported (each file gets unique inode) |
| Sparse files | Allocated fully (no hole punching) |
| File with spaces | Paths preserved via `PathBuf` |
| Symlink to non-existent target | Stored as-is (ext4 allows it) |

## Verification Checklist

- [x] Image mounts with `mount -o loop image.img /mnt`
- [x] `e2fsck -fn image.img` passes
- [x] All files present with correct sizes
- [x] Directory structure preserved
- [x] Symlinks point to correct targets
- [x] Permissions and ownership match source
- [x] Timestamps preserved

## Future Improvements

- Sparse file support (hole punching via `fallocate`)
- Journal (has_journal feature)
- Multiple extents per file (depth > 0)
- Flex block groups
- Meta_bg mode
- Encryption / casefold / project quota
- Incremental / diff builds
