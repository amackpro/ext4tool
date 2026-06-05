use crate::ext4::{EXT4_ROOT_INODE, EXT4_SUPERBLOCK_OFFSET, EXT4_SUPER_MAGIC};
use crate::ext4::FileType;
use anyhow::{Context, Result};
use byteorder::{ByteOrder, LittleEndian};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;

// ── Geometry ──────────────────────────────────────────────────────────────────
const BLOCK_SIZE: u64 = 4096;
const INODE_SIZE: u16 = 256;
const BLOCKS_PER_GROUP: u32 = 32768;
const INODES_PER_GROUP: u32 = 2048;

const EXT4_FEATURE_INCOMPAT_FILETYPE: u32 = 0x0002;
const EXT4_FEATURE_INCOMPAT_EXTENTS: u32 = 0x0040;
const EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER: u32 = 0x0001;
const EXT4_FEATURE_RO_COMPAT_LARGE_FILE: u32 = 0x0008;
#[allow(unused)]
const EXT4_FEATURE_RO_COMPAT_GDT_CSUM: u32 = 0x0010;
const EXT4_FEATURE_RO_COMPAT_DIR_NLINK: u32 = 0x0020;
const EXT4_FEATURE_RO_COMPAT_EXTRA_ISIZE: u32 = 0x0040;
const EXT4_FEATURE_COMPAT_DIR_PREALLOC: u32 = 0x0001;
const EXT4_FEATURE_COMPAT_EXT_ATTR: u32 = 0x0008;
// (reserved, not used — resize_inode requires extra inode)

const EXT4_FIRST_USER_INO: u32 = 12;
const EXT4_EXTENT_MAGIC: u16 = 0xF30A;
const EXT4_EXTENTS_FL: u32 = 0x00080000;

const EXT4_FT_REG_FILE: u8 = 1;
const EXT4_FT_DIR: u8 = 2;
const EXT4_FT_SYMLINK: u8 = 7;

// ── Per-file metadata collected during walk ───────────────────────────────────
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct FileEntry {
    inode: u32,
    file_type: FileType,
    size: u64,
    mode: u16,
    uid: u32,
    gid: u32,
    mtime: u64,
    symlink_target: Option<Vec<u8>>,
    source_path: Option<PathBuf>,
    rel_path: PathBuf,
}

// ── Tree node for directory hierarchy ─────────────────────────────────────────
#[derive(Debug, Clone)]
struct FileNode {
    name: String,
    inode: u32,
    file_type: FileType,
    children: Vec<FileNode>,
}

// ── Builder ───────────────────────────────────────────────────────────────────
pub struct Builder {
    block_size: u64,
    inode_size: u16,
    blocks_per_group: u32,
    inodes_per_group: u32,
    num_groups: u32,
    total_blocks: u64,
    total_inodes: u32,

    // Per-group metadata block counts
    #[allow(dead_code)]
    metadata_blocks: u64,    // blocks consumed by sb + gdt + bm + im + itable per group
    #[allow(dead_code)]
    first_data_block: u64,   // first usable data block (group-relative, absolute for group 0)

    // Allocation trackers
    next_inode: u32,
    next_data_block: u64,    // absolute block number
    block_bitmap: Vec<u8>,
    inode_bitmap: Vec<u8>,
    dir_count: u16,

    file: fs::File,
}

impl Builder {
    /// Create a new ext4 filesystem with total_size bytes.
    pub fn create<P: AsRef<Path>>(path: P, total_size: u64) -> Result<Self> {
        let total_blocks = total_size / BLOCK_SIZE;
        let num_groups = ((total_blocks + BLOCKS_PER_GROUP as u64 - 1) / BLOCKS_PER_GROUP as u64) as u32;
        let num_groups = num_groups.max(1);
        let total_inodes = num_groups * INODES_PER_GROUP;

        let itable_blocks = ((INODES_PER_GROUP as u64 * INODE_SIZE as u64) + BLOCK_SIZE - 1) / BLOCK_SIZE;
        let metadata_blocks = 4 + itable_blocks; // sb + gdt + bm + im + itable

        let bm_bytes = (BLOCKS_PER_GROUP as u64 + 7) / 8;
        let mut block_bitmap = vec![0u8; bm_bytes as usize];
        // Mark all metadata blocks in group 0 as used
        for b in 0..metadata_blocks {
            set_bit(&mut block_bitmap, b);
        }

        let im_bytes = (INODES_PER_GROUP as u64 + 7) / 8;
        let mut inode_bitmap = vec![0u8; im_bytes as usize];
        // Mark reserved inodes 0-11 as used
        for i in 0..12u64 {
            set_bit(&mut inode_bitmap, i);
        }

        let file = fs::File::create(path.as_ref())
            .with_context(|| format!("Failed to create {}", path.as_ref().display()))?;
        file.set_len(total_size)
            .with_context(|| "Failed to allocate output file")?;

        let mut b = Builder {
            block_size: BLOCK_SIZE,
            inode_size: INODE_SIZE,
            blocks_per_group: BLOCKS_PER_GROUP,
            inodes_per_group: INODES_PER_GROUP,
            num_groups,
            total_blocks,
            total_inodes,
            metadata_blocks,
            first_data_block: metadata_blocks,
            next_inode: EXT4_FIRST_USER_INO,
            next_data_block: metadata_blocks,
            block_bitmap,
            inode_bitmap,
            dir_count: 0,
            file,
        };

        b.write_superblock()?;
        b.write_group_descriptors()?;
        Ok(b)
    }

    // ── Allocation helpers ──────────────────────────────────────────────────

    fn alloc_inode(&mut self) -> u32 {
        let ino = self.next_inode;
        self.next_inode += 1;
        let idx = (ino - 1) as u64;
        if idx < self.inodes_per_group as u64 {
            set_bit(&mut self.inode_bitmap, idx);
        }
        ino
    }

    fn alloc_blocks(&mut self, count: u64) -> u64 {
        let start = self.next_data_block;
        self.next_data_block += count;
        for b in start..start + count {
            set_bit(&mut self.block_bitmap, b);
        }
        start
    }

    fn free_blocks(&self) -> u64 {
        let used: u64 = self.block_bitmap.iter().map(|b| b.count_ones() as u64).sum();
        self.total_blocks.saturating_sub(used.min(self.total_blocks))
    }

    fn free_inodes(&self) -> u32 {
        let used: u64 = self.inode_bitmap.iter().map(|b| b.count_ones() as u64).sum();
        (self.total_inodes as u64).saturating_sub(used) as u32
    }

    // ── Superblock ──────────────────────────────────────────────────────────

    fn write_superblock(&mut self) -> Result<()> {
        self.file.seek(SeekFrom::Start(EXT4_SUPERBLOCK_OFFSET))?;

        let now = now_unix_secs() as u32;

        let w16 = |buf: &mut [u8], off: usize, v: u16| {
            let mut tmp = [0u8; 2];
            LittleEndian::write_u16(&mut tmp, v);
            buf[off..off + 2].copy_from_slice(&tmp);
        };
        let w32 = |buf: &mut [u8], off: usize, v: u32| {
            let mut tmp = [0u8; 4];
            LittleEndian::write_u32(&mut tmp, v);
            buf[off..off + 4].copy_from_slice(&tmp);
        };

        let mut sb = vec![0u8; 1024];

        w32(&mut sb, 0x00, self.total_inodes);                      // s_inodes_count
        w32(&mut sb, 0x04, self.total_blocks as u32);                // s_blocks_count_lo
        w32(&mut sb, 0x08, 0);                                       // s_r_blocks_count_lo
        w32(&mut sb, 0x0C, self.free_blocks() as u32);               // s_free_blocks_count_lo
        w32(&mut sb, 0x10, self.free_inodes());                      // s_free_inodes_count_lo
        w32(&mut sb, 0x14, 0);                                       // s_first_data_block (=0 for 4K)
        w32(&mut sb, 0x18, 2);                                       // s_log_block_size (4096)
        w32(&mut sb, 0x1C, 2);                                       // s_log_cluster_size
        w32(&mut sb, 0x20, self.blocks_per_group);                   // s_blocks_per_group
        w32(&mut sb, 0x24, self.blocks_per_group);                   // s_clusters_per_group
        w32(&mut sb, 0x28, self.inodes_per_group);                   // s_inodes_per_group
        w32(&mut sb, 0x2C, 0);                                       // s_mtime
        w32(&mut sb, 0x30, now);                                     // s_wtime
        w16(&mut sb, 0x34, 0);                                       // s_mnt_count
        w16(&mut sb, 0x36, 0xFFFF);                                  // s_max_mnt_count
        w16(&mut sb, 0x38, EXT4_SUPER_MAGIC);                        // s_magic
        w16(&mut sb, 0x3A, 1);                                       // s_state (clean)
        w16(&mut sb, 0x3C, 1);                                       // s_errors (continue)
        w16(&mut sb, 0x3E, 0);                                       // s_minor_rev_level
        w32(&mut sb, 0x40, now);                                     // s_lastcheck
        w32(&mut sb, 0x44, 0);                                       // s_checkinterval
        w32(&mut sb, 0x48, 0);                                       // s_creator_os (Linux)
        w32(&mut sb, 0x4C, 1);                                       // s_rev_level (dynamic)
        w16(&mut sb, 0x50, 0);                                       // s_def_resuid
        w16(&mut sb, 0x52, 0);                                       // s_def_resgid

        // s_first_ino (rev_level >= 1)
        w32(&mut sb, 0x54, EXT4_FIRST_USER_INO);
        // s_inode_size
        w16(&mut sb, 0x58, self.inode_size);
        // s_block_group_nr
        w16(&mut sb, 0x5A, 0);

        // Feature flags
        w32(&mut sb, 0x5C,
            EXT4_FEATURE_COMPAT_DIR_PREALLOC
            | EXT4_FEATURE_COMPAT_EXT_ATTR);
        w32(&mut sb, 0x60,
            EXT4_FEATURE_INCOMPAT_FILETYPE | EXT4_FEATURE_INCOMPAT_EXTENTS);
        w32(&mut sb, 0x64,
            EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER
            | EXT4_FEATURE_RO_COMPAT_LARGE_FILE
            | EXT4_FEATURE_RO_COMPAT_DIR_NLINK
            | EXT4_FEATURE_RO_COMPAT_EXTRA_ISIZE);

        // UUID (16 bytes at 0x68)
        let uuid = uuid::Uuid::new_v4();
        sb[0x68..0x68 + 16].copy_from_slice(uuid.as_bytes());

        // Volume name (16 bytes at 0x78) — skip, leave zero

        // s_desc_size = 0 means default 32 bytes (no 64bit, no metadata_csum)
        w16(&mut sb, 0xFE, 0);

        // Extended fields (after the 256-byte superblock base)
        w16(&mut sb, 0x15C, 28);                                     // s_min_extra_isize
        w16(&mut sb, 0x15E, 28);                                     // s_want_extra_isize

        // blocks_count_hi
        w32(&mut sb, 0x150, (self.total_blocks >> 32) as u32);       // s_blocks_count_hi

        // Write full block (sb fills from offset 0 to block_size, written at byte 1024)
        self.file.write_all(&sb)?;
        Ok(())
    }

    // ── Group descriptors ───────────────────────────────────────────────────

    fn write_group_descriptors(&mut self) -> Result<()> {
        let gdt_offset = 1 * self.block_size;
        self.file.seek(SeekFrom::Start(gdt_offset))?;

        let fb = self.free_blocks() as u16;
        let fi = self.free_inodes() as u16;

        for g in 0..self.num_groups {
            let base_block = g as u64 * self.blocks_per_group as u64;

            let mut gd = [0u8; 32];

            let w16 = |buf: &mut [u8], off: usize, v: u16| {
                let mut tmp = [0u8; 2];
                LittleEndian::write_u16(&mut tmp, v);
                buf[off..off + 2].copy_from_slice(&tmp);
            };
            let w32 = |buf: &mut [u8], off: usize, v: u32| {
                let mut tmp = [0u8; 4];
                LittleEndian::write_u32(&mut tmp, v);
                buf[off..off + 4].copy_from_slice(&tmp);
            };

            w32(&mut gd, 0x00, (base_block + 2) as u32);            // bg_block_bitmap_lo
            w32(&mut gd, 0x04, (base_block + 3) as u32);            // bg_inode_bitmap_lo
            w32(&mut gd, 0x08, (base_block + 4) as u32);            // bg_inode_table_lo
            w16(&mut gd, 0x0C, fb);                                  // bg_free_blocks_count_lo
            w16(&mut gd, 0x0E, fi);                                  // bg_free_inodes_count_lo
            w16(&mut gd, 0x10, self.dir_count);                      // bg_used_dirs_count_lo
            w16(&mut gd, 0x12, 0);                                   // bg_flags

            self.file.write_all(&gd)?;
            // Pad remaining descriptor area to block boundary
            let pad = self.block_size as usize - gd.len();
            if pad > 0 && g == self.num_groups - 1 {
                // Only need to pad after the last descriptor
            }
        }

        // Pad the entire group descriptor table to one block
        let written = self.num_groups as u64 * 32;
        let remaining = self.block_size - (written % self.block_size);
        if remaining < self.block_size {
            let pad = vec![0u8; remaining as usize];
            self.file.write_all(&pad)?;
        }

        Ok(())
    }

    // ── Inode serialization ────────────────────────────────────────────────

    fn write_inode(&mut self, inode_num: u32, data: &[u8; 256]) -> Result<()> {
        if inode_num == 0 {
            return Ok(());
        }
        let idx = (inode_num - 1) as u64;
        let pos = 4 * self.block_size + idx * self.inode_size as u64;
        self.file.seek(SeekFrom::Start(pos))?;
        self.file.write_all(data)?;
        Ok(())
    }

    fn make_inode(
        &self,
        mode: u16,
        uid: u32,
        gid: u32,
        size: u64,
        flags: u32,
        blocks_512: u32,
        i_block: &[u8; 60],
        mtime: u64,
        links_count: u16,
    ) -> [u8; 256] {
        let mut inode = [0u8; 256];
        let t = mtime as u32;

        let mut tmp2 = [0u8; 2];
        let mut tmp4 = [0u8; 4];

        LittleEndian::write_u16(&mut tmp2, mode);
        inode[0x00..0x02].copy_from_slice(&tmp2);

        LittleEndian::write_u16(&mut tmp2, uid as u16);
        inode[0x02..0x04].copy_from_slice(&tmp2);

        LittleEndian::write_u32(&mut tmp4, size as u32);
        inode[0x04..0x08].copy_from_slice(&tmp4);

        LittleEndian::write_u32(&mut tmp4, t);
        inode[0x08..0x0C].copy_from_slice(&tmp4); // atime
        inode[0x0C..0x10].copy_from_slice(&tmp4); // ctime
        inode[0x10..0x14].copy_from_slice(&tmp4); // mtime
        // dtime = 0

        LittleEndian::write_u16(&mut tmp2, gid as u16);
        inode[0x18..0x1A].copy_from_slice(&tmp2);

        LittleEndian::write_u16(&mut tmp2, links_count);
        inode[0x1A..0x1C].copy_from_slice(&tmp2);

        LittleEndian::write_u32(&mut tmp4, blocks_512);
        inode[0x1C..0x20].copy_from_slice(&tmp4);

        LittleEndian::write_u32(&mut tmp4, flags);
        inode[0x20..0x24].copy_from_slice(&tmp4);

        // i_block
        inode[0x28..0x28 + 60].copy_from_slice(i_block);

        LittleEndian::write_u32(&mut tmp4, (size >> 32) as u32);
        inode[0x6C..0x70].copy_from_slice(&tmp4); // i_size_hi

        // Extra isize field (offset 0x80 = 128)
        LittleEndian::write_u16(&mut tmp2, 28); // i_extra_isize
        inode[0x80..0x82].copy_from_slice(&tmp2);

        LittleEndian::write_u32(&mut tmp4, t);
        inode[0x90..0x94].copy_from_slice(&tmp4); // i_crtime

        inode
    }

    // ── Extent tree ────────────────────────────────────────────────────────

    fn extent_root(entries: &[(u32, u16, u64)]) -> [u8; 60] {
        let mut buf = [0u8; 60];
        let mut tmp2 = [0u8; 2];
        let mut tmp4 = [0u8; 4];

        // Extent header (12 bytes)
        LittleEndian::write_u16(&mut tmp2, EXT4_EXTENT_MAGIC);
        buf[0..2].copy_from_slice(&tmp2);
        LittleEndian::write_u16(&mut tmp2, entries.len() as u16);
        buf[2..4].copy_from_slice(&tmp2);
        LittleEndian::write_u16(&mut tmp2, 4); // max entries
        buf[4..6].copy_from_slice(&tmp2);
        // depth = 0 (already zero), generation = 0 (already zero)

        for (i, &(block, len, start)) in entries.iter().enumerate() {
            let off = 12 + i * 12;
            LittleEndian::write_u32(&mut tmp4, block);
            buf[off..off + 4].copy_from_slice(&tmp4);
            LittleEndian::write_u16(&mut tmp2, len);
            buf[off + 4..off + 6].copy_from_slice(&tmp2);
            LittleEndian::write_u16(&mut tmp2, (start >> 32) as u16);
            buf[off + 6..off + 8].copy_from_slice(&tmp2);
            LittleEndian::write_u32(&mut tmp4, start as u32);
            buf[off + 8..off + 12].copy_from_slice(&tmp4);
        }

        buf
    }

    // ── Directory blocks ───────────────────────────────────────────────────

    fn make_dir_block(entries: &[(u32, &[u8], u8)], block_size: u64) -> Vec<u8> {
        let mut block = vec![0u8; block_size as usize];
        let mut off = 0usize;

        for (i, &(inode, name, file_type)) in entries.iter().enumerate() {
            let raw_len = 8 + name.len();
            let padded = ((raw_len + 3) / 4) * 4;
            let is_last = i == entries.len() - 1;
            let rec_len = if is_last {
                block_size as usize - off
            } else {
                padded
            };

            let mut tmp2 = [0u8; 2];
            let mut tmp4 = [0u8; 4];

            LittleEndian::write_u32(&mut tmp4, inode);
            block[off..off + 4].copy_from_slice(&tmp4);
            LittleEndian::write_u16(&mut tmp2, rec_len as u16);
            block[off + 4..off + 6].copy_from_slice(&tmp2);
            block[off + 6] = name.len() as u8;
            block[off + 7] = file_type;
            block[off + 8..off + 8 + name.len()].copy_from_slice(name);

            off += rec_len;
            if off >= block_size as usize {
                break;
            }
        }

        block
    }

    // ── Build from source directory ────────────────────────────────────────

    /// Main entry point: walk src_dir and write a complete ext4 image.
    pub fn build_from_dir<P: AsRef<Path>>(&mut self, src_dir: P) -> Result<()> {
        let src = src_dir.as_ref();
        if !src.is_dir() {
            anyhow::bail!("Source is not a directory: {}", src.display());
        }

        // Phase 1 — walk source tree, assign inodes, get path→inode map
        println!("Scanning source directory...");
        let (root_children, mut all_entries, path_to_inode) = self.walk_tree(src)?;

        // Phase 2 — write everything
        println!("Writing filesystem...");

        // 2a. Create root inode entry and add to all_entries
        let root_inode_num = EXT4_ROOT_INODE;
        set_bit(&mut self.inode_bitmap, (root_inode_num - 1) as u64);

        let root_meta = fs::symlink_metadata(src)
            .with_context(|| format!("Failed to read metadata for {}", src.display()))?;

        let root_mode = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                root_meta.mode() as u16
            }
            #[cfg(not(unix))]
            { 0o40755 }
        };
        let root_uid = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                root_meta.uid()
            }
            #[cfg(not(unix))]
            { 0 }
        };
        let root_gid = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                root_meta.gid()
            }
            #[cfg(not(unix))]
            { 0 }
        };
        let root_mtime = root_meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        all_entries.insert(root_inode_num, FileEntry {
            inode: root_inode_num,
            file_type: FileType::Directory,
            size: 0,
            mode: root_mode,
            uid: root_uid,
            gid: root_gid,
            mtime: root_mtime,
            symlink_target: None,
            source_path: None,
            rel_path: PathBuf::from(""),
        });

        // 2b. Write all non-directory entries first (files + symlinks)
        for (&ino, entry) in &all_entries {
            match entry.file_type {
                FileType::RegularFile => self.write_file_inode(ino, entry)?,
                FileType::Symlink => self.write_symlink_inode(ino, entry)?,
                _ => {}
            }
        }

        // 2c. BFS: write directory blocks and directory inodes
        let mut queue: VecDeque<(PathBuf, u32, Vec<FileNode>)> = VecDeque::new();
        queue.push_back((PathBuf::from(""), root_inode_num, root_children));

        while let Some((parent_rel, parent_ino, children)) = queue.pop_front() {
            self.dir_count += 1;
            // Count subdirectories for links_count
            let subdir_count = children.iter()
                .filter(|c| c.file_type == FileType::Directory)
                .count() as u16;

            // Build directory entries: ., .., children
            let mut entries = Vec::new();

            // Compute parent's parent inode for ..
            let parent_parent_ino = if parent_rel.as_os_str().is_empty() {
                root_inode_num
            } else {
                let grandparent_rel = parent_rel.parent()
                    .map(|p| {
                        if p.as_os_str().is_empty() {
                            PathBuf::from("")
                        } else {
                            p.to_path_buf()
                        }
                    })
                    .unwrap_or_else(|| PathBuf::from(""));
                path_to_inode.get(&grandparent_rel).copied().unwrap_or(root_inode_num)
            };

            entries.push((parent_ino, b"." as &[u8], EXT4_FT_DIR));
            entries.push((parent_parent_ino, b".." as &[u8], EXT4_FT_DIR));

            for child in &children {
                let ft = match child.file_type {
                    FileType::RegularFile => EXT4_FT_REG_FILE,
                    FileType::Directory => EXT4_FT_DIR,
                    FileType::Symlink => EXT4_FT_SYMLINK,
                    _ => EXT4_FT_REG_FILE,
                };
                entries.push((child.inode, child.name.as_bytes(), ft));
            }

            // Allocate block(s) for the directory and write data
            let dir_data = Self::make_dir_block(&entries, self.block_size);
            let nblocks = (dir_data.len() as u64 + self.block_size - 1) / self.block_size;
            let start_block = self.alloc_blocks(nblocks);

            let file_off = start_block * self.block_size;
            self.file.seek(SeekFrom::Start(file_off))?;
            self.file.write_all(&dir_data)?;

            // Write/update inode for this directory
            let i_blocks_512 = (nblocks * (self.block_size / 512)) as u32;
            let extent = [(0u32, nblocks as u16, start_block)];
            let ib = Self::extent_root(&extent);
            let entry = all_entries.get(&parent_ino)
                .expect("directory entry missing");
            let mode = entry.mode | 0o040000; // S_IFDIR

            let links_count = 2 + subdir_count;
            let inode_data = self.make_inode(
                mode,
                entry.uid,
                entry.gid,
                dir_data.len() as u64,
                EXT4_EXTENTS_FL,
                i_blocks_512,
                &ib,
                entry.mtime,
                links_count,
            );
            self.write_inode(parent_ino, &inode_data)?;

            // Enqueue child directories
            for child in children {
                if child.file_type == FileType::Directory {
                    let child_rel = if parent_rel.as_os_str().is_empty() {
                        PathBuf::from(&child.name)
                    } else {
                        parent_rel.join(&child.name)
                    };
                    queue.push_back((child_rel, child.inode, child.children));
                }
            }
        }

        // Finalize
        self.finalize()?;
        Ok(())
    }

    fn write_file_inode(&mut self, ino: u32, entry: &FileEntry) -> Result<()> {
        let size = entry.size;
        let nblocks = if size == 0 {
            1u64 // at least one block so extent isn't empty
        } else {
            (size + self.block_size - 1) / self.block_size
        };
        let start_block = self.alloc_blocks(nblocks);

        // Write file data (zero-filled for empty files)
        let file_off = start_block * self.block_size;
        self.file.seek(SeekFrom::Start(file_off))?;

        // Read source file
        let src_path = entry.source_path.as_ref()
            .expect("file entry missing source path");
        if size > 0 {
            let mut src_f = fs::File::open(src_path)
                .with_context(|| format!("Failed to open {}", src_path.display()))?;
            let mut written = 0u64;
            let mut buf = vec![0u8; self.block_size as usize];
            loop {
                if written >= size {
                    break;
                }
                let to_read = (size - written).min(self.block_size);
                let n = src_f.read(&mut buf[..to_read as usize])?;
                if n == 0 {
                    break;
                }
                self.file.write_all(&buf[..n])?;
                // Pad to block boundary
                if (n as u64) < self.block_size {
                    let zeros = vec![0u8; self.block_size as usize - n];
                    self.file.write_all(&zeros)?;
                }
                written += n as u64;
            }
        } else {
            let zeros = vec![0u8; self.block_size as usize];
            self.file.write_all(&zeros)?;
        }

        let extent = [(0u32, nblocks as u16, start_block)];
        let ib = Self::extent_root(&extent);
        let i_blocks_512 = (nblocks * (self.block_size / 512)) as u32;
        let mode = entry.mode | 0o100000; // S_IFREG

        let inode_data = self.make_inode(
            mode,
            entry.uid,
            entry.gid,
            size,
            EXT4_EXTENTS_FL,
            i_blocks_512,
            &ib,
            entry.mtime,
            1,
        );
        self.write_inode(ino, &inode_data)?;
        Ok(())
    }

    fn write_symlink_inode(&mut self, ino: u32, entry: &FileEntry) -> Result<()> {
        let target = entry.symlink_target.as_deref().unwrap_or(&[]);
        let target_len = target.len();

        let (ib, size, i_blocks_512, flags) = if target_len <= 60 {
            // Fast symlink: inline in i_block (no extents)
            let mut ib = [0u8; 60];
            ib[..target_len].copy_from_slice(target);
            (ib, target_len as u64, 0u32, 0u32)
        } else {
            // Slow symlink: stored in data blocks
            let nblocks = (target_len as u64 + self.block_size - 1) / self.block_size;
            let start_block = self.alloc_blocks(nblocks);
            let file_off = start_block * self.block_size;
            self.file.seek(SeekFrom::Start(file_off))?;
            self.file.write_all(target)?;

            let extent = [(0u32, nblocks as u16, start_block)];
            let ib = Self::extent_root(&extent);
            let i_blocks_512 = (nblocks * (self.block_size / 512)) as u32;
            (ib, target_len as u64, i_blocks_512, EXT4_EXTENTS_FL)
        };

        let mode = entry.mode | 0o120000; // S_IFLNK
        let inode_data = self.make_inode(
            mode,
            entry.uid,
            entry.gid,
            size,
            flags,
            i_blocks_512,
            &ib,
            entry.mtime,
            1,
        );
        self.write_inode(ino, &inode_data)?;
        Ok(())
    }

    // ── Directory walker ───────────────────────────────────────────────────

    fn walk_tree(&mut self, src: &Path)
        -> Result<(Vec<FileNode>, HashMap<u32, FileEntry>, HashMap<PathBuf, u32>)>
    {
        #[derive(Debug, Clone)]
        struct RawEntry {
            path: PathBuf,     // absolute path on host
            relative: PathBuf, // relative to src_dir
            name: String,
            file_type: FileType,
            size: u64,
            mode: u16,
            uid: u32,
            gid: u32,
            mtime: u64,
            symlink_target: Option<Vec<u8>>,
            parent_rel: Option<PathBuf>,
        }

        let mut raws = Vec::new();

        // Walk the directory recursively
        let mut stack = vec![src.to_path_buf()];
        while let Some(dir_path) = stack.pop() {
            let dir_rd = match fs::read_dir(&dir_path) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Warning: cannot read {}: {}", dir_path.display(), e);
                    continue;
                }
            };

            for entry in dir_rd {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                let path = entry.path();
                let name = entry
                    .file_name()
                    .to_string_lossy()
                    .to_string();

                let metadata = match fs::symlink_metadata(&path) {
                    Ok(m) => m,
                    Err(_) => continue,
                };

                let file_type = if metadata.file_type().is_symlink() {
                    FileType::Symlink
                } else if metadata.is_dir() {
                    FileType::Directory
                } else if metadata.file_type().is_socket() {
                    FileType::Socket
                } else if metadata.file_type().is_char_device() {
                    FileType::CharDevice
                } else if metadata.file_type().is_block_device() {
                    FileType::BlockDevice
                } else if metadata.file_type().is_fifo() {
                    FileType::Fifo
                } else {
                    FileType::RegularFile
                };

                let relative = path
                    .strip_prefix(src)
                    .unwrap_or(&path)
                    .to_path_buf();
                let parent_rel = relative.parent().map(|p| p.to_path_buf());

                let mode = {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        metadata.mode() as u16
                    }
                    #[cfg(not(unix))]
                    {
                        if metadata.is_dir() { 0o40755 }
                        else { 0o100644 }
                    }
                };

                let uid = {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        metadata.uid()
                    }
                    #[cfg(not(unix))]
                    { 0 }
                };
                let gid = {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        metadata.gid()
                    }
                    #[cfg(not(unix))]
                    { 0 }
                };
                let mtime = metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);

                let symlink_target = if file_type == FileType::Symlink {
                    fs::read_link(&path)
                        .ok()
                        .map(|t| t.to_string_lossy().to_string().into_bytes())
                } else {
                    None
                };

                let size = if metadata.is_dir() {
                    0u64
                } else if metadata.is_symlink() {
                    metadata.len()
                } else {
                    metadata.len()
                };

                raws.push(RawEntry {
                    path,
                    relative,
                    name,
                    file_type,
                    size,
                    mode,
                    uid,
                    gid,
                    mtime,
                    symlink_target,
                    parent_rel,
                });

                // If directory, push for recursion
                if file_type == FileType::Directory {
                    stack.push(entry.path());
                }
            }
        }

        // Sort by depth (parents before children)
        raws.sort_by(|a, b| {
            a.relative
                .components()
                .count()
                .cmp(&b.relative.components().count())
        });

        // Assign inodes, build entry map and path→inode map
        let mut entries: HashMap<u32, FileEntry> = HashMap::new();
        let mut path_to_inode: HashMap<PathBuf, u32> = HashMap::new();

        for raw in &raws {
            let ino = self.alloc_inode();
            path_to_inode.insert(raw.relative.clone(), ino);

            let source_path = match raw.file_type {
                FileType::RegularFile | FileType::Symlink => Some(raw.path.clone()),
                _ => None,
            };

            let fe = FileEntry {
                inode: ino,
                file_type: raw.file_type,
                size: raw.size,
                mode: raw.mode,
                uid: raw.uid,
                gid: raw.gid,
                mtime: raw.mtime,
                symlink_target: raw.symlink_target.clone(),
                source_path,
                rel_path: raw.relative.clone(),
            };
            entries.insert(ino, fe);
        }

        // Build tree recursively
        fn build_subtree(
            raws: &[RawEntry],
            path_to_inode: &HashMap<PathBuf, u32>,
            parent_rel: Option<&Path>,
        ) -> Vec<FileNode> {
            let mut nodes = Vec::new();
            for raw in raws {
                let raw_parent = raw.parent_rel.as_deref();
                // Root-level items have parent_rel = Some("") — match against that too
                let matches = if parent_rel.is_none() {
                    raw_parent.is_none() || raw_parent == Some(Path::new(""))
                } else {
                    raw_parent == parent_rel
                };
                if !matches {
                    continue;
                }
                let ino = path_to_inode[&raw.relative];
                let children = build_subtree(raws, path_to_inode, Some(&raw.relative));
                nodes.push(FileNode {
                    name: raw.name.clone(),
                    inode: ino,
                    file_type: raw.file_type,
                    children,
                });
            }
            nodes
        }

        let root_children = build_subtree(&raws, &path_to_inode, None);

        Ok((root_children, entries, path_to_inode))
    }

    // ── Finalize ────────────────────────────────────────────────────────────

    fn finalize(&mut self) -> Result<()> {
        // Write block bitmap at block 2 (padded to block size, trailing bits set to 1)
        let mut bm = self.block_bitmap.clone();
        bm.resize(self.block_size as usize, 0xFF);
        // Also mark bits beyond total_blocks as used (within the group)
        for b in self.total_blocks..self.blocks_per_group as u64 {
            set_bit(&mut bm, b);
        }
        let bm_off = 2 * self.block_size;
        self.file.seek(SeekFrom::Start(bm_off))?;
        self.file.write_all(&bm)?;

        // Write inode bitmap at block 3 (padded to block size, trailing bits set to 1)
        let mut im = self.inode_bitmap.clone();
        im.resize(self.block_size as usize, 0xFF);
        let im_off = 3 * self.block_size;
        self.file.seek(SeekFrom::Start(im_off))?;
        self.file.write_all(&im)?;

        // Rewrite group descriptors with final free counts
        self.write_group_descriptors()?;

        // Rewrite superblock with final free counts
        self.write_superblock()?;
        Ok(())
    }
}

// ── Bitmap helper ─────────────────────────────────────────────────────────────
fn set_bit(bm: &mut [u8], bit: u64) {
    let byte = (bit / 8) as usize;
    let bit_in_byte = (bit % 8) as u8;
    if byte < bm.len() {
        bm[byte] |= 1 << bit_in_byte;
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── Public entry point ────────────────────────────────────────────────────────
pub fn build_image<P: AsRef<Path>>(src_dir: P, output: P, size_mb: u64) -> Result<()> {
    let total_size = (size_mb as u64).max(64) * 1024 * 1024;
    let mut builder = Builder::create(output, total_size)?;
    builder.build_from_dir(src_dir)?;
    println!("Done.");
    Ok(())
}
