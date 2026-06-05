use crate::ext4::{Volume, Inode, FileType, EXT4_ROOT_INODE};
use anyhow::{Context, Result};
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};

fn unix_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[derive(Debug, Clone)]
pub struct FileInfo {
    pub path: PathBuf,
    pub inode_num: u32,
    pub uid: u32,
    pub gid: u32,
    pub mode: u32,
    pub size: u64,
    pub file_type: FileType,
}

#[derive(Debug, Clone)]
pub struct FsConfigEntry {
    pub path: String,
    pub uid: u32,
    pub gid: u32,
    pub mode: u32,
    pub capabilities: Option<String>,
    pub selabel: Option<String>,
}

pub struct Extractor {
    output_dir: PathBuf,
    partition_name: String,
    fs_config: Arc<Mutex<Vec<FsConfigEntry>>>,
    file_contexts: Arc<Mutex<HashMap<String, String>>>,
    files_with_spaces: Arc<Mutex<Vec<String>>>,
}

impl Extractor {
    pub fn new<P: AsRef<Path>>(output_dir: P, partition_name: String) -> Self {
        Extractor {
            output_dir: output_dir.as_ref().to_path_buf(),
            partition_name,
            fs_config: Arc::new(Mutex::new(Vec::new())),
            file_contexts: Arc::new(Mutex::new(HashMap::new())),
            files_with_spaces: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn extract(&self, image_path: &Path) -> Result<()> {
        println!("Opening ext4 image: {}", image_path.display());
        let mut volume = Volume::open(image_path)?;

        println!("Filesystem info:");
        println!("  Block size: {} bytes", volume.superblock.block_size);
        println!("  Total inodes: {}", volume.superblock.s_inodes_count);
        println!("  Total blocks: {}", volume.superblock.s_blocks_count);

        // Create output directory structure: output_dir/partition_name/
        let extract_dir = self.output_dir.join(&self.partition_name);
        fs::create_dir_all(&extract_dir)?;

        // Get root inode
        let root_inode = volume.get_inode(EXT4_ROOT_INODE)
            .context("Failed to read root inode (inode 2)")?;

        // Debug: Check root inode
        if std::env::var("DEBUG").is_ok() {
            println!("\nDEBUG: Root inode info:");
            println!("  Mode: 0o{:o} (type bits: 0o{:o})", root_inode.i_mode, root_inode.i_mode & 0o170000);
            println!("  Is directory: {}", root_inode.is_dir());
            println!("  Size: {} bytes", root_inode.i_size);
            println!("  Flags: 0x{:08X}", root_inode.i_flags);
        }

        if !root_inode.is_dir() {
            return Err(anyhow::anyhow!(
                "Root inode is not a directory (mode: 0o{:o}). Image may be corrupted.",
                root_inode.i_mode
            ));
        }

        println!("\nScanning filesystem...");
        let file_list = self.scan_filesystem(&mut volume, &root_inode, PathBuf::from(""), &extract_dir)?;

        println!("\nExtracting {} files in parallel...", file_list.len());
        self.extract_files_parallel(image_path, &file_list, &extract_dir)?;

        println!("\nGenerating metadata files...");
        self.write_metadata(image_path)?;

        println!("\nExtraction complete!");
        println!("  Extracted files: {}/{}/", self.output_dir.display(), self.partition_name);
        println!("  Metadata files: {}/config/", self.output_dir.display());
        Ok(())
    }

    fn scan_filesystem(
        &self,
        volume: &mut Volume,
        inode: &Inode,
        current_path: PathBuf,
        extract_dir: &Path,
    ) -> Result<Vec<FileInfo>> {
        let mut file_list = Vec::new();

        // Create directory
        if !current_path.as_os_str().is_empty() {
            let dir_path = extract_dir.join(&current_path);
            fs::create_dir_all(&dir_path)?;

            // Set permissions on Unix
            #[cfg(unix)]
            {
                let permissions = fs::Permissions::from_mode(inode.permissions());
                fs::set_permissions(&dir_path, permissions).ok();
            }

            // Add to fs_config
            let path_str = unix_path(&current_path);
            self.add_fs_config(&path_str, inode.i_uid, inode.i_gid, inode.permissions(), None, None);
        }

        // Read directory entries
        let entries = volume.read_dir(inode)?;

        for entry in entries {
            if entry.name == "." || entry.name == ".." {
                continue;
            }

            let entry_path = current_path.join(&entry.name);
            let entry_inode = volume.get_inode(entry.inode)?;

            // Check for spaces in filename
            if entry.name.contains(' ') {
                self.files_with_spaces.lock().unwrap().push(unix_path(&entry_path));
            }

            // Read extended attributes
            let xattrs = volume.read_xattrs(&entry_inode).unwrap_or_default();

            if std::env::var("DEBUG").is_ok() && xattrs.len() > 0 {
                println!("DEBUG: xattrs for {}: {:?}", entry.name, xattrs.keys().collect::<Vec<_>>());
            }

            let selinux_label = xattrs.get("security.selinux")
                .and_then(|v| String::from_utf8(v.clone()).ok())
                .map(|s| {
                    let trimmed = s.trim_end_matches('\0').to_string();
                    if std::env::var("DEBUG").is_ok() && entry.name == "lost+found" {
                        println!("DEBUG: SELinux label for {}: '{}'", entry.name, trimmed);
                    }
                    trimmed
                });

            let capabilities = xattrs.get("security.capability")
                .map(|v| hex::encode(v));

            match entry_inode.file_type() {
                FileType::Directory => {
                    // Recursively scan subdirectory
                    let mut subdir_files = self.scan_filesystem(volume, &entry_inode, entry_path.clone(), extract_dir)?;
                    file_list.append(&mut subdir_files);

                    // Add directory to contexts
                    if let Some(ref label) = selinux_label {
                        self.add_file_context(&unix_path(&entry_path), label, true);
                    }
                }
                FileType::RegularFile => {
                    // Add file to extraction list
                    file_list.push(FileInfo {
                        path: entry_path.clone(),
                        inode_num: entry.inode,
                        uid: entry_inode.i_uid,
                        gid: entry_inode.i_gid,
                        mode: entry_inode.permissions(),
                        size: entry_inode.i_size,
                        file_type: entry_inode.file_type(),
                    });

                    // Add to fs_config
                    self.add_fs_config(
                        &unix_path(&entry_path),
                        entry_inode.i_uid,
                        entry_inode.i_gid,
                        entry_inode.permissions(),
                        capabilities,
                        None, // target is only for symlinks
                    );

                    // Add to file_contexts
                    if let Some(ref label) = selinux_label {
                        self.add_file_context(&unix_path(&entry_path), label, false);
                    }
                }
                FileType::Symlink => {
                    let target_data = volume.read_inode_data(&entry_inode)?;
                    let target = String::from_utf8_lossy(&target_data).to_string();

                    let symlink_path = extract_dir.join(&entry_path);

                    // Create symlink
                    #[cfg(unix)]
                    {
                        if let Err(e) = symlink(&target, &symlink_path) {
                            eprintln!("Warning: Failed to create symlink {} -> {}: {}",
                                      symlink_path.display(), target, e);
                        }
                    }

                    #[cfg(windows)]
                    {
                        // Create Windows native reparse point (matching Python version)
                        let mut file = fs::File::create(&symlink_path)?;
                        file.write_all(b"!<symlink>\xff\xfe")?;
                        for c in target.chars() {
                            let mut buf = [0u8; 4];
                            let encoded = c.encode_utf8(&mut buf);
                            file.write_all(encoded.as_bytes())?;
                            file.write_all(&[0u8])?;
                        }
                        file.write_all(&[0u8; 2])?;
                        file.sync_all()?;
                        std::process::Command::new("attrib")
                            .arg("+s")
                            .arg(&symlink_path.as_os_str())
                            .output().ok();
                    }

                    // Add to fs_config with target
                    self.add_fs_config(
                        &unix_path(&entry_path),
                        entry_inode.i_uid,
                        entry_inode.i_gid,
                        entry_inode.permissions(),
                        capabilities,
                        Some(target),
                    );

                    // Add to file_contexts
                    if let Some(ref label) = selinux_label {
                        self.add_file_context(&unix_path(&entry_path), label, false);
                    }
                }
                _ => {
                    // Skip special files
                    eprintln!("Skipping special file: {}", entry_path.display());
                }
            }
        }

        Ok(file_list)
    }

    fn extract_files_parallel(&self, image_path: &Path, file_list: &[FileInfo], extract_dir: &Path) -> Result<()> {
        let total_files = file_list.len();
        let progress = Arc::new(Mutex::new(0usize));
        let extract_dir = extract_dir.to_path_buf();

        // Extract files in parallel
        file_list.par_iter().try_for_each(|file_info| -> Result<()> {
            // Open volume for this thread
            let mut volume = Volume::open(image_path)?;

            // Get inode and read data
            let inode = volume.get_inode(file_info.inode_num)?;
            let data = volume.read_inode_data(&inode)?;

            // Write file
            let output_path = extract_dir.join(&file_info.path);
            if let Some(parent) = output_path.parent() {
                fs::create_dir_all(parent)?;
            }

            fs::write(&output_path, &data)?;

            // Set permissions
            #[cfg(unix)]
            {
                let permissions = fs::Permissions::from_mode(file_info.mode);
                fs::set_permissions(&output_path, permissions).ok();

                // Try to set ownership (requires root)
                unsafe {
                    libc::chown(
                        std::ffi::CString::new(output_path.to_string_lossy().as_ref())?.as_ptr(),
                        file_info.uid,
                        file_info.gid,
                    );
                }
            }

            // Update progress
            let mut prog = progress.lock().unwrap();
            *prog += 1;
            if *prog % 100 == 0 || *prog == total_files {
                print!("\rExtracted: {}/{} files", *prog, total_files);
                std::io::stdout().flush().ok();
            }

            Ok(())
        })?;

        println!();
        Ok(())
    }

    fn add_fs_config(
        &self,
        path: &str,
        uid: u32,
        gid: u32,
        mode: u32,
        capabilities: Option<String>,
        target: Option<String>,
    ) {
        let mut config = self.fs_config.lock().unwrap();

        // Add partition prefix to path
        let full_path = if path.is_empty() {
            self.partition_name.clone()
        } else {
            format!("{}/{}", self.partition_name, path)
        };

        // Handle symlink targets in path
        let entry_path = if let Some(tgt) = target {
            format!("{} {}", full_path, tgt)
        } else {
            full_path
        };

        config.push(FsConfigEntry {
            path: entry_path,
            uid,
            gid,
            mode,
            capabilities,
            selabel: None,
        });
    }

    fn add_file_context(&self, path: &str, label: &str, is_dir: bool) {
        let mut contexts = self.file_contexts.lock().unwrap();

        // Escape regex special characters
        let mut safe = path.to_string();
        for c in ['\\', '^', '$', '.', '|', '?', '*', '+', '(', ')', '{', '}', '[', ']'] {
            safe = safe.replace(c, &format!("\\{}", c));
        }

        // Add /partition_name prefix to paths
        let full_path = format!("/{}/{}", self.partition_name, safe);

        // For directories, add both the path and a wildcard pattern
        if is_dir {
            contexts.insert(full_path.clone(), label.to_string());
            contexts.insert(format!("{}(/.*)?", full_path), label.to_string());
        } else {
            contexts.insert(full_path, label.to_string());
        }
    }

    fn write_metadata(&self, image_path: &Path) -> Result<()> {
        // Create config directory inside output_dir
        let config_dir = self.output_dir.join("config");
        fs::create_dir_all(&config_dir)?;

        // Get image size
        let image_size = fs::metadata(image_path)?.len();

        // Write fs_config
        let fs_config_path = config_dir.join(format!("{}_fs_config", self.partition_name));
        let mut fs_config = self.fs_config.lock().unwrap();

        // Deduplicate and sort
        fs_config.sort_by(|a, b| a.path.cmp(&b.path));
        fs_config.dedup_by(|a, b| a.path == b.path);

        // Add root and partition entries at the top
        let mut config_file = fs::File::create(&fs_config_path)?;
        writeln!(config_file, "/ 0 0 0755")?;
        writeln!(config_file, "{} 0 0 0755", self.partition_name)?;

        for entry in fs_config.iter() {
            let mut line = format!("{} {} {} 0{:o}", entry.path, entry.uid, entry.gid, entry.mode);

            if let Some(ref caps) = entry.capabilities {
                line.push_str(&format!(" capabilities={}", caps));
            }

            writeln!(config_file, "{}", line)?;
        }
        println!("  Generated: {}", fs_config_path.display());

        // Write file_contexts
        let file_contexts_path = config_dir.join(format!("{}_file_contexts", self.partition_name));
        let contexts = self.file_contexts.lock().unwrap();

        let mut contexts_file = fs::File::create(&file_contexts_path)?;
        let mut sorted_contexts: Vec<_> = contexts.iter().collect();
        sorted_contexts.sort_by(|a, b| a.0.cmp(b.0));

        // Find the partition root context (from /bin or any other entry)
        let partition_context = sorted_contexts.iter()
            .find(|(path, _)| path.contains(&format!("/{}/bin", self.partition_name)))
            .map(|(_, label)| label.as_str())
            .or_else(|| sorted_contexts.first().map(|(_, label)| label.as_str()));

        // Add partition root entries at the top
        if let Some(ctx) = partition_context {
            writeln!(contexts_file, "/{} {}", self.partition_name, ctx)?;
            writeln!(contexts_file, "/{}(/.*)? {}", self.partition_name, ctx)?;
        }

        for (path, label) in sorted_contexts {
            writeln!(contexts_file, "{} {}", path, label)?;
        }
        println!("  Generated: {}", file_contexts_path.display());

        // Write size file
        let size_path = config_dir.join(format!("{}_size.txt", self.partition_name));
        fs::write(&size_path, format!("{}", image_size))?;
        println!("  Generated: {}", size_path.display());

        // Write name file
        let name_path = config_dir.join(format!("{}_name.txt", self.partition_name));
        fs::write(&name_path, &self.partition_name)?;
        println!("  Generated: {}", name_path.display());

        // Write spaces file
        let spaces = self.files_with_spaces.lock().unwrap();
        if !spaces.is_empty() {
            let spaces_path = config_dir.join(format!("{}_space.txt", self.partition_name));
            let mut spaces_file = fs::File::create(&spaces_path)?;
            for path in spaces.iter() {
                writeln!(spaces_file, "{}", path)?;
            }
            println!("  Generated: {}", spaces_path.display());
        }

        Ok(())
    }
}
