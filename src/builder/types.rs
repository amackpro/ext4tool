use crate::ext4::FileType;
use std::path::PathBuf;

/// Per-file metadata collected during directory walk
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(super) struct FileEntry {
    pub(super) inode: u32,
    pub(super) file_type: FileType,
    pub(super) size: u64,
    pub(super) mode: u16,
    pub(super) uid: u32,
    pub(super) gid: u32,
    pub(super) mtime: u64,
    pub(super) symlink_target: Option<Vec<u8>>,
    pub(super) source_path: Option<PathBuf>,
    pub(super) rel_path: PathBuf,
    pub(super) selinux_context: Option<String>,
    pub(super) capabilities: Option<String>,
}

/// Tree node for directory hierarchy
#[derive(Debug, Clone)]
pub(super) struct FileNode {
    pub(super) name: String,
    pub(super) inode: u32,
    pub(super) file_type: FileType,
    pub(super) children: Vec<FileNode>,
}
