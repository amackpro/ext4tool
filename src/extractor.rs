use crate::ext4::{Volume, Inode, FileType, EXT4_ROOT_INODE};
use anyhow::{Context, Result};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};

fn unix_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

struct FileTask {
    path: PathBuf,
    inode_num: u32,
    uid: u32,
    gid: u32,
    mode: u32,
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
    num_threads: usize,
}

impl Extractor {
    pub fn new<P: AsRef<Path>>(output_dir: P, partition_name: String, num_threads: usize) -> Self {
        Extractor {
            output_dir: output_dir.as_ref().to_path_buf(),
            partition_name,
            fs_config: Arc::new(Mutex::new(Vec::new())),
            file_contexts: Arc::new(Mutex::new(HashMap::new())),
            files_with_spaces: Arc::new(Mutex::new(Vec::new())),
            num_threads,
        }
    }

    pub fn extract(&self, image_path: &Path) -> Result<()> {
        println!("Opening ext4 image: {}", image_path.display());
        let mut volume = Volume::open(image_path)?;

        println!("Filesystem info:");
        println!("  Block size: {} bytes", volume.superblock.block_size);
        println!("  Total inodes: {}", volume.superblock.s_inodes_count);
        println!("  Total blocks: {}", volume.superblock.s_blocks_count);

        let extract_dir = self.output_dir.join(&self.partition_name);
        fs::create_dir_all(&extract_dir)?;

        let root_inode = volume.get_inode(EXT4_ROOT_INODE)
            .context("Failed to read root inode (inode 2)")?;

        if !root_inode.is_dir() {
            return Err(anyhow::anyhow!(
                "Root inode is not a directory (mode: 0o{:o}). Image may be corrupted.",
                root_inode.i_mode
            ));
        }

        // Shared work queue
        let queue: Arc<Mutex<VecDeque<FileTask>>> = Arc::new(Mutex::new(VecDeque::new()));
        let cvar = Arc::new(Condvar::new());
        let all_done = Arc::new(Mutex::new(false));
        let extracted = Arc::new(Mutex::new(0usize));

        println!("\nExtracting files...");

        // Spawn worker threads
        let mut handles = Vec::new();
        for _ in 0..self.num_threads {
            let queue = queue.clone();
            let cvar = cvar.clone();
            let all_done = all_done.clone();
            let extracted = extracted.clone();
            let image_path = image_path.to_path_buf();
            let extract_dir = extract_dir.clone();

            handles.push(std::thread::spawn(move || -> Result<()> {
                let mut vol = Volume::open(&image_path)?;

                loop {
                    let task = {
                        let mut q = queue.lock().unwrap();
                        loop {
                            if let Some(t) = q.pop_front() {
                                break Some(t);
                            }
                            if *all_done.lock().unwrap() {
                                return Ok(());
                            }
                            q = cvar.wait(q).unwrap();
                        }
                    };

                    if let Some(t) = task {
                        let inode = vol.get_inode(t.inode_num)?;
                        let data = vol.read_inode_data(&inode)?;

                        let out_path = extract_dir.join(&t.path);
                        if let Some(parent) = out_path.parent() {
                            fs::create_dir_all(parent)?;
                        }
                        fs::write(&out_path, &data)?;

                        #[cfg(unix)]
                        {
                            let perms = fs::Permissions::from_mode(t.mode);
                            fs::set_permissions(&out_path, perms).ok();
                            unsafe {
                                libc::chown(
                                    std::ffi::CString::new(out_path.to_string_lossy().as_ref())?.as_ptr(),
                                    t.uid,
                                    t.gid,
                                );
                            }
                        }

                        let mut n = extracted.lock().unwrap();
                        *n += 1;
                        if *n % 100 == 0 {
                            print!("\rExtracted: {} files", *n);
                            std::io::stdout().flush().ok();
                        }
                    }
                }
            }));
        }

        // Traverse filesystem (single thread), push file tasks to queue
        self.traverse(&mut volume, &root_inode, PathBuf::from(""), &extract_dir, &queue, &cvar)?;

        // Signal workers to finish
        *all_done.lock().unwrap() = true;
        cvar.notify_all();

        // Wait for all workers to finish
        for h in handles {
            h.join().unwrap()?;
        }

        println!();

        println!("\nGenerating metadata files...");
        self.write_metadata(image_path)?;

        let count = *extracted.lock().unwrap();
        println!("\nExtraction complete! ({count} files)");
        println!("  Extracted files: {}/{}/", self.output_dir.display(), self.partition_name);
        println!("  Metadata files: {}/config/", self.output_dir.display());
        Ok(())
    }

    fn traverse(
        &self,
        volume: &mut Volume,
        inode: &Inode,
        current_path: PathBuf,
        extract_dir: &Path,
        queue: &Arc<Mutex<VecDeque<FileTask>>>,
        cvar: &Condvar,
    ) -> Result<()> {
        if !current_path.as_os_str().is_empty() {
            let dir_path = extract_dir.join(&current_path);
            fs::create_dir_all(&dir_path)?;

            #[cfg(unix)]
            {
                let permissions = fs::Permissions::from_mode(inode.permissions());
                fs::set_permissions(&dir_path, permissions).ok();
            }

            let path_str = unix_path(&current_path);
            self.add_fs_config(&path_str, inode.i_uid, inode.i_gid, inode.permissions(), None, None);
        }

        let entries = volume.read_dir(inode)?;

        for entry in entries {
            if entry.name == "." || entry.name == ".." {
                continue;
            }

            let entry_path = current_path.join(&entry.name);
            let entry_inode = volume.get_inode(entry.inode)?;

            if entry.name.contains(' ') {
                self.files_with_spaces.lock().unwrap().push(unix_path(&entry_path));
            }

            let xattrs = volume.read_xattrs(&entry_inode).unwrap_or_default();

            let selinux_label = xattrs.get("security.selinux")
                .and_then(|v| String::from_utf8(v.clone()).ok())
                .map(|s| s.trim_end_matches('\0').to_string());

            let capabilities = xattrs.get("security.capability")
                .map(|v| hex::encode(v));

            match entry_inode.file_type() {
                FileType::Directory => {
                    self.traverse(volume, &entry_inode, entry_path.clone(), extract_dir, queue, cvar)?;

                    if let Some(ref label) = selinux_label {
                        self.add_file_context(&unix_path(&entry_path), label, true);
                    }
                }
                FileType::RegularFile => {
                    // Push to worker queue
                    let task = FileTask {
                        path: entry_path.clone(),
                        inode_num: entry.inode,
                        uid: entry_inode.i_uid,
                        gid: entry_inode.i_gid,
                        mode: entry_inode.permissions(),
                    };
                    queue.lock().unwrap().push_back(task);
                    cvar.notify_one();

                    self.add_fs_config(
                        &unix_path(&entry_path),
                        entry_inode.i_uid,
                        entry_inode.i_gid,
                        entry_inode.permissions(),
                        capabilities,
                        None,
                    );

                    if let Some(ref label) = selinux_label {
                        self.add_file_context(&unix_path(&entry_path), label, false);
                    }
                }
                FileType::Symlink => {
                    let target_data = volume.read_inode_data(&entry_inode)?;
                    let target = String::from_utf8_lossy(&target_data).to_string();

                    let symlink_path = extract_dir.join(&entry_path);

                    #[cfg(unix)]
                    {
                        if let Err(e) = symlink(&target, &symlink_path) {
                            eprintln!("Warning: Failed to create symlink {} -> {}: {}",
                                      symlink_path.display(), target, e);
                        }
                    }

                    #[cfg(windows)]
                    {
                        use std::os::windows::ffi::OsStrExt;
                        let target_wide: Vec<u16> = std::ffi::OsStr::new(&target)
                            .encode_wide()
                            .chain(std::iter::once(0))
                            .collect();
                        let mut reparse = b"!<symlink>\xff\xfe".to_vec();
                        for &cu in &target_wide {
                            reparse.extend_from_slice(&cu.to_le_bytes());
                        }
                        if let Err(e) = fs::write(&symlink_path, &reparse) {
                            eprintln!("Warning: Failed to write symlink {}: {}", symlink_path.display(), e);
                        } else {
                            std::process::Command::new("attrib")
                                .arg("+s")
                                .arg(&symlink_path.as_os_str())
                                .output().ok();
                        }
                    }

                    self.add_fs_config(
                        &unix_path(&entry_path),
                        entry_inode.i_uid,
                        entry_inode.i_gid,
                        entry_inode.permissions(),
                        capabilities,
                        Some(target),
                    );

                    if let Some(ref label) = selinux_label {
                        self.add_file_context(&unix_path(&entry_path), label, false);
                    }
                }
                _ => {
                    eprintln!("Skipping special file: {}", entry_path.display());
                }
            }
        }

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

        let full_path = if path.is_empty() {
            self.partition_name.clone()
        } else {
            format!("{}/{}", self.partition_name, path)
        };

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

        let mut safe = path.to_string();
        for c in ['\\', '^', '$', '.', '|', '?', '*', '+', '(', ')', '{', '}', '[', ']'] {
            safe = safe.replace(c, &format!("\\{}", c));
        }

        let full_path = format!("/{}/{}", self.partition_name, safe);

        if is_dir {
            contexts.insert(full_path.clone(), label.to_string());
            contexts.insert(format!("{}(/.*)?", full_path), label.to_string());
        } else {
            contexts.insert(full_path, label.to_string());
        }
    }

    fn write_metadata(&self, image_path: &Path) -> Result<()> {
        let config_dir = self.output_dir.join("config");
        fs::create_dir_all(&config_dir)?;

        let image_size = fs::metadata(image_path)?.len();

        let fs_config_path = config_dir.join(format!("{}_fs_config", self.partition_name));
        let mut fs_config = self.fs_config.lock().unwrap();

        fs_config.sort_by(|a, b| a.path.cmp(&b.path));
        fs_config.dedup_by(|a, b| a.path == b.path);

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

        let file_contexts_path = config_dir.join(format!("{}_file_contexts", self.partition_name));
        let contexts = self.file_contexts.lock().unwrap();

        let mut contexts_file = fs::File::create(&file_contexts_path)?;
        let mut sorted_contexts: Vec<_> = contexts.iter().collect();
        sorted_contexts.sort_by(|a, b| a.0.cmp(b.0));

        let partition_context = sorted_contexts.iter()
            .find(|(path, _)| path.contains(&format!("/{}/bin", self.partition_name)))
            .map(|(_, label)| label.as_str())
            .or_else(|| sorted_contexts.first().map(|(_, label)| label.as_str()));

        if let Some(ctx) = partition_context {
            writeln!(contexts_file, "/{} {}", self.partition_name, ctx)?;
            writeln!(contexts_file, "/{}(/.*)? {}", self.partition_name, ctx)?;
        }

        for (path, label) in sorted_contexts {
            writeln!(contexts_file, "{} {}", path, label)?;
        }
        println!("  Generated: {}", file_contexts_path.display());

        let size_path = config_dir.join(format!("{}_size.txt", self.partition_name));
        fs::write(&size_path, format!("{}", image_size))?;
        println!("  Generated: {}", size_path.display());

        let name_path = config_dir.join(format!("{}_name.txt", self.partition_name));
        fs::write(&name_path, &self.partition_name)?;
        println!("  Generated: {}", name_path.display());

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
