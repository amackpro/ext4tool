// Sub-modules
mod allocation;
mod constants;
mod directory;
mod extent;
mod inode;
mod metadata;
mod types;
mod xattr;
mod xattr_multi;

// Internal imports
use allocation::set_bit;
use constants::*;
use directory::{make_dir_block, walk_directory_tree};
use extent::build_extent_root;
use types::{FileEntry, FileNode};
use xattr_multi::build_security_xattrs;

// External imports
use crate::ext4::{FileType, EXT4_ROOT_INODE};
use anyhow::{Context, Result};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::time::UNIX_EPOCH;

/// Main builder for creating ext4 filesystems
pub struct Builder {
    block_size: u64,
    inode_size: u16,
    blocks_per_group: u32,
    inodes_per_group: u32,
    num_groups: u32,
    total_blocks: u64,
    total_inodes: u32,

    itable_blocks: u64,
    reserved_blocks: u64,

    next_inode: u32,
    next_data_block: u64,
    block_bitmap: Vec<u8>,
    inode_bitmap: Vec<u8>,
    dir_count_group: Vec<u16>,

    file: fs::File,
}

impl Builder {
    /// Create a new ext4 filesystem with the specified size
    pub fn create<P: AsRef<Path>>(path: P, total_size: u64, block_size: u64, reserved_pct: u32) -> Result<Self> {
        let block_size = if block_size == 0 { DEFAULT_BLOCK_SIZE } else { block_size };
        assert!(block_size == 1024 || block_size == 2048 || block_size == 4096,
            "block_size must be 1024, 2048, or 4096");

        let total_blocks = total_size / block_size;
        if total_blocks < BLOCKS_PER_GROUP as u64 {
            anyhow::bail!("Image too small for one block group (need at least {} bytes)",
                BLOCKS_PER_GROUP as u64 * block_size);
        }
        let num_groups = ((total_blocks + BLOCKS_PER_GROUP as u64 - 1) / BLOCKS_PER_GROUP as u64) as u32;
        let num_groups = num_groups.max(1);
        let total_inodes = num_groups * INODES_PER_GROUP;

        let itable_blocks = ((INODES_PER_GROUP as u64 * INODE_SIZE as u64) + block_size - 1) / block_size;

        // Initialize block bitmap
        let bm_bytes = (total_blocks + 7) / 8;
        let mut block_bitmap = vec![0u8; bm_bytes as usize];
        for g in 0..num_groups {
            let base = g as u64 * BLOCKS_PER_GROUP as u64;
            let meta_start: u64 = if has_sparse_super(g) { 0 } else { 2 };
            let meta_end = 4 + itable_blocks;
            for b in meta_start..meta_end {
                let block = base + b;
                if block < total_blocks {
                    set_bit(&mut block_bitmap, block);
                }
            }
        }

        // Initialize inode bitmap
        let im_bytes = (total_inodes as u64 + 7) / 8;
        let mut inode_bitmap = vec![0u8; im_bytes as usize];
        for i in 0..12u64 {
            set_bit(&mut inode_bitmap, i);
        }

        let file = fs::File::create(path.as_ref())
            .with_context(|| format!("Failed to create {}", path.as_ref().display()))?;
        file.set_len(total_size)
            .with_context(|| "Failed to allocate output file")?;

        let first_data = 4 + itable_blocks;
        let reserved_blocks = total_blocks * reserved_pct as u64 / 100;

        let mut b = Builder {
            block_size,
            inode_size: INODE_SIZE,
            blocks_per_group: BLOCKS_PER_GROUP,
            inodes_per_group: INODES_PER_GROUP,
            num_groups,
            total_blocks,
            total_inodes,
            itable_blocks,
            reserved_blocks,
            next_inode: EXT4_FIRST_USER_INO,
            next_data_block: first_data,
            block_bitmap,
            inode_bitmap,
            dir_count_group: vec![0u16; num_groups as usize],
            file,
        };

        b.write_superblock()?;
        b.write_group_descriptors()?;
        Ok(b)
    }

    /// Build an ext4 filesystem from a source directory
    pub fn build_from_dir<P: AsRef<Path>>(
        &mut self,
        src_dir: P,
        fs_config: Option<&HashMap<String, crate::config::FsConfigEntry>>,
        file_contexts: Option<&Vec<(String, String)>>,
        fs_contexts_prefix: Option<&str>,
    ) -> Result<()> {
        let src = src_dir.as_ref();
        if !src.is_dir() {
            anyhow::bail!("Source is not a directory: {}", src.display());
        }

        println!("Scanning source directory...");
        let (root_children, mut all_entries, path_to_inode) =
            walk_directory_tree(src, &mut || self.alloc_inode())?;

        // Apply fs_config
        let mut fs_config_applied = 0;
        let mut capabilities_applied = 0;
        if let Some(config) = fs_config {
            // Try to determine the prefix from fs_contexts_prefix or by checking config keys
            let prefix = fs_contexts_prefix.map(|s| s.trim_matches('/').to_string());

            for entry in all_entries.values_mut() {
                let rel_str = entry.rel_path.to_string_lossy();
                let rel_path = rel_str.replace('\\', "/");

                // Try multiple path variants for matching
                let paths_to_try: Vec<String> = if let Some(ref pfx) = prefix {
                    vec![
                        rel_path.clone(),                    // as-is: "bin/cnd"
                        format!("/{}", rel_path),            // with leading slash: "/bin/cnd"
                        format!("{}/{}", pfx, rel_path),     // with prefix: "vendor/bin/cnd"
                        format!("/{}/{}", pfx, rel_path),    // with both: "/vendor/bin/cnd"
                    ]
                } else {
                    vec![
                        rel_path.clone(),
                        format!("/{}", rel_path),
                    ]
                };

                let matched = paths_to_try.iter()
                    .find_map(|path| config.get(path));

                if let Some(cfg) = matched {
                    entry.uid = cfg.uid;
                    entry.gid = cfg.gid;
                    entry.mode = cfg.mode;
                    if cfg.capabilities.is_some() {
                        entry.capabilities = cfg.capabilities.clone();
                        capabilities_applied += 1;
                    }
                    fs_config_applied += 1;
                }
            }
            if fs_config_applied > 0 {
                println!("Applied fs_config to {} entries ({} with capabilities)",
                    fs_config_applied, capabilities_applied);
            }
        }

        // Pre-compile file_contexts patterns
        let compiled_patterns: Option<Vec<(regex::Regex, String)>> = if let Some(contexts) = file_contexts {
            Some(contexts
                .iter()
                .filter_map(|(pattern, context)| {
                    regex::Regex::new(pattern)
                        .ok()
                        .map(|re| (re, context.clone()))
                })
                .collect())
        } else {
            None
        };

        // Apply file_contexts
        // Android SELinux uses LAST matching pattern, not first!
        let mut selinux_applied = 0;
        if let Some(ref patterns) = compiled_patterns {
            let prefix = fs_contexts_prefix.map(|s| format!("/{}", s.trim_start_matches('/'))).unwrap_or_default();

            for entry in all_entries.values_mut() {
                let rel_str = entry.rel_path.to_string_lossy();
                let rel_path = rel_str.replace('\\', "/");
                let path_str = if !prefix.is_empty() {
                    format!("{}/{}", prefix, rel_path)
                } else {
                    format!("/{}", rel_path)
                };

                // Find ALL matching patterns and use the LAST one (most specific)
                let mut matched_context: Option<String> = None;
                for (re, context) in patterns {
                    if re.is_match(&path_str) {
                        matched_context = Some(context.clone());
                        // Don't break - keep looking for more specific matches
                    }
                }

                if let Some(ctx) = matched_context {
                    entry.selinux_context = Some(ctx);
                    selinux_applied += 1;
                }
            }
            if selinux_applied > 0 {
                println!("Applied file_contexts to {} entries", selinux_applied);
            }
        }

        println!("Writing filesystem...");

        // Create root inode
        let root_inode_num = EXT4_ROOT_INODE;
        set_bit(&mut self.inode_bitmap, (root_inode_num - 1) as u64);

        let root_meta = fs::symlink_metadata(src)
            .with_context(|| format!("Failed to read metadata for {}", src.display()))?;

        let (root_mode, root_uid, root_gid, root_mtime) = self.get_file_metadata(&root_meta);

        let mut root_uid_final = root_uid;
        let mut root_gid_final = root_gid;
        let mut root_mode_final = root_mode;
        if let Some(config) = fs_config {
            // Try multiple root path variants
            let pfx_str = fs_contexts_prefix.map(|s| s.trim_matches('/').to_string());
            let with_slash = pfx_str.as_ref().map(|s| format!("/{}", s));

            let root_paths: Vec<&str> = if let Some(ref pfx) = pfx_str {
                vec!["/", "", pfx.as_str(), with_slash.as_ref().unwrap().as_str()]
            } else {
                vec!["/", ""]
            };

            let matched = root_paths.iter()
                .find_map(|path| config.get(*path));

            if let Some(cfg) = matched {
                root_uid_final = cfg.uid;
                root_gid_final = cfg.gid;
                root_mode_final = cfg.mode;
            }
        }

        let mut root_selinux: Option<String> = None;
        if let Some(ref patterns) = compiled_patterns {
            let prefix = fs_contexts_prefix.map(|s| format!("/{}", s.trim_start_matches('/'))).unwrap_or_else(|| "/".to_string());
            let root_path = if prefix == "/" { "/" } else { &prefix };

            // Use LAST matching pattern (most specific)
            for (re, context) in patterns {
                if re.is_match(root_path) {
                    root_selinux = Some(context.clone());
                    // Don't break - keep looking
                }
            }
        }

        all_entries.insert(root_inode_num, FileEntry {
            inode: root_inode_num,
            file_type: FileType::Directory,
            size: 0,
            mode: root_mode_final,
            uid: root_uid_final,
            gid: root_gid_final,
            mtime: root_mtime,
            symlink_target: None,
            source_path: None,
            rel_path: PathBuf::from(""),
            selinux_context: root_selinux,
            capabilities: None,
        });

        // Write all non-directory entries
        for (&ino, entry) in &all_entries {
            match entry.file_type {
                FileType::RegularFile => self.write_file_inode(ino, entry)?,
                FileType::Symlink => self.write_symlink_inode(ino, entry)?,
                _ => {}
            }
        }

        // BFS: write directory blocks and inodes
        let mut queue: VecDeque<(PathBuf, u32, Vec<FileNode>)> = VecDeque::new();
        queue.push_back((PathBuf::from(""), root_inode_num, root_children));

        while let Some((parent_rel, parent_ino, children)) = queue.pop_front() {
            self.write_directory(parent_ino, &parent_rel, &children, &all_entries, &path_to_inode, root_inode_num)?;

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

        self.finalize()?;
        Ok(())
    }

    /// Write a directory's blocks and inode
    fn write_directory(
        &mut self,
        parent_ino: u32,
        parent_rel: &Path,
        children: &[FileNode],
        all_entries: &HashMap<u32, FileEntry>,
        path_to_inode: &HashMap<PathBuf, u32>,
        root_inode_num: u32,
    ) -> Result<()> {
        let dir_group = ((parent_ino - 1) / self.inodes_per_group) as usize;
        if dir_group < self.dir_count_group.len() {
            self.dir_count_group[dir_group] += 1;
        }

        let subdir_count = children.iter()
            .filter(|c| c.file_type == FileType::Directory)
            .count() as u16;

        let mut entries: Vec<(u32, Vec<u8>, u8)> = Vec::new();

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

        entries.push((parent_ino, b".".to_vec(), EXT4_FT_DIR));
        entries.push((parent_parent_ino, b"..".to_vec(), EXT4_FT_DIR));

        for child in children {
            let ft = match child.file_type {
                FileType::RegularFile => EXT4_FT_REG_FILE,
                FileType::Directory => EXT4_FT_DIR,
                FileType::Symlink => EXT4_FT_SYMLINK,
                _ => EXT4_FT_REG_FILE,
            };
            entries.push((child.inode, child.name.as_bytes().to_vec(), ft));
        }

        let dir_data = make_dir_block(&entries, self.block_size);
        let nblocks = (dir_data.len() as u64 + self.block_size - 1) / self.block_size;
        let segments = self.alloc_blocks(nblocks);

        // Write directory data
        let mut written = 0usize;
        for &(seg_start, seg_nblocks) in &segments {
            let file_off = seg_start * self.block_size;
            self.file.seek(SeekFrom::Start(file_off))?;
            let end = (written + seg_nblocks as usize * self.block_size as usize).min(dir_data.len());
            self.file.write_all(&dir_data[written..end])?;
            written = end;
        }

        let i_blocks_512 = (nblocks * (self.block_size / 512)) as u32;
        let extent_entries: Vec<(u32, u16, u64)> = {
            let mut logical = 0u32;
            segments.iter().map(|&(start, count)| {
                let entry = (logical, count as u16, start);
                logical += count as u32;
                entry
            }).collect()
        };
        let ib = build_extent_root(&extent_entries)?;
        let entry = all_entries.get(&parent_ino).expect("directory entry missing");
        let mode = entry.mode | 0o040000;

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

        let xattr = build_security_xattrs(
            entry.selinux_context.as_deref(),
            entry.capabilities.as_deref()
        );
        self.write_inode_with_xattr(parent_ino, &inode_data, xattr.as_deref())?;

        Ok(())
    }

    /// Write a regular file's data and inode
    fn write_file_inode(&mut self, ino: u32, entry: &FileEntry) -> Result<()> {
        let size = entry.size;
        let nblocks = if size == 0 { 1u64 } else { (size + self.block_size - 1) / self.block_size };
        let segments = self.alloc_blocks(nblocks);

        if segments.len() > 4 {
            eprintln!("\n>>> File with {} extents:", segments.len());
            eprintln!("    Path: {}", entry.rel_path.display());
            eprintln!("    Size: {} bytes ({} blocks)", size, nblocks);
        }

        let src_path = entry.source_path.as_ref().expect("file entry missing source path");
        if size > 0 {
            let mut src_f = fs::File::open(src_path)
                .with_context(|| format!("Failed to open {}", src_path.display()))?;
            let mut written = 0u64;
            let mut buf = vec![0u8; self.block_size as usize];
            for &(seg_start, seg_nblocks) in &segments {
                let file_off = seg_start * self.block_size;
                self.file.seek(SeekFrom::Start(file_off))?;
                for _ in 0..seg_nblocks {
                    let to_read = (size - written).min(self.block_size) as usize;
                    if to_read == 0 {
                        break;
                    }
                    let n = src_f.read(&mut buf[..to_read])?;
                    if n == 0 {
                        break;
                    }
                    self.file.write_all(&buf[..n])?;
                    if n < self.block_size as usize {
                        let zeros = vec![0u8; self.block_size as usize - n];
                        self.file.write_all(&zeros)?;
                    }
                    written += n as u64;
                }
            }
        } else {
            for &(seg_start, seg_nblocks) in &segments {
                let file_off = seg_start * self.block_size;
                self.file.seek(SeekFrom::Start(file_off))?;
                for _ in 0..seg_nblocks {
                    let zeros = vec![0u8; self.block_size as usize];
                    self.file.write_all(&zeros)?;
                }
            }
        }

        let extent_entries: Vec<(u32, u16, u64)> = {
            let mut logical = 0u32;
            segments.iter().map(|&(start, count)| {
                let entry = (logical, count as u16, start);
                logical += count as u32;
                entry
            }).collect()
        };
        let ib = build_extent_root(&extent_entries)?;
        let i_blocks_512 = (nblocks * (self.block_size / 512)) as u32;
        let mode = entry.mode | 0o100000;

        let inode_data = self.make_inode(mode, entry.uid, entry.gid, size, EXT4_EXTENTS_FL, i_blocks_512, &ib, entry.mtime, 1);
        let xattr = build_security_xattrs(
            entry.selinux_context.as_deref(),
            entry.capabilities.as_deref()
        );
        self.write_inode_with_xattr(ino, &inode_data, xattr.as_deref())?;
        Ok(())
    }

    /// Write a symlink's data and inode
    fn write_symlink_inode(&mut self, ino: u32, entry: &FileEntry) -> Result<()> {
        let target = entry.symlink_target.as_deref().unwrap_or(&[]);
        let target_len = target.len();

        let (ib, size, i_blocks_512, flags) = if target_len <= 60 {
            let mut ib = [0u8; 60];
            ib[..target_len].copy_from_slice(target);
            (ib, target_len as u64, 0u32, 0u32)
        } else {
            let nblocks = (target_len as u64 + self.block_size - 1) / self.block_size;
            let segments = self.alloc_blocks(nblocks);

            let (seg_start, _) = segments[0];
            let file_off = seg_start * self.block_size;
            self.file.seek(SeekFrom::Start(file_off))?;
            self.file.write_all(target)?;

            let extent_entries: Vec<(u32, u16, u64)> = {
                let mut logical = 0u32;
                segments.iter().map(|&(start, count)| {
                    let entry = (logical, count as u16, start);
                    logical += count as u32;
                    entry
                }).collect()
            };
            let ib = build_extent_root(&extent_entries)?;
            let i_blocks_512 = (nblocks * (self.block_size / 512)) as u32;
            (ib, target_len as u64, i_blocks_512, EXT4_EXTENTS_FL)
        };

        let mode = entry.mode | 0o120000;
        let inode_data = self.make_inode(mode, entry.uid, entry.gid, size, flags, i_blocks_512, &ib, entry.mtime, 1);
        let xattr = build_security_xattrs(
            entry.selinux_context.as_deref(),
            entry.capabilities.as_deref()
        );
        self.write_inode_with_xattr(ino, &inode_data, xattr.as_deref())?;
        Ok(())
    }

    /// Extract file metadata (mode, uid, gid, mtime)
    fn get_file_metadata(&self, metadata: &fs::Metadata) -> (u16, u32, u32, u64) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let mode = metadata.mode() as u16;
            let uid = metadata.uid();
            let gid = metadata.gid();
            let mtime = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            (mode, uid, gid, mtime)
        }
        #[cfg(not(unix))]
        {
            let mode = 0o40755;
            let uid = 0;
            let gid = 0;
            let mtime = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            (mode, uid, gid, mtime)
        }
    }
}

/// Public entry point for building an ext4 image
pub fn build_image<P: AsRef<Path>>(
    src_dir: P,
    output: P,
    size_bytes: u64,
    block_size: u64,
    reserved_pct: u32,
    fs_config: Option<HashMap<String, crate::config::FsConfigEntry>>,
    file_contexts: Option<Vec<(String, String)>>,
    fs_contexts_prefix: Option<String>,
    sparse: bool,
) -> Result<()> {
    let final_output = output.as_ref().to_path_buf();
    let build_path: PathBuf = if sparse {
        output.as_ref().with_extension("img.raw.tmp")
    } else {
        final_output.clone()
    };

    let total_size = size_bytes.max(4096 * 32768);
    let mut builder = Builder::create(&build_path, total_size, block_size, reserved_pct)?;

    let config_ref = fs_config.as_ref();
    let contexts_ref = file_contexts.as_ref();
    builder.build_from_dir(src_dir, config_ref, contexts_ref, fs_contexts_prefix.as_deref())?;

    builder.file.flush()?;
    drop(builder);

    if sparse {
        println!("Converting to sparse image...");
        eprintln!("  debug: raw_path={:?}, out_path={:?}", &build_path, &final_output);
        eprintln!("  debug: raw_size={}", std::fs::metadata(&build_path).unwrap().len());
        crate::sparse::write_sparse_image(&build_path, &final_output)?;
        eprintln!("  debug: keeping temp file for inspection");
    }

    println!("Done.");
    Ok(())
}
