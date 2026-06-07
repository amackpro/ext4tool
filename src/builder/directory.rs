use super::types::{FileEntry, FileNode};
use crate::ext4::FileType;
use anyhow::Result;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use std::time::UNIX_EPOCH;

/// Create directory entry blocks
/// Packs directory entries into blocks with proper alignment
pub(super) fn make_dir_block(entries: &[(u32, Vec<u8>, u8)], block_size: u64) -> Vec<u8> {
    let bs = block_size as usize;
    let mut blocks = Vec::new();
    blocks.reserve(bs);
    blocks.resize(bs, 0u8);

    let mut off: usize = 0;
    let mut prev_off: usize = 0;
    let mut has_prev: bool = false;
    let total = entries.len();

    for idx in 0..total {
        let this_ino = entries[idx].0;
        let this_namelen = entries[idx].1.len();
        let this_file_type = entries[idx].2;
        let is_last = idx + 1 == total;

        let raw_len = 8 + this_namelen;
        let padded = ((raw_len + 3) / 4) * 4;

        if off + padded > bs && (has_prev || is_last) {
            if has_prev {
                let rem = (bs - prev_off % bs) as u16;
                blocks[prev_off + 4] = (rem & 0xff) as u8;
                blocks[prev_off + 5] = (rem >> 8) as u8;
            }
            blocks.resize(blocks.len() + bs, 0u8);
            off = blocks.len() - bs;
            has_prev = false;
        }

        let reclen: u16 = if is_last { (bs - off % bs) as u16 } else { padded as u16 };

        blocks[off] = (this_ino & 0xff) as u8;
        blocks[off + 1] = ((this_ino >> 8) & 0xff) as u8;
        blocks[off + 2] = ((this_ino >> 16) & 0xff) as u8;
        blocks[off + 3] = ((this_ino >> 24) & 0xff) as u8;

        blocks[off + 4] = (reclen & 0xff) as u8;
        blocks[off + 5] = (reclen >> 8) as u8;

        blocks[off + 6] = this_namelen as u8;
        blocks[off + 7] = this_file_type;

        let name = &entries[idx].1;
        let mut copy_off = 0usize;
        while copy_off < this_namelen {
            blocks[off + 8 + copy_off] = name[copy_off];
            copy_off += 1;
        }

        if !is_last {
            prev_off = off;
            has_prev = true;
        }
        off += reclen as usize;
    }

    blocks
}

/// Raw entry during directory tree walk
#[derive(Debug, Clone)]
struct RawEntry {
    path: PathBuf,
    relative: PathBuf,
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

/// Walk a directory tree and assign inodes
pub(super) fn walk_directory_tree<F>(
    src: &Path,
    alloc_inode: &mut F,
) -> Result<(Vec<FileNode>, HashMap<u32, FileEntry>, HashMap<PathBuf, u32>)>
where
    F: FnMut() -> u32,
{
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
            let name = entry.file_name().to_string_lossy().to_string();

            let metadata = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };

            let file_type = if metadata.file_type().is_symlink() {
                FileType::Symlink
            } else if metadata.is_dir() {
                FileType::Directory
            } else {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::FileTypeExt;
                    if metadata.file_type().is_socket() {
                        FileType::Socket
                    } else if metadata.file_type().is_char_device() {
                        FileType::CharDevice
                    } else if metadata.file_type().is_block_device() {
                        FileType::BlockDevice
                    } else if metadata.file_type().is_fifo() {
                        FileType::Fifo
                    } else {
                        FileType::RegularFile
                    }
                }
                #[cfg(not(unix))]
                FileType::RegularFile
            };

            let relative = path.strip_prefix(src).unwrap_or(&path).to_path_buf();
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
        let ino = alloc_inode();
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
            selinux_context: None,
            capabilities: None,
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
