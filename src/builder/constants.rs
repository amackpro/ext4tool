/// ext4 geometry and feature constants for building images

// ── Sparse superblock helpers ──────────────────────────────────────────────────
pub(super) fn is_power_of(n: u32, base: u32) -> bool {
    if n == 0 { return false; }
    let mut p = 1u64;
    while p < n as u64 {
        p = p.wrapping_mul(base as u64);
        if p > n as u64 { return false; }
    }
    p == n as u64
}

pub(super) fn has_sparse_super(group: u32) -> bool {
    group == 0 || group == 1
        || is_power_of(group, 3)
        || is_power_of(group, 5)
        || is_power_of(group, 7)
}

// ── Geometry ──────────────────────────────────────────────────────────────────
pub(super) const DEFAULT_BLOCK_SIZE: u64 = 4096;
pub(super) const INODE_SIZE: u16 = 256;
pub(super) const BLOCKS_PER_GROUP: u32 = 32768;
pub(super) const INODES_PER_GROUP: u32 = 2048;

pub(super) const EXT4_FEATURE_INCOMPAT_FILETYPE: u32 = 0x0002;
pub(super) const EXT4_FEATURE_INCOMPAT_EXTENTS: u32 = 0x0040;
pub(super) const EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER: u32 = 0x0001;
pub(super) const EXT4_FEATURE_RO_COMPAT_HUGE_FILE: u32 = 0x0008;
#[allow(unused)]
pub(super) const EXT4_FEATURE_RO_COMPAT_GDT_CSUM: u32 = 0x0010;
pub(super) const EXT4_FEATURE_RO_COMPAT_DIR_NLINK: u32 = 0x0020;
pub(super) const EXT4_FEATURE_RO_COMPAT_EXTRA_ISIZE: u32 = 0x0040;
pub(super) const EXT4_FEATURE_COMPAT_DIR_PREALLOC: u32 = 0x0001;
pub(super) const EXT4_FEATURE_COMPAT_EXT_ATTR: u32 = 0x0008;

pub(super) const EXT4_FIRST_USER_INO: u32 = 12;
pub(super) const EXT4_EXTENT_MAGIC: u16 = 0xF30A;
pub(super) const EXT4_EXTENTS_FL: u32 = 0x00080000;

pub(super) const EXT4_FT_REG_FILE: u8 = 1;
pub(super) const EXT4_FT_DIR: u8 = 2;
pub(super) const EXT4_FT_SYMLINK: u8 = 7;

// Extended attribute constants
pub(super) const XATTR_MAGIC: u32 = 0xEA020000;
pub(super) const XATTR_SECURITY_PREFIX: u8 = 6;
pub(super) const XATTR_SELINUX_SUFFIX: &[u8] = b"selinux";
