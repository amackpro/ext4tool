/// Build inline xattrs with multiple attributes (SELinux + capabilities)

use super::constants::{XATTR_MAGIC, XATTR_SECURITY_PREFIX};
use byteorder::{ByteOrder, LittleEndian};

const XATTR_SELINUX_SUFFIX: &[u8] = b"selinux";
const XATTR_CAPABILITY_SUFFIX: &[u8] = b"capability";

/// Build inline xattrs with both SELinux and capabilities
pub fn build_security_xattrs(selinux_context: Option<&str>, capabilities: Option<&str>) -> Option<Vec<u8>> {
    // Collect attributes to write
    let mut attrs: Vec<(&[u8], Vec<u8>)> = Vec::new();

    // Add SELinux context if present
    if let Some(ctx) = selinux_context {
        if !ctx.is_empty() {
            let mut value = ctx.as_bytes().to_vec();
            if !value.ends_with(&[0]) {
                value.push(0);
            }
            attrs.push((XATTR_SELINUX_SUFFIX, value));
        }
    }

    // Add capabilities if present
    if let Some(caps) = capabilities {
        if !caps.is_empty() {
            // Capabilities are hex-encoded, decode them
            if let Some(value) = decode_capabilities(caps) {
                attrs.push((XATTR_CAPABILITY_SUFFIX, value));
            }
        }
    }

    if attrs.is_empty() {
        return None;
    }

    // Calculate total size
    let header_size = 4; // magic
    let mut entries_size = 0;
    let mut values_size = 0;

    for (name, value) in &attrs {
        let entry_base = 16; // fixed header
        let entry_total = entry_base + name.len();
        let entry_padded = (entry_total + 3) & !3;
        entries_size += entry_padded;
        values_size += value.len();
    }

    let terminator_size = 4;
    let xattr_data_size = header_size + entries_size + terminator_size + values_size;

    const MAX_INLINE_XATTR_SIZE: usize = 100;
    if xattr_data_size > MAX_INLINE_XATTR_SIZE {
        eprintln!("Warning: combined xattrs too large: {} > {} bytes", xattr_data_size, MAX_INLINE_XATTR_SIZE);
        return None;
    }

    let mut xattr = vec![0u8; xattr_data_size];

    // Write magic
    LittleEndian::write_u32(&mut xattr[0..4], XATTR_MAGIC);

    // Write entries and collect value offsets
    let mut pos = 4;
    // e_value_offs is relative to after the magic header (offset 4), not offset 0
    let mut value_offset = (entries_size + terminator_size) as u16;

    for (name, value) in &attrs {
        let name_len = name.len() as u8;
        let value_size = value.len() as u32;

        // Write entry header
        xattr[pos] = name_len;
        pos += 1;
        xattr[pos] = XATTR_SECURITY_PREFIX;
        pos += 1;
        LittleEndian::write_u16(&mut xattr[pos..pos+2], value_offset);
        pos += 2;
        LittleEndian::write_u32(&mut xattr[pos..pos+4], 0); // e_value_inum
        pos += 4;
        LittleEndian::write_u32(&mut xattr[pos..pos+4], value_size);
        pos += 4;
        LittleEndian::write_u32(&mut xattr[pos..pos+4], 0); // e_hash
        pos += 4;

        // Write name
        xattr[pos..pos + name_len as usize].copy_from_slice(name);
        pos += name_len as usize;

        // Pad to 4-byte boundary
        let entry_total = 16 + name_len as usize;
        let padding = ((entry_total + 3) & !3) - entry_total;
        pos += padding;

        // Update value offset for next entry
        value_offset += value_size as u16;
    }

    // Write terminator
    xattr[pos..pos + 4].fill(0);
    pos += 4;

    // Write values
    for (_, value) in &attrs {
        xattr[pos..pos + value.len()].copy_from_slice(value);
        pos += value.len();
    }


    Some(xattr)
}

/// Decode hex-encoded capabilities
/// Format: 0x<hex_string>
fn decode_capabilities(caps_str: &str) -> Option<Vec<u8>> {
    let hex_str = caps_str.strip_prefix("0x").or(caps_str.strip_prefix("0X"))?;

    // Decode hex string
    let mut bytes = Vec::new();
    for chunk in hex_str.as_bytes().chunks(2) {
        if chunk.len() != 2 {
            return None;
        }
        let hex_byte = std::str::from_utf8(chunk).ok()?;
        let byte = u8::from_str_radix(hex_byte, 16).ok()?;
        bytes.push(byte);
    }

    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_selinux_only() {
        let xattr = build_security_xattrs(Some("u:object_r:vendor_file:s0"), None)
            .expect("should build");
        assert!(xattr.len() > 4);
        assert_eq!(LittleEndian::read_u32(&xattr[0..4]), XATTR_MAGIC);
    }

    #[test]
    fn test_capabilities_only() {
        let xattr = build_security_xattrs(None, Some("0x0000000a00002000"))
            .expect("should build");
        assert!(xattr.len() > 4);
    }

    #[test]
    fn test_both() {
        let xattr = build_security_xattrs(
            Some("u:object_r:system_file:s0"),
            Some("0x0000000a00002000")
        ).expect("should build");
        assert!(xattr.len() > 4);
    }

    #[test]
    fn test_decode_capabilities() {
        let caps = decode_capabilities("0x0000000a00002000").expect("should decode");
        assert_eq!(caps.len(), 8);
        assert_eq!(caps, vec![0x00, 0x00, 0x00, 0x0a, 0x00, 0x00, 0x20, 0x00]);
    }
}
