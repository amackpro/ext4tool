use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Parsed fs_config entry with all fields
#[derive(Debug, Clone)]
pub struct FsConfigEntry {
    pub uid: u32,
    pub gid: u32,
    pub mode: u16,
    pub capabilities: Option<String>,
}

/// Parse an Android fs_config file into a path→FsConfigEntry map.
/// Format: `<rel_path> [symlink_target] <uid> <gid> <mode> [capabilities=...]`
///
/// Examples:
///   system/bin/app_process 0 2000 0755
///   system/bin/ping 0 0 0755 capabilities=0x4000000a
///   system/lib/libcutils.so 0 0 0644
pub fn parse_fs_config<P: AsRef<Path>>(path: P) -> Result<HashMap<String, FsConfigEntry>> {
    let content = fs::read_to_string(path.as_ref())
        .with_context(|| format!("Failed to read fs_config: {}", path.as_ref().display()))?;
    let mut map = HashMap::new();

    for (line_num, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 {
            eprintln!("Warning: fs_config line {}: too few fields, skipping", line_num + 1);
            continue;
        }

        // Parse path (first field)
        let path = fields[0].to_string();

        // Parse capabilities if present (last field starting with "capabilities=")
        let mut capabilities = None;
        let mut last_idx = fields.len() - 1;

        if fields[last_idx].starts_with("capabilities=") {
            if let Some(caps) = fields[last_idx].strip_prefix("capabilities=") {
                capabilities = Some(caps.to_string());
            }
            last_idx -= 1;
        }

        // Now parse uid, gid, mode from the end (before capabilities)
        // Format: ... <uid> <gid> <mode> [capabilities=...]
        if last_idx < 2 {
            eprintln!("Warning: fs_config line {}: invalid format, skipping", line_num + 1);
            continue;
        }

        let mode_str = fields[last_idx];
        let gid_str = fields[last_idx - 1];
        let uid_str = fields[last_idx - 2];

        // Parse mode (octal)
        let mode = u16::from_str_radix(mode_str.trim_start_matches('0'), 8)
            .with_context(|| format!("fs_config line {}: bad mode '{}'", line_num + 1, mode_str))?;

        // Parse uid and gid
        let uid: u32 = uid_str.parse()
            .with_context(|| format!("fs_config line {}: bad uid '{}'", line_num + 1, uid_str))?;
        let gid: u32 = gid_str.parse()
            .with_context(|| format!("fs_config line {}: bad gid '{}'", line_num + 1, gid_str))?;

        map.insert(path, FsConfigEntry {
            uid,
            gid,
            mode,
            capabilities,
        });
    }

    Ok(map)
}

/// Parse an Android file_contexts file into a list of (pattern, context).
/// Format: `<regex_pattern> <selinux_context>`, one per line.
pub fn parse_file_contexts<P: AsRef<Path>>(path: P) -> Result<Vec<(String, String)>> {
    let content = fs::read_to_string(path.as_ref())
        .with_context(|| format!("Failed to read file_contexts: {}", path.as_ref().display()))?;
    let mut entries = Vec::new();
    for (lineno, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Split on whitespace; the first token is the regex, the rest is the context
        let mut parts = line.splitn(2, |c: char| c.is_whitespace());
        let pattern = match parts.next() {
            Some(p) => p.to_string(),
            None => continue,
        };
        let ctx = match parts.next() {
            Some(c) => c.trim().to_string(),
            None => {
                eprintln!("Warning: file_contexts:{}: no context found, skipping", lineno + 1);
                continue;
            }
        };
        entries.push((pattern, ctx));
    }
    Ok(entries)
}
