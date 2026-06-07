use super::allocation::{get_bit, set_bit};
use super::constants::*;
use super::Builder;
use crate::ext4::{EXT4_SUPERBLOCK_OFFSET, EXT4_SUPER_MAGIC};
use anyhow::Result;
use byteorder::{ByteOrder, LittleEndian};
use std::io::{Seek, SeekFrom, Write};
use std::time::{SystemTime, UNIX_EPOCH};

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl Builder {
    /// Write the ext4 superblock
    pub(super) fn write_superblock(&mut self) -> Result<()> {
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

        let log_block_size = match self.block_size {
            1024 => 0,
            2048 => 1,
            4096 => 2,
            _ => 2,
        };

        w32(&mut sb, 0x00, self.total_inodes);
        w32(&mut sb, 0x04, self.total_blocks as u32);
        w32(&mut sb, 0x08, self.reserved_blocks as u32);
        w32(&mut sb, 0x0C, self.free_blocks() as u32);
        w32(&mut sb, 0x10, self.free_inodes());
        w32(&mut sb, 0x14, 0);
        w32(&mut sb, 0x18, log_block_size);
        w32(&mut sb, 0x1C, log_block_size);
        w32(&mut sb, 0x20, self.blocks_per_group);
        w32(&mut sb, 0x24, self.blocks_per_group);
        w32(&mut sb, 0x28, self.inodes_per_group);
        w32(&mut sb, 0x2C, 0);
        w32(&mut sb, 0x30, now);
        w16(&mut sb, 0x34, 0);
        w16(&mut sb, 0x36, 0xFFFF);
        w16(&mut sb, 0x38, EXT4_SUPER_MAGIC);
        w16(&mut sb, 0x3A, 1);
        w16(&mut sb, 0x3C, 1);
        w16(&mut sb, 0x3E, 0);
        w32(&mut sb, 0x40, now);
        w32(&mut sb, 0x44, 0);
        w32(&mut sb, 0x48, 0);
        w32(&mut sb, 0x4C, 1);
        w16(&mut sb, 0x50, 0);
        w16(&mut sb, 0x52, 0);

        w32(&mut sb, 0x54, EXT4_FIRST_USER_INO);
        w16(&mut sb, 0x58, self.inode_size);
        w16(&mut sb, 0x5A, 0);

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

        let uuid = uuid::Uuid::new_v4();
        sb[0x68..0x68 + 16].copy_from_slice(uuid.as_bytes());

        w16(&mut sb, 0xFE, 0);

        w16(&mut sb, 0x15C, 28);
        w16(&mut sb, 0x15E, 28);

        w32(&mut sb, 0x150, (self.total_blocks >> 32) as u32);

        self.file.write_all(&sb)?;
        Ok(())
    }

    /// Write group descriptor table
    pub(super) fn write_group_descriptors(&mut self) -> Result<()> {
        let gdt_offset = 1 * self.block_size;
        self.file.seek(SeekFrom::Start(gdt_offset))?;

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

            w32(&mut gd, 0x00, (base_block + 2) as u32);
            w32(&mut gd, 0x04, (base_block + 3) as u32);
            w32(&mut gd, 0x08, (base_block + 4) as u32);
            w16(&mut gd, 0x0C, self.free_blocks_for_group(g));
            w16(&mut gd, 0x0E, self.free_inodes_for_group(g));
            w16(&mut gd, 0x10, self.dir_count_group[g as usize]);
            w16(&mut gd, 0x12, 0);

            self.file.write_all(&gd)?;
        }

        let written = self.num_groups as u64 * 32;
        let remaining = self.block_size - (written % self.block_size);
        if remaining < self.block_size {
            let pad = vec![0u8; remaining as usize];
            self.file.write_all(&pad)?;
        }

        Ok(())
    }

    /// Finalize filesystem by writing bitmaps and updating metadata
    pub(super) fn finalize(&mut self) -> Result<()> {
        for g in 0..self.num_groups {
            let base = g as u64 * self.blocks_per_group as u64;
            let mut bm = vec![0u8; self.block_size as usize];
            let group_end = self.blocks_per_group as u64;
            for i in 0..group_end {
                let bit = base + i;
                if bit < self.total_blocks && get_bit(&self.block_bitmap, bit) {
                    set_bit(&mut bm, i);
                }
            }
            let last_data_bit = (self.total_blocks - base).min(group_end);
            for i in last_data_bit..self.block_size as u64 * 8 {
                set_bit(&mut bm, i);
            }

            let bm_off = (base + 2) * self.block_size;
            self.file.seek(SeekFrom::Start(bm_off))?;
            self.file.write_all(&bm)?;

            let inode_base = g as u64 * self.inodes_per_group as u64;
            let group_inodes = self.inodes_per_group as u64;
            let mut im = vec![0u8; self.block_size as usize];
            for i in 0..group_inodes {
                let bit = inode_base + i;
                if bit < self.total_inodes as u64 && get_bit(&self.inode_bitmap, bit) {
                    set_bit(&mut im, i);
                }
            }
            let last_inode_bit = (self.total_inodes as u64 - inode_base).min(group_inodes);
            for i in last_inode_bit..self.block_size as u64 * 8 {
                set_bit(&mut im, i);
            }

            let im_off = (base + 3) * self.block_size;
            self.file.seek(SeekFrom::Start(im_off))?;
            self.file.write_all(&im)?;
        }

        self.write_group_descriptors()?;
        self.write_superblock()?;
        Ok(())
    }
}
