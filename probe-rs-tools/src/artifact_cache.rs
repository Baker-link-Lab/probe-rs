use anyhow::{Context, Result, bail};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::SystemTime;

pub const DEFAULT_LIMIT: u64 = 1024 * 1024 * 1024;

#[derive(Default)]
pub struct Cache {
    limits: HashMap<PathBuf, u64>,
    active: HashMap<PathBuf, usize>,
    uploads: HashSet<PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub struct Usage {
    pub bytes: u64,
    pub files: usize,
    pub active_files: usize,
    pub limit: u64,
}

struct Entry {
    path: PathBuf,
    bytes: u64,
    used: SystemTime,
}

static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

pub fn lock() -> Result<MutexGuard<'static, Cache>> {
    CACHE
        .get_or_init(|| Mutex::new(Cache::default()))
        .lock()
        .map_err(|_| anyhow::anyhow!("Artifact cache lock is poisoned"))
}

fn entries(root: &Path) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for project in fs::read_dir(root)? {
        let project = project?;
        if !project.file_type()?.is_dir() || !valid_id(&project.file_name().to_string_lossy()) {
            continue;
        }
        for file in fs::read_dir(project.path())? {
            let file = file?;
            if !file.file_type()?.is_file() || file.path().extension().is_none_or(|e| e != "elf") {
                continue;
            }
            if !file
                .path()
                .file_stem()
                .is_some_and(|stem| valid_id(&stem.to_string_lossy()))
            {
                continue;
            }
            let metadata = file.metadata()?;
            entries.push(Entry {
                path: fs::canonicalize(file.path())?,
                bytes: metadata.len(),
                used: metadata.modified()?,
            });
        }
    }
    entries.sort_by(|a, b| a.used.cmp(&b.used).then_with(|| a.path.cmp(&b.path)));
    Ok(entries)
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn cache_key(path: &Path) -> PathBuf {
    #[cfg(target_os = "windows")]
    if let Some(name) = path.file_name() {
        return path.with_file_name(name.to_string_lossy().to_ascii_lowercase());
    }
    path.to_path_buf()
}

impl Cache {
    pub fn limit(&self, root: &Path) -> u64 {
        self.limits.get(root).copied().unwrap_or(DEFAULT_LIMIT)
    }

    pub fn usage(&self, root: &Path) -> Result<Usage> {
        let entries = entries(root)?;
        Ok(Usage {
            bytes: entries.iter().map(|entry| entry.bytes).sum(),
            files: entries.len(),
            active_files: entries
                .iter()
                .filter(|entry| self.active.contains_key(&cache_key(&entry.path)))
                .count(),
            limit: self.limit(root),
        })
    }

    pub fn set_limit(&mut self, root: &Path, limit: u64) -> Result<Usage> {
        if limit == 0 {
            bail!("Artifact cache limit must be greater than zero");
        }
        self.clean_temporary_files(root)?;
        self.make_room(root, None, 0, limit)?;
        self.limits.insert(root.to_path_buf(), limit);
        self.usage(root)
    }

    fn clean_temporary_files(&self, root: &Path) -> Result<()> {
        for project in fs::read_dir(root)? {
            let project = project?;
            if !project.file_type()?.is_dir() || !valid_id(&project.file_name().to_string_lossy()) {
                continue;
            }
            let project_path = fs::canonicalize(project.path())?;
            for file in fs::read_dir(project.path())? {
                let file = file?;
                let name = file.file_name();
                let name = name.to_string_lossy();
                if file.file_type()?.is_file()
                    && !self
                        .uploads
                        .contains(&cache_key(&project_path.join(file.file_name())))
                    && name
                        .strip_prefix('.')
                        .and_then(|name| name.strip_suffix(".upload"))
                        .is_some_and(valid_id)
                {
                    fs::remove_file(file.path())?;
                }
            }
            if !self
                .uploads
                .iter()
                .any(|upload| upload.parent() == Some(project_path.as_path()))
                && fs::read_dir(&project_path)?.next().is_none()
            {
                fs::remove_dir(project.path())?;
            }
        }
        Ok(())
    }

    pub fn prepare_store(&mut self, root: &Path, destination: &Path, bytes: u64) -> Result<()> {
        let destination = match fs::canonicalize(destination) {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => destination.to_path_buf(),
            Err(error) => return Err(error.into()),
        };
        self.make_room(root, Some(&destination), bytes, self.limit(root))
    }

    fn make_room(
        &self,
        root: &Path,
        destination: Option<&Path>,
        bytes: u64,
        limit: u64,
    ) -> Result<()> {
        let entries = entries(root)?;
        let mut total = bytes;
        let mut candidates = Vec::new();
        for entry in entries {
            if destination == Some(entry.path.as_path()) {
                continue;
            }
            total = total
                .checked_add(entry.bytes)
                .context("Artifact cache size overflow")?;
            if !self.active.contains_key(&cache_key(&entry.path)) {
                candidates.push(entry);
            }
        }
        let reclaimable: u64 = candidates.iter().map(|entry| entry.bytes).sum();
        if total.saturating_sub(reclaimable) > limit {
            bail!(
                "Artifact cache is full: active artifacts and the upload exceed the limit. Stop debug sessions or increase the cache limit"
            );
        }
        for entry in candidates {
            if total <= limit {
                break;
            }
            fs::remove_file(&entry.path)
                .with_context(|| format!("Failed to evict {}", entry.path.display()))?;
            total -= entry.bytes;
            remove_empty_parent(&entry.path, &self.uploads)?;
        }
        Ok(())
    }

    pub fn clear_unused(&mut self, root: &Path) -> Result<Usage> {
        self.clean_temporary_files(root)?;
        for entry in entries(root)? {
            if !self.active.contains_key(&cache_key(&entry.path)) {
                fs::remove_file(&entry.path)
                    .with_context(|| format!("Failed to remove {}", entry.path.display()))?;
                remove_empty_parent(&entry.path, &self.uploads)?;
            }
        }
        self.usage(root)
    }
}

fn remove_empty_parent(path: &Path, uploads: &HashSet<PathBuf>) -> Result<()> {
    let parent = path.parent().context("Artifact has no parent")?;
    if !uploads.iter().any(|upload| upload.parent() == Some(parent))
        && fs::read_dir(parent)?.next().is_none()
    {
        fs::remove_dir(parent)?;
    }
    Ok(())
}

#[derive(Debug)]
pub struct Lease {
    path: PathBuf,
}

pub struct Upload {
    path: PathBuf,
}

impl Upload {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub fn begin_upload(root: &Path, path: &Path, bytes: u64) -> Result<Upload> {
    let mut cache = lock()?;
    if bytes > cache.limit(root) {
        bail!("Artifact exceeds the configured cache capacity");
    }
    let parent = path
        .parent()
        .context("Artifact upload has no parent directory")?;
    fs::create_dir_all(parent)?;
    if fs::symlink_metadata(parent)?.file_type().is_symlink() {
        bail!("Artifact project directory must not be a symbolic link");
    }
    let path = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::canonicalize(parent)?
            .join(
                path.file_name()
                    .context("Artifact upload has no file name")?,
            ),
        Err(error) => return Err(error.into()),
    };
    if !cache.uploads.insert(cache_key(&path)) {
        bail!("An upload for this artifact is already in progress");
    }
    Ok(Upload { path })
}

impl Drop for Upload {
    fn drop(&mut self) {
        match lock() {
            Ok(mut cache) => {
                cache.uploads.remove(&cache_key(&self.path));
            }
            Err(error) => tracing::error!("Failed to release artifact upload: {error}"),
        }
    }
}

pub fn acquire(path: &Path) -> Result<Arc<Lease>> {
    let mut cache = lock()?;
    let path = fs::canonicalize(path)?;
    fs::OpenOptions::new()
        .write(true)
        .open(&path)?
        .set_times(fs::FileTimes::new().set_modified(SystemTime::now()))?;
    let key = cache_key(&path);
    *cache.active.entry(key.clone()).or_default() += 1;
    Ok(Arc::new(Lease { path: key }))
}

impl Drop for Lease {
    fn drop(&mut self) {
        match lock() {
            Ok(mut cache) => {
                if let Some(count) = cache.active.get_mut(&self.path) {
                    *count -= 1;
                    if *count == 0 {
                        cache.active.remove(&self.path);
                    }
                }
            }
            Err(error) => tracing::error!("Failed to release artifact cache lease: {error}"),
        }
    }
}
