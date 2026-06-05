use anyhow::{anyhow, Context, Result};
use byteorder::{LittleEndian, ReadBytesExt};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

// Constants
pub const EXT4_SUPER_MAGIC: u16 = 0xEF53;
pub const EXT4_SUPERBLOCK_OFFSET: u64 = 1024;
pub const EXT4_ROOT_INODE: u32 = 2;

// Feature flags
const EXT4_FEATURE_INCOMPAT_64BIT: u32 = 0x0080;
const EXT4_FEATURE_INCOMPAT_FILETYPE: u32 = 0x0002;

// Inode flags
const EXT4_EXTENTS_FL: u32 = 0x00080000;
const EXT4_INLINE_DATA_FL: u32 = 0x10000000;

// Extent magic
const EXT4_EXTENT_MAGIC: u16 = 0xF30A;

// File type constants
pub const S_IFMT: u16 = 0o170000;
pub const S_IFSOCK: u16 = 0o140000;
pub const S_IFLNK: u16 = 0o120000;
pub const S_IFREG: u16 = 0o100000;
pub const S_IFBLK: u16 = 0o060000;
pub const S_IFDIR: u16 = 0o040000;
pub const S_IFCHR: u16 = 0o020000;
pub const S_IFIFO: u16 = 0o010000;

// Directory entry file types
const EXT4_FT_UNKNOWN: u8 = 0;
const EXT4_FT_REG_FILE: u8 = 1;
const EXT4_FT_DIR: u8 = 2;
const EXT4_FT_CHRDEV: u8 = 3;
const EXT4_FT_BLKDEV: u8 = 4;
const EXT4_FT_FIFO: u8 = 5;
const EXT4_FT_SOCK: u8 = 6;
const EXT4_FT_SYMLINK: u8 = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    Unknown = 0,
    RegularFile = 1,
    Directory = 2,
    CharDevice = 3,
    BlockDevice = 4,
    Fifo = 5,
    Socket = 6,
    Symlink = 7,
}

impl From<u8> for FileType {
    fn from(val: u8) -> Self {
        match val {
            1 => FileType::RegularFile,
            2 => FileType::Directory,
            3 => FileType::CharDevice,
            4 => FileType::BlockDevice,
            5 => FileType::Fifo,
            6 => FileType::Socket,
            7 => FileType::Symlink,
            _ => FileType::Unknown,
        }
    }
}

#[derive(Debug)]
pub struct Superblock {
    pub s_inodes_count: u32,
    pub s_blocks_count: u64,
    pub s_log_block_size: u32,
    pub s_inodes_per_group: u32,
    pub s_blocks_per_group: u32,
    pub s_inode_size: u16,
    pub s_magic: u16,
    pub s_feature_incompat: u32,
    pub s_desc_size: u16,
    pub block_size: u32,
}

impl Superblock {
    pub fn read_from<R: Read + Seek>(reader: &mut R) -> Result<Self> {
        // Superblock starts at offset 1024
        reader.seek(SeekFrom::Start(EXT4_SUPERBLOCK_OFFSET))?;

        // Read fields in order (offsets relative to superblock start)
        let s_inodes_count = reader.read_u32::<LittleEndian>()?;              // 0x0000
        let s_blocks_count_lo = reader.read_u32::<LittleEndian>()?;           // 0x0004
        let _s_r_blocks_count_lo = reader.read_u32::<LittleEndian>()?;        // 0x0008
        let _s_free_blocks_count_lo = reader.read_u32::<LittleEndian>()?;     // 0x000C
        let _s_free_inodes_count = reader.read_u32::<LittleEndian>()?;        // 0x0010
        let _s_first_data_block = reader.read_u32::<LittleEndian>()?;         // 0x0014
        let s_log_block_size = reader.read_u32::<LittleEndian>()?;            // 0x0018
        let _s_log_cluster_size = reader.read_u32::<LittleEndian>()?;         // 0x001C
        let s_blocks_per_group = reader.read_u32::<LittleEndian>()?;          // 0x0020
        let _s_clusters_per_group = reader.read_u32::<LittleEndian>()?;       // 0x0024
        let s_inodes_per_group = reader.read_u32::<LittleEndian>()?;          // 0x0028
        let _s_mtime = reader.read_u32::<LittleEndian>()?;                    // 0x002C
        let _s_wtime = reader.read_u32::<LittleEndian>()?;                    // 0x0030
        let _s_mnt_count = reader.read_u16::<LittleEndian>()?;                // 0x0034
        let _s_max_mnt_count = reader.read_u16::<LittleEndian>()?;            // 0x0036
        let s_magic = reader.read_u16::<LittleEndian>()?;                     // 0x0038

        if s_magic != EXT4_SUPER_MAGIC {
            return Err(anyhow!("Invalid ext4 magic: 0x{:X}", s_magic));
        }

        let _s_state = reader.read_u16::<LittleEndian>()?;                    // 0x003A
        let _s_errors = reader.read_u16::<LittleEndian>()?;                   // 0x003C
        let _s_minor_rev_level = reader.read_u16::<LittleEndian>()?;          // 0x003E
        let _s_lastcheck = reader.read_u32::<LittleEndian>()?;                // 0x0040
        let _s_checkinterval = reader.read_u32::<LittleEndian>()?;            // 0x0044
        let _s_creator_os = reader.read_u32::<LittleEndian>()?;               // 0x0048
        let _s_rev_level = reader.read_u32::<LittleEndian>()?;                // 0x004C
        let _s_def_resuid = reader.read_u16::<LittleEndian>()?;               // 0x0050
        let _s_def_resgid = reader.read_u16::<LittleEndian>()?;               // 0x0052
        let _s_first_ino = reader.read_u32::<LittleEndian>()?;                // 0x0054
        let s_inode_size = reader.read_u16::<LittleEndian>()?;                // 0x0058
        let _s_block_group_nr = reader.read_u16::<LittleEndian>()?;           // 0x005A
        let _s_feature_compat = reader.read_u32::<LittleEndian>()?;           // 0x005C
        let s_feature_incompat = reader.read_u32::<LittleEndian>()?;          // 0x0060
        let _s_feature_ro_compat = reader.read_u32::<LittleEndian>()?;        // 0x0064

        // Skip UUID, volume name, last mounted, etc. (0x0068 to 0x00FE)
        reader.seek(SeekFrom::Current(16))?;  // s_uuid (0x0068)
        reader.seek(SeekFrom::Current(16))?;  // s_volume_name (0x0078)
        reader.seek(SeekFrom::Current(64))?;  // s_last_mounted (0x0088)
        reader.seek(SeekFrom::Current(4))?;   // s_algorithm_usage_bitmap (0x00C8)
        reader.seek(SeekFrom::Current(1))?;   // s_prealloc_blocks (0x00CC)
        reader.seek(SeekFrom::Current(1))?;   // s_prealloc_dir_blocks (0x00CD)
        reader.seek(SeekFrom::Current(2))?;   // s_reserved_gdt_blocks (0x00CE)
        reader.seek(SeekFrom::Current(16))?;  // s_journal_uuid (0x00D0)
        reader.seek(SeekFrom::Current(4))?;   // s_journal_inum (0x00E0)
        reader.seek(SeekFrom::Current(4))?;   // s_journal_dev (0x00E4)
        reader.seek(SeekFrom::Current(4))?;   // s_last_orphan (0x00E8)
        reader.seek(SeekFrom::Current(16))?;  // s_hash_seed (0x00EC)
        reader.seek(SeekFrom::Current(1))?;   // s_def_hash_version (0x00FC)
        reader.seek(SeekFrom::Current(1))?;   // s_jnl_backup_type (0x00FD)

        let s_desc_size = reader.read_u16::<LittleEndian>()?;                 // 0x00FE

        // Skip to s_blocks_count_hi at 0x0150
        reader.seek(SeekFrom::Current(4))?;   // s_default_mount_opts (0x0100)
        reader.seek(SeekFrom::Current(4))?;   // s_first_meta_bg (0x0104)
        reader.seek(SeekFrom::Current(4))?;   // s_mkfs_time (0x0108)
        reader.seek(SeekFrom::Current(68))?;  // s_jnl_blocks (0x010C-0x014F, 17*4 bytes)

        let s_blocks_count_hi = reader.read_u32::<LittleEndian>()?;           // 0x0150

        let s_blocks_count = ((s_blocks_count_hi as u64) << 32) | (s_blocks_count_lo as u64);
        let block_size = 1024u32 << s_log_block_size;

        Ok(Superblock {
            s_inodes_count,
            s_blocks_count,
            s_log_block_size,
            s_inodes_per_group,
            s_blocks_per_group,
            s_inode_size,
            s_magic,
            s_feature_incompat,
            s_desc_size,
            block_size,
        })
    }
}

#[derive(Debug)]
pub struct GroupDescriptor {
    pub bg_inode_table: u64,
    pub bg_inode_bitmap: u64,
    pub bg_block_bitmap: u64,
}

impl GroupDescriptor {
    pub fn read_from<R: Read + Seek>(reader: &mut R, platform64: bool) -> Result<Self> {
        let bg_block_bitmap_lo = reader.read_u32::<LittleEndian>()?;
        let bg_inode_bitmap_lo = reader.read_u32::<LittleEndian>()?;
        let bg_inode_table_lo = reader.read_u32::<LittleEndian>()?;

        reader.seek(SeekFrom::Current(20))?; // skip other fields

        let (bg_block_bitmap_hi, bg_inode_bitmap_hi, bg_inode_table_hi) = if platform64 {
            let bb_hi = reader.read_u32::<LittleEndian>()?;
            let ib_hi = reader.read_u32::<LittleEndian>()?;
            let it_hi = reader.read_u32::<LittleEndian>()?;
            reader.seek(SeekFrom::Current(20))?; // skip remaining fields
            (bb_hi, ib_hi, it_hi)
        } else {
            (0, 0, 0)
        };

        Ok(GroupDescriptor {
            bg_inode_table: ((bg_inode_table_hi as u64) << 32) | (bg_inode_table_lo as u64),
            bg_inode_bitmap: ((bg_inode_bitmap_hi as u64) << 32) | (bg_inode_bitmap_lo as u64),
            bg_block_bitmap: ((bg_block_bitmap_hi as u64) << 32) | (bg_block_bitmap_lo as u64),
        })
    }
}

#[derive(Debug)]
pub struct Inode {
    pub i_mode: u16,
    pub i_uid: u32,
    pub i_size: u64,
    pub i_gid: u32,
    pub i_links_count: u16,
    pub i_blocks: u32,
    pub i_flags: u32,
    pub i_block: [u32; 15],
    pub i_file_acl: u64,
    pub i_extra_isize: u16,
    pub extra_data: Vec<u8>,  // Data after standard inode (for inline xattrs)
}

impl Inode {
    pub fn read_from<R: Read + Seek>(reader: &mut R, inode_size: u16) -> Result<Self> {
        // Read entire inode into buffer first to avoid seek position issues
        let mut inode_buf = vec![0u8; inode_size as usize];
        reader.read_exact(&mut inode_buf)?;

        // Now parse from buffer
        let mut cursor = io::Cursor::new(&inode_buf);

        let i_mode = cursor.read_u16::<LittleEndian>()?;
        let i_uid_lo = cursor.read_u16::<LittleEndian>()?;
        let i_size_lo = cursor.read_u32::<LittleEndian>()?;
        cursor.seek(SeekFrom::Current(12))?; // skip atime, ctime, mtime
        cursor.seek(SeekFrom::Current(4))?; // skip dtime
        let i_gid_lo = cursor.read_u16::<LittleEndian>()?;
        let i_links_count = cursor.read_u16::<LittleEndian>()?;
        let i_blocks = cursor.read_u32::<LittleEndian>()?;
        let i_flags = cursor.read_u32::<LittleEndian>()?;
        cursor.seek(SeekFrom::Current(4))?; // skip osd1

        let mut i_block = [0u32; 15];
        for i in 0..15 {
            i_block[i] = cursor.read_u32::<LittleEndian>()?;
        }

        cursor.seek(SeekFrom::Current(4))?; // skip generation
        let i_file_acl_lo = cursor.read_u32::<LittleEndian>()?;
        let i_size_hi = cursor.read_u32::<LittleEndian>()?;
        cursor.seek(SeekFrom::Current(4))?; // skip obso_faddr

        // osd2 - 12 bytes
        cursor.seek(SeekFrom::Current(2))?; // skip l_i_blocks_high
        let i_file_acl_hi = cursor.read_u16::<LittleEndian>()?;
        let i_uid_hi = cursor.read_u16::<LittleEndian>()?;
        let i_gid_hi = cursor.read_u16::<LittleEndian>()?;
        cursor.seek(SeekFrom::Current(4))?; // skip checksum_lo and reserved

        // Now at offset 128 (0x80) - read i_extra_isize
        let i_extra_isize = if inode_size > 128 {
            cursor.read_u16::<LittleEndian>()?
        } else {
            0
        };

        // Read inline xattr area from the buffer
        // Layout: [128 bytes std][i_extra_isize bytes extra][inline xattrs]
        let mut extra_data = Vec::new();
        if inode_size > 128 && i_extra_isize > 0 {
            let xattr_start = (128 + i_extra_isize) as usize;
            let xattr_end = inode_size as usize;

            if xattr_end > xattr_start {
                // Copy inline xattr area from the buffer
                extra_data = inode_buf[xattr_start..xattr_end].to_vec();
            }
        }

        let i_size = ((i_size_hi as u64) << 32) | (i_size_lo as u64);
        let i_uid = ((i_uid_hi as u32) << 16) | (i_uid_lo as u32);
        let i_gid = ((i_gid_hi as u32) << 16) | (i_gid_lo as u32);
        let i_file_acl = ((i_file_acl_hi as u64) << 32) | (i_file_acl_lo as u64);

        Ok(Inode {
            i_mode,
            i_uid,
            i_size,
            i_gid,
            i_links_count,
            i_blocks,
            i_flags,
            i_block,
            i_file_acl,
            i_extra_isize,
            extra_data,
        })
    }

    pub fn file_type(&self) -> FileType {
        let mode = self.i_mode & S_IFMT;
        match mode {
            S_IFREG => FileType::RegularFile,
            S_IFDIR => FileType::Directory,
            S_IFLNK => FileType::Symlink,
            S_IFCHR => FileType::CharDevice,
            S_IFBLK => FileType::BlockDevice,
            S_IFIFO => FileType::Fifo,
            S_IFSOCK => FileType::Socket,
            _ => FileType::Unknown,
        }
    }

    pub fn is_dir(&self) -> bool {
        self.file_type() == FileType::Directory
    }

    pub fn is_file(&self) -> bool {
        self.file_type() == FileType::RegularFile
    }

    pub fn is_symlink(&self) -> bool {
        self.file_type() == FileType::Symlink
    }

    pub fn permissions(&self) -> u32 {
        (self.i_mode & 0o7777) as u32
    }
}

#[derive(Debug, Clone)]
pub struct Extent {
    pub ee_block: u32,
    pub ee_len: u16,
    pub ee_start: u64,
}

#[derive(Debug)]
pub struct ExtentHeader {
    pub eh_magic: u16,
    pub eh_entries: u16,
    pub eh_depth: u16,
}

pub struct Volume {
    file: File,
    pub superblock: Superblock,
    group_descriptors: Vec<GroupDescriptor>,
    platform64: bool,
}

impl Volume {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let mut file = File::open(path)?;
        let superblock = Superblock::read_from(&mut file)?;

        if std::env::var("DEBUG").is_ok() {
            println!("\nDEBUG: Superblock info:");
            println!("  Inodes per group: {}", superblock.s_inodes_per_group);
            println!("  Blocks per group: {}", superblock.s_blocks_per_group);
            println!("  Inode size: {} bytes", superblock.s_inode_size);
            println!("  Feature incompat: 0x{:08X}", superblock.s_feature_incompat);
            println!("  Desc size: {}", superblock.s_desc_size);
        }

        let platform64 = (superblock.s_feature_incompat & EXT4_FEATURE_INCOMPAT_64BIT) != 0;

        // Read group descriptors
        let desc_size = if platform64 && superblock.s_desc_size > 32 {
            superblock.s_desc_size as u64
        } else {
            32
        };

        let group_count = ((superblock.s_blocks_count + superblock.s_blocks_per_group as u64 - 1)
                          / superblock.s_blocks_per_group as u64) as usize;

        let gdt_offset = if superblock.block_size == 1024 {
            2048
        } else {
            superblock.block_size as u64
        };

        file.seek(SeekFrom::Start(gdt_offset))?;

        let mut group_descriptors = Vec::with_capacity(group_count);
        for _ in 0..group_count {
            let gd = GroupDescriptor::read_from(&mut file, platform64)?;
            group_descriptors.push(gd);
        }

        Ok(Volume {
            file,
            superblock,
            group_descriptors,
            platform64,
        })
    }

    pub fn get_inode(&mut self, inode_num: u32) -> Result<Inode> {
        if inode_num == 0 || inode_num > self.superblock.s_inodes_count {
            return Err(anyhow!("Invalid inode number: {}", inode_num));
        }

        let group_idx = ((inode_num - 1) / self.superblock.s_inodes_per_group) as usize;
        let local_idx = (inode_num - 1) % self.superblock.s_inodes_per_group;

        let gd = &self.group_descriptors[group_idx];
        let inode_table_offset = gd.bg_inode_table * self.superblock.block_size as u64;
        let inode_offset = inode_table_offset + (local_idx as u64 * self.superblock.s_inode_size as u64);

        if std::env::var("DEBUG").is_ok() && inode_num == EXT4_ROOT_INODE {
            println!("\nDEBUG: Reading inode {}:", inode_num);
            println!("  Group index: {}", group_idx);
            println!("  Local index: {}", local_idx);
            println!("  Inode table block: {}", gd.bg_inode_table);
            println!("  Inode table offset: 0x{:X}", inode_table_offset);
            println!("  Final inode offset: 0x{:X}", inode_offset);
        }

        self.file.seek(SeekFrom::Start(inode_offset))?;
        Inode::read_from(&mut self.file, self.superblock.s_inode_size)
    }

    pub fn read_block(&mut self, block_num: u64) -> Result<Vec<u8>> {
        let offset = block_num * self.superblock.block_size as u64;
        self.file.seek(SeekFrom::Start(offset))
            .context(format!("Failed to seek to block {} at offset 0x{:X}", block_num, offset))?;

        let mut buffer = vec![0u8; self.superblock.block_size as usize];
        self.file.read_exact(&mut buffer)
            .context(format!("Failed to read block {} (size: {} bytes) at offset 0x{:X}",
                block_num, self.superblock.block_size, offset))?;
        Ok(buffer)
    }

    pub fn read_extents(&mut self, inode: &Inode) -> Result<Vec<Extent>> {
        if (inode.i_flags & EXT4_EXTENTS_FL) == 0 {
            return Ok(Vec::new());
        }

        let i_block_bytes: Vec<u8> = inode.i_block.iter()
            .flat_map(|&x| x.to_le_bytes())
            .collect();

        if std::env::var("DEBUG").is_ok() {
            println!("\nDEBUG: Reading extents from i_block:");
            println!("  i_block hex: {}", i_block_bytes.iter()
                .map(|b| format!("{:02X}", b))
                .collect::<Vec<_>>()
                .join(" "));
        }

        let mut cursor = io::Cursor::new(i_block_bytes);
        self.parse_extent_tree(&mut cursor, &mut Vec::new())
    }

    fn parse_extent_tree<R: Read + Seek>(&mut self, reader: &mut R, extents: &mut Vec<Extent>) -> Result<Vec<Extent>> {
        let eh_magic = reader.read_u16::<LittleEndian>()?;
        if eh_magic != EXT4_EXTENT_MAGIC {
            return Err(anyhow!("Invalid extent magic: 0x{:X}", eh_magic));
        }

        let eh_entries = reader.read_u16::<LittleEndian>()?;
        let eh_max = reader.read_u16::<LittleEndian>()?;
        let eh_depth = reader.read_u16::<LittleEndian>()?;
        reader.seek(SeekFrom::Current(4))?; // skip generation

        if std::env::var("DEBUG").is_ok() {
            println!("  Extent header: magic=0x{:X}, entries={}, max={}, depth={}",
                eh_magic, eh_entries, eh_max, eh_depth);
        }

        if eh_depth == 0 {
            // Leaf node - read extents
            for i in 0..eh_entries {
                let ee_block = reader.read_u32::<LittleEndian>()?;
                let ee_len = reader.read_u16::<LittleEndian>()?;
                let ee_start_hi = reader.read_u16::<LittleEndian>()?;
                let ee_start_lo = reader.read_u32::<LittleEndian>()?;

                let ee_start = ((ee_start_hi as u64) << 32) | (ee_start_lo as u64);

                if std::env::var("DEBUG").is_ok() && i < 3 {
                    println!("    Extent {}: block={}, len={}, start={} (hi={}, lo={})",
                        i, ee_block, ee_len, ee_start, ee_start_hi, ee_start_lo);
                }

                extents.push(Extent {
                    ee_block,
                    ee_len,
                    ee_start,
                });
            }
        } else {
            // Index node - read child blocks
            for _ in 0..eh_entries {
                let ei_block = reader.read_u32::<LittleEndian>()?;
                let ei_leaf_lo = reader.read_u32::<LittleEndian>()?;
                let ei_leaf_hi = reader.read_u16::<LittleEndian>()?;
                reader.seek(SeekFrom::Current(2))?; // skip unused

                let ei_leaf = ((ei_leaf_hi as u64) << 32) | (ei_leaf_lo as u64);

                // Read child block
                let child_block = self.read_block(ei_leaf)?;
                let mut child_cursor = io::Cursor::new(child_block);
                self.parse_extent_tree(&mut child_cursor, extents)?;
            }
        }

        Ok(extents.clone())
    }

    pub fn read_inode_data(&mut self, inode: &Inode) -> Result<Vec<u8>> {
        if inode.i_size == 0 {
            return Ok(Vec::new());
        }

        // Handle inline data (including small symlinks)
        // Symlinks with target <= 60 bytes are stored inline in i_block
        if (inode.i_flags & EXT4_INLINE_DATA_FL) != 0 || (inode.is_symlink() && inode.i_size <= 60) {
            let inline_data: Vec<u8> = inode.i_block.iter()
                .flat_map(|&x| x.to_le_bytes())
                .take(inode.i_size as usize)
                .collect();
            return Ok(inline_data);
        }

        // Handle extent-based files
        if (inode.i_flags & EXT4_EXTENTS_FL) != 0 {
            let extents = self.read_extents(inode)?;
            let mut data = Vec::with_capacity(inode.i_size as usize);

            for extent in extents {
                for block_offset in 0..extent.ee_len as u64 {
                    let block_data = self.read_block(extent.ee_start + block_offset)?;
                    data.extend_from_slice(&block_data);
                }
            }

            data.truncate(inode.i_size as usize);
            return Ok(data);
        }

        // Handle direct block pointers (legacy)
        let mut data = Vec::with_capacity(inode.i_size as usize);
        for &block_num in &inode.i_block[0..12] {
            if block_num == 0 {
                break;
            }
            let block_data = self.read_block(block_num as u64)?;
            data.extend_from_slice(&block_data);
            if data.len() >= inode.i_size as usize {
                break;
            }
        }

        data.truncate(inode.i_size as usize);
        Ok(data)
    }

    pub fn read_dir(&mut self, inode: &Inode) -> Result<Vec<DirEntry>> {
        if !inode.is_dir() {
            return Err(anyhow!(
                "Not a directory: mode=0o{:o}, expected directory bit (0o40000)",
                inode.i_mode
            ));
        }

        let data = self.read_inode_data(inode)?;
        let mut entries = Vec::new();
        let mut offset = 0;

        while offset < data.len() {
            if offset + 8 > data.len() {
                break;
            }

            let inode_num = u32::from_le_bytes([data[offset], data[offset+1], data[offset+2], data[offset+3]]);
            let rec_len = u16::from_le_bytes([data[offset+4], data[offset+5]]) as usize;
            let name_len = data[offset+6] as usize;
            let file_type = data[offset+7];

            if rec_len == 0 || rec_len < 8 {
                break;
            }

            if inode_num != 0 && name_len > 0 && offset + 8 + name_len <= data.len() {
                let name_bytes = &data[offset+8..offset+8+name_len];
                if let Ok(name) = String::from_utf8(name_bytes.to_vec()) {
                    entries.push(DirEntry {
                        inode: inode_num,
                        name,
                        file_type: FileType::from(file_type),
                    });
                }
            }

            offset += rec_len;
        }

        Ok(entries)
    }

    pub fn read_xattrs(&mut self, inode: &Inode) -> Result<HashMap<String, Vec<u8>>> {
        let mut xattrs = HashMap::new();

        if std::env::var("DEBUG").is_ok() {
            println!("DEBUG read_xattrs: i_extra_isize={}, extra_data.len()={}, i_file_acl={}",
                inode.i_extra_isize, inode.extra_data.len(), inode.i_file_acl);
        }

        // Parse inline xattrs first (stored in inode extra space)
        if std::env::var("DEBUG").is_ok() && inode.extra_data.len() > 0 {
            println!("  extra_isize={}, extra_data len={}, first 8 bytes: {:02X?}",
                inode.i_extra_isize, inode.extra_data.len(),
                &inode.extra_data[..inode.extra_data.len().min(8)]);
        }

        if inode.i_extra_isize > 0 && inode.extra_data.len() >= 4 {
            // Check for inline xattr magic (0xEA020000)
            let magic = u32::from_le_bytes([
                inode.extra_data[0],
                inode.extra_data[1],
                inode.extra_data[2],
                inode.extra_data[3],
            ]);

            if std::env::var("DEBUG").is_ok() {
                println!("  Inline xattr magic: 0x{:08X} (expected 0xEA020000)", magic);
            }

            if magic == 0xEA020000 {
                // Parse inline xattrs starting after the 4-byte header
                // Pass sliced data (without magic) and offset=0 for inline xattrs
                let offset = 4;
                if inode.extra_data.len() > offset {
                    self.parse_xattr_entries(&inode.extra_data[offset..], 0, &mut xattrs, true)?;
                    if std::env::var("DEBUG").is_ok() {
                        println!("  Parsed {} inline xattrs", xattrs.len());
                    }
                }
            }
        }

        // Parse block xattrs (if i_file_acl is set)
        if inode.i_file_acl == 0 {
            return Ok(xattrs);
        }

        let block_data = self.read_block(inode.i_file_acl)?;
        let header_size = 32; // ext4_xattr_header is 32 bytes

        // For block xattrs, value_offs is relative to block start, so pass full block data
        // The is_inline parameter will ensure proper offset handling
        if block_data.len() > header_size {
            self.parse_xattr_entries(&block_data, header_size, &mut xattrs, false)?;
        }

        Ok(xattrs)
    }

    fn parse_xattr_entries(&self, data: &[u8], mut offset: usize, xattrs: &mut HashMap<String, Vec<u8>>, is_inline: bool) -> Result<()> {
        while offset + 16 <= data.len() {
            let name_len = data[offset] as usize;
            let name_index = data[offset + 1];
            let value_offs_raw = u16::from_le_bytes([data[offset+2], data[offset+3]]) as usize;
            let value_size = u32::from_le_bytes([
                data[offset+8], data[offset+9],
                data[offset+10], data[offset+11]
            ]) as usize;

            if name_len == 0 {
                break;
            }

            let name_start = offset + 16;
            if name_start + name_len > data.len() {
                break;
            }

            let name_bytes = &data[name_start..name_start + name_len];
            if let Ok(name_suffix) = String::from_utf8(name_bytes.to_vec()) {
                let prefix = match name_index {
                    1 => "user.",
                    4 => "trusted.",
                    6 => "security.",
                    7 => "system.",
                    _ => "",
                };
                let full_name = format!("{}{}", prefix, name_suffix);

                // For inline xattrs: data is sliced after magic, value_offs is relative to sliced data
                // For block xattrs: data includes header, value_offs is relative to block start
                let value_offs = if is_inline {
                    // Inline: value_offs is relative to the sliced data (after magic)
                    value_offs_raw
                } else {
                    // Block: value_offs is relative to block start (offset 0), which is before our sliced data
                    // So we don't need to adjust since we're passing full block data
                    value_offs_raw
                };

                if value_offs > 0 && value_offs + value_size <= data.len() {
                    let value = data[value_offs..value_offs + value_size].to_vec();

                    if std::env::var("DEBUG").is_ok() && full_name == "security.selinux" {
                        let decoded = String::from_utf8_lossy(&value);
                        eprintln!("DEBUG xattr: {} = '{}' (len={}, bytes={:?})",
                            full_name, decoded, value.len(), &value[..value.len().min(30)]);
                    }

                    xattrs.insert(full_name, value);
                }
            }

            // Move to next entry (align to 4 bytes)
            let entry_size = 16 + ((name_len + 3) & !3);
            offset += entry_size;
        }

        Ok(())
    }
}

#[derive(Debug)]
pub struct DirEntry {
    pub inode: u32,
    pub name: String,
    pub file_type: FileType,
}
