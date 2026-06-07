use super::Builder;
use anyhow::Result;
use byteorder::{ByteOrder, LittleEndian};
use std::io::{Seek, SeekFrom, Write};

impl Builder {
    /// Create an inode structure (256 bytes)
    pub(super) fn make_inode(
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

    /// Write an inode to disk
    pub(super) fn write_inode(&mut self, inode_num: u32, data: &[u8; 256]) -> Result<()> {
        self.write_inode_with_xattr(inode_num, data, None)
    }

    /// Write an inode with optional inline xattrs
    pub(super) fn write_inode_with_xattr(&mut self, inode_num: u32, data: &[u8; 256], xattr: Option<&[u8]>) -> Result<()> {
        if inode_num == 0 {
            return Ok(());
        }
        let idx = (inode_num - 1) as u64;
        let group = idx / self.inodes_per_group as u64;
        let in_group = idx % self.inodes_per_group as u64;
        let table_block = group * self.blocks_per_group as u64 + 4;
        let pos = table_block * self.block_size + in_group * self.inode_size as u64;

        self.file.seek(SeekFrom::Start(pos))?;
        self.file.write_all(data)?;

        if let Some(xattr_data) = xattr {
            let xattr_pos = pos + 156;
            let available_space = self.inode_size as usize - 156;

            if xattr_data.len() <= available_space {
                self.file.seek(SeekFrom::Start(xattr_pos))?;
                self.file.write_all(xattr_data)?;
            } else {
                eprintln!("Warning: xattr too large for inline storage ({} > {} bytes), skipping",
                    xattr_data.len(), available_space);
            }
        }

        Ok(())
    }
}
