use super::constants::EXT4_EXTENT_MAGIC;
use anyhow::Result;
use byteorder::{ByteOrder, LittleEndian};

/// Build an extent tree root in i_block
/// Returns a 60-byte array for inline extent storage
/// Supports up to 4 extents inline (12-byte header + 4 * 12-byte extents)
pub(super) fn build_extent_root(entries: &[(u32, u16, u64)]) -> Result<[u8; 60]> {
    const MAX_INLINE_EXTENTS: usize = 4;

    if entries.len() > MAX_INLINE_EXTENTS {
        eprintln!("\n!!! EXTENT LIMIT HIT !!!");
        eprintln!("File needs {} extents (max {})", entries.len(), MAX_INLINE_EXTENTS);
        eprintln!("Extent details:");
        for (i, &(logical, count, physical)) in entries.iter().enumerate() {
            eprintln!("  Extent {}: logical={} count={} physical={}",
                i, logical, count, physical);
        }
        anyhow::bail!(
            "File requires {} extents but inline extent tree only supports {}. \
             This file is too fragmented. Consider using a larger block size or \
             defragmenting the source.",
            entries.len(),
            MAX_INLINE_EXTENTS
        );
    }

    let mut buf = [0u8; 60];
    let mut tmp2 = [0u8; 2];
    let mut tmp4 = [0u8; 4];

    // Extent header (12 bytes)
    LittleEndian::write_u16(&mut tmp2, EXT4_EXTENT_MAGIC);
    buf[0..2].copy_from_slice(&tmp2);
    LittleEndian::write_u16(&mut tmp2, entries.len() as u16);
    buf[2..4].copy_from_slice(&tmp2);
    LittleEndian::write_u16(&mut tmp2, MAX_INLINE_EXTENTS as u16);
    buf[4..6].copy_from_slice(&tmp2);
    // depth = 0 (already zero), generation = 0 (already zero)

    // Write extent entries (each 12 bytes)
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

    Ok(buf)
}
