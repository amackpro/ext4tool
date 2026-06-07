/// Extended attribute (xattr) building for SELinux contexts and capabilities
///
/// This module handles inline xattr creation for security attributes.
/// Inline xattrs are stored in the inode's extra space (offsets 156-255 in a 256-byte inode).

use super::constants::{XATTR_MAGIC, XATTR_SECURITY_PREFIX, XATTR_SELINUX_SUFFIX};
use byteorder::{ByteOrder, LittleEndian};

const XATTR_CAPABILITY_SUFFIX: &[u8] = b"capability";

/// Build inline xattr data for security.selinux
///
/// Format:
/// - 4 bytes: magic (0xEA020000)
/// - Entry: name_len(1) + name_index(1) + value_offs(2) + value_inum(4) + value_size(4) + hash(4) + name
/// - Value: SELinux context string (null-terminated)
///
/// For inline xattrs in ext4:
/// - Xattrs start at offset 156 in a 256-byte inode (after 128-byte base + 28-byte extra)
/// - value_offs is relative to the start of the xattr data (offset 0 = first byte after we write)
/// - Total available space: 100 bytes (256 - 156)
pub fn build_selinux_xattr(context: &str) -> Option<Vec<u8>> {
    if context.is_empty() {
        return None;
    }

    // SELinux contexts should be null-terminated
    let mut context_bytes = context.as_bytes().to_vec();
    if !context_bytes.ends_with(&[0]) {
        context_bytes.push(0);
    }

    let name = XATTR_SELINUX_SUFFIX;
    let name_len = name.len() as u8;
    let value_size = context_bytes.len() as u32;

    // Calculate sizes
    // Entry structure: name_len(1) + name_index(1) + value_offs(2) + value_inum(4) + value_size(4) + hash(4) + name
    let entry_base_size = 1 + 1 + 2 + 4 + 4 + 4; // 16 bytes
    let entry_total = entry_base_size + name_len as usize;
    let entry_padded = (entry_total + 3) & !3; // 4-byte aligned

    // Need 4 bytes for terminating entry (all zeros)
    let terminator_size = 4;

    let header_size = 4; // magic only

    // For inline xattrs, e_value_offs is relative to AFTER the magic header
    // Xattr area layout: [magic:4] [entry:padded] [terminator:4] [value:value_size]
    // e_value_offs points to value position relative to offset 4 (after magic)
    let value_offset = (entry_padded + terminator_size) as u16;

    // Size of xattr data (not including inode offset)
    let xattr_data_size = value_offset as usize + value_size as usize;

    // Check if it fits in available space (100 bytes in 256-byte inode)
    const MAX_INLINE_XATTR_SIZE: usize = 100;
    if xattr_data_size > MAX_INLINE_XATTR_SIZE {
        eprintln!("Warning: SELinux context too large for inline xattr: {} > {} bytes",
            xattr_data_size, MAX_INLINE_XATTR_SIZE);
        eprintln!("  Context: {}", context);
        return None;
    }

    let mut xattr = vec![0u8; xattr_data_size];

    // Write header (magic = 0xEA020000)
    LittleEndian::write_u32(&mut xattr[0..4], XATTR_MAGIC);

    // Write entry at offset 4
    let mut pos = 4;
    xattr[pos] = name_len;                                    // e_name_len
    pos += 1;
    xattr[pos] = XATTR_SECURITY_PREFIX;                       // e_name_index (6 = security)
    pos += 1;
    LittleEndian::write_u16(&mut xattr[pos..pos+2], value_offset); // e_value_offs
    pos += 2;
    LittleEndian::write_u32(&mut xattr[pos..pos+4], 0);       // e_value_inum (0 for inline)
    pos += 4;
    LittleEndian::write_u32(&mut xattr[pos..pos+4], value_size); // e_value_size
    pos += 4;
    LittleEndian::write_u32(&mut xattr[pos..pos+4], 0);       // e_hash (0 for inline)
    pos += 4;

    // Write name ("selinux")
    xattr[pos..pos + name_len as usize].copy_from_slice(name);

    // Write terminating entry (4 zero bytes) at entry_padded offset
    // This marks the end of the entry list
    let term_pos = (header_size + entry_padded) as usize;
    xattr[term_pos..term_pos + 4].fill(0);

    // Write value at value_offset (relative to xattr start)
    let value_start = value_offset as usize;
    xattr[value_start..value_start + value_size as usize].copy_from_slice(&context_bytes);

    Some(xattr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_selinux_xattr_basic() {
        let context = "u:object_r:vendor_file:s0";
        let xattr = build_selinux_xattr(context).expect("should build xattr");

        // Check magic
        assert_eq!(LittleEndian::read_u32(&xattr[0..4]), XATTR_MAGIC);

        // Check entry
        assert_eq!(xattr[4], 7); // name_len = "selinux".len()
        assert_eq!(xattr[5], XATTR_SECURITY_PREFIX); // name_index = 6

        // Check name
        let name_start = 4 + 16; // after entry header
        assert_eq!(&xattr[name_start..name_start + 7], b"selinux");

        // Check value includes null terminator
        let value_offset = LittleEndian::read_u16(&xattr[6..8]) as usize;
        let value_size = LittleEndian::read_u32(&xattr[10..14]) as usize;
        let value = &xattr[value_offset..value_offset + value_size];
        assert!(value.ends_with(&[0]), "SELinux context should be null-terminated");
    }

    #[test]
    fn test_selinux_xattr_empty() {
        assert!(build_selinux_xattr("").is_none());
    }

    #[test]
    fn test_selinux_xattr_size_limit() {
        // Create a very long context that exceeds 100 bytes
        let long_context = "u:object_r:".to_string() + &"x".repeat(100) + ":s0";
        assert!(build_selinux_xattr(&long_context).is_none());
    }
}
