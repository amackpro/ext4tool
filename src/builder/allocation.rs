use super::constants::*;
use super::Builder;

/// Bitmap helper functions
pub(super) fn set_bit(bm: &mut [u8], bit: u64) {
    let byte = (bit / 8) as usize;
    let bit_in_byte = (bit % 8) as u8;
    if byte < bm.len() {
        bm[byte] |= 1 << bit_in_byte;
    }
}

pub(super) fn get_bit(bm: &[u8], bit: u64) -> bool {
    let byte = (bit / 8) as usize;
    let bit_in_byte = (bit % 8) as u8;
    if byte < bm.len() {
        bm[byte] & (1 << bit_in_byte) != 0
    } else {
        false
    }
}

impl Builder {
    /// Allocate a new inode
    pub(super) fn alloc_inode(&mut self) -> u32 {
        let ino = self.next_inode;
        self.next_inode += 1;
        let idx = (ino - 1) as u64;
        set_bit(&mut self.inode_bitmap, idx);
        ino
    }

    /// Allocate contiguous blocks, returning segments of (start_block, count)
    pub(super) fn alloc_blocks(&mut self, count: u64) -> Vec<(u64, u64)> {
        let mut segments = Vec::new();
        let mut actual = self.next_data_block;
        let mut remaining = count;

        while remaining > 0 {
            let group = (actual / self.blocks_per_group as u64) as u32;
            let group_base = group as u64 * self.blocks_per_group as u64;

            let has_sb = has_sparse_super(group);
            let metadata_start = if has_sb { 0 } else { 2 };
            let metadata_end = 4 + self.itable_blocks;

            let group_end = group_base + self.blocks_per_group as u64;
            let group_data_start = group_base + metadata_start;

            if actual >= group_end {
                let next_group = group + 1;
                if next_group >= self.num_groups {
                    break;
                }
                let next_base = next_group as u64 * self.blocks_per_group as u64;
                if next_base >= self.total_blocks {
                    break;
                }
                let next_has_sb = has_sparse_super(next_group);
                let next_actual = if next_has_sb {
                    next_base + 4 + self.itable_blocks
                } else {
                    next_base + 0
                };
                if next_actual >= self.total_blocks {
                    break;
                }
                actual = next_actual;
                continue;
            }

            if actual < group_data_start {
                actual = group_data_start;
            }

            let available = if actual < group_base + metadata_start {
                (group_base + metadata_start).saturating_sub(actual)
            } else if actual < group_base + metadata_end {
                let after_metadata = group_base + metadata_end;
                if after_metadata >= self.total_blocks {
                    break;
                }
                actual = after_metadata;
                group_end.saturating_sub(actual).min(self.total_blocks - actual)
            } else {
                group_end.saturating_sub(actual).min(self.total_blocks - actual)
            };

            if available == 0 {
                let next_group = group + 1;
                if next_group >= self.num_groups {
                    break;
                }
                let next_base = next_group as u64 * self.blocks_per_group as u64;
                if next_base >= self.total_blocks {
                    break;
                }
                let next_has_sb = has_sparse_super(next_group);
                let next_actual = if next_has_sb {
                    next_base + 4 + self.itable_blocks
                } else {
                    next_base + 0
                };
                if next_actual >= self.total_blocks {
                    break;
                }
                actual = next_actual;
                continue;
            }

            let take = remaining.min(available);
            for b in actual..actual + take {
                set_bit(&mut self.block_bitmap, b);
            }
            if take > 0 {
                segments.push((actual, take));
            }
            actual += take;
            remaining -= take;
        }

        if actual < self.total_blocks {
            self.next_data_block = actual;
        }

        for &(start, count) in &segments {
            if start + count > self.total_blocks {
                eprintln!("ERROR: alloc_blocks returned invalid segment: start={} count={} (total_blocks={})",
                    start, count, self.total_blocks);
            }
        }

        segments
    }

    /// Count free blocks in the filesystem
    pub(super) fn free_blocks(&self) -> u64 {
        let used: u64 = self.block_bitmap.iter().map(|b| b.count_ones() as u64).sum();
        self.total_blocks.saturating_sub(used.min(self.total_blocks))
    }

    /// Count free inodes in the filesystem
    pub(super) fn free_inodes(&self) -> u32 {
        let used: u64 = self.inode_bitmap.iter().map(|b| b.count_ones() as u64).sum();
        (self.total_inodes as u64).saturating_sub(used) as u32
    }

    /// Count free blocks for a specific block group
    pub(super) fn free_blocks_for_group(&self, group: u32) -> u16 {
        let base = group as u64 * self.blocks_per_group as u64;
        let end = (base + self.blocks_per_group as u64).min(self.total_blocks);
        let mut used = 0u64;
        for b in base..end {
            if get_bit(&self.block_bitmap, b) {
                used += 1;
            }
        }
        (end - base - used) as u16
    }

    /// Count free inodes for a specific block group
    pub(super) fn free_inodes_for_group(&self, group: u32) -> u16 {
        let base = group as u64 * self.inodes_per_group as u64;
        let end = (base + self.inodes_per_group as u64).min(self.total_inodes as u64);
        let mut used = 0u64;
        for i in base..end {
            if get_bit(&self.inode_bitmap, i) {
                used += 1;
            }
        }
        (end - base - used) as u16
    }
}
