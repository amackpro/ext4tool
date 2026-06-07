use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Parse an Android fs_config file into a path→(uid,gid,mode) map.
/// Format: `<rel_path> [extra...] <uid> <gid> <mode> [capabilities=...]`
pub fn parse_fs_config<P: AsRef<Path>>(path: P) -> Result<HashMap<String, (u32, u32, u16)>> {
    let content = fs::read_to_string(path.as_ref())
        .with_context(|| format!("Failed to read fs_config: {}", path.as_ref().display()))?;
    let mut map = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }
        // The path is the first field
        let path = fields[0].to_string();
        // Scan from end: last field may be capabilities=...
        // Fields before that: uid, gid, mode (last 3 regular fields)
        let mut idx = fields.len() - 1;
        if fields[idx].starts_with("capabilities=") {
            idx -= 1;
        }
        // Now idx points to last field before optional capabilities
        // We need: ... uid gid mode [extra1 extra2 ...]
        // So the last 3 are mode, gid, uid
        if idx < 2 {
            continue;
        }
        let mode_str = fields[idx];
        let gid_str = fields[idx - 1];
        let uid_str = fields[idx - 2];

        let mode = u16::from_str_radix(mode_str.trim_start_matches("0"), 8)
            .map_err(|e| anyhow::anyhow!("Bad mode '{}': {}", mode_str, e))?;
        let uid: u32 = uid_str.parse()
            .map_err(|e| anyhow::anyhow!("Bad uid '{}': {}", uid_str, e))?;
        let gid: u32 = gid_str.parse()
            .map_err(|e| anyhow::anyhow!("Bad gid '{}': {}", gid_str, e))?;

        map.insert(path, (uid, gid, mode));
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
