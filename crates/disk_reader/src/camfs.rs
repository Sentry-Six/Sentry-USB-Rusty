//! Filesystem inside the CAM partition of a snapshot image: exFAT when the
//! Pi was set up with USE_EXFAT and kernel support (via the `exfat` crate),
//! FAT32 otherwise (via `fatfs`).

use std::{
    io::{self, Read, Seek, SeekFrom, Write},
    sync::Mutex,
};

use anyhow::{anyhow, bail, Context, Result};
use exfat::directory::Item;

#[derive(Clone, Debug)]
pub struct CamEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

/// What filesystem the CAM partition carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CamFsKind {
    Fat32,
    Exfat,
}

pub fn sniff<R: Read + Seek>(part: &mut R) -> Result<CamFsKind> {
    let mut boot = [0u8; 512];
    part.seek(SeekFrom::Start(0)).context("seek to boot sector")?;
    part.read_exact(&mut boot).context("read boot sector")?;
    if &boot[3..11] == b"EXFAT   " {
        Ok(CamFsKind::Exfat)
    } else if &boot[82..90] == b"FAT32   " {
        Ok(CamFsKind::Fat32)
    } else {
        bail!("CAM partition is neither exFAT nor FAT32");
    }
}

/// `fatfs` insists on `Read + Write + Seek` even for read-only use; this
/// adapter satisfies the bound while guaranteeing we can never write.
pub struct ReadOnly<T>(pub T);

impl<T: Read> Read for ReadOnly<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl<T: Seek> Seek for ReadOnly<T> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.0.seek(pos)
    }
}

impl<T> Write for ReadOnly<T> {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "disk_reader is strictly read-only",
        ))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A read-only view of a CAM filesystem.
pub enum CamFs<T: Read + Seek + Send + 'static> {
    Fat32(fatfs::FileSystem<ReadOnly<T>>),
    // The `exfat` crate hands out a root item list; walking re-opens
    // directories on demand. File opens need `&mut`, hence the Mutex.
    Exfat(Mutex<Vec<Item<T>>>),
}

impl<T: Read + Seek + Send + 'static> CamFs<T> {
    pub fn open(mut part: T) -> Result<Self> {
        match sniff(&mut part)? {
            CamFsKind::Fat32 => {
                part.seek(SeekFrom::Start(0))?;
                let fs = fatfs::FileSystem::new(
                    ReadOnly(part),
                    fatfs::FsOptions::new().update_accessed_date(false),
                )
                .context("open FAT32 CAM filesystem")?;
                Ok(CamFs::Fat32(fs))
            }
            CamFsKind::Exfat => {
                part.seek(SeekFrom::Start(0))?;
                let fs = exfat::ExFat::open(part)
                    .map_err(|e| anyhow!("open exFAT CAM filesystem: {e}"))?;
                Ok(CamFs::Exfat(Mutex::new(fs.into_iter().collect())))
            }
        }
    }

    /// Open the directory at `dirs` (a component list) starting from the
    /// exFAT root, returning its freshly-read item list. Returns `None`
    /// when `dirs` is empty (i.e. the caller wants the root itself, which
    /// it already holds). Matching is case-insensitive, like the
    /// filesystem.
    fn exfat_dir_items(root: &[Item<T>], dirs: &[&str]) -> Result<Option<Vec<Item<T>>>> {
        let Some((first, rest)) = dirs.split_first() else {
            return Ok(None);
        };
        let mut items = Self::exfat_open_subdir(root, first)?;
        for comp in rest {
            items = Self::exfat_open_subdir(&items, comp)?;
        }
        Ok(Some(items))
    }

    fn exfat_open_subdir(level: &[Item<T>], name: &str) -> Result<Vec<Item<T>>> {
        for item in level {
            if let Item::Directory(d) = item {
                if d.name().eq_ignore_ascii_case(name) {
                    return d.open().map_err(|e| anyhow!("open dir {name}: {e}"));
                }
            }
        }
        bail!("no such directory: {name}")
    }

    fn exfat_entries(items: &[Item<T>]) -> Vec<CamEntry> {
        items
            .iter()
            .map(|i| match i {
                Item::Directory(d) => CamEntry {
                    name: d.name().to_string(),
                    is_dir: true,
                    size: 0,
                },
                Item::File(f) => CamEntry {
                    name: f.name().to_string(),
                    is_dir: false,
                    size: f.len(),
                },
            })
            .collect()
    }

    /// Read `len` bytes at `offset` from a file item list entry.
    fn exfat_read_from(
        items: &mut [Item<T>],
        file_name: &str,
        offset: u64,
        len: Option<usize>,
    ) -> Result<Vec<u8>> {
        let file = items
            .iter_mut()
            .find_map(|i| match i {
                Item::File(f) if f.name().eq_ignore_ascii_case(file_name) => Some(f),
                _ => None,
            })
            .ok_or_else(|| anyhow!("no such file: {file_name}"))?;
        let file_len = file.len();
        let reader = file
            .open()
            .map_err(|e| anyhow!("open {file_name}: {e}"))?;
        let Some(mut reader) = reader else {
            return Ok(Vec::new()); // empty file
        };
        let want = match len {
            Some(l) => l.min(file_len.saturating_sub(offset) as usize),
            None => file_len.saturating_sub(offset) as usize,
        };
        if offset > 0 {
            reader
                .seek(SeekFrom::Start(offset))
                .with_context(|| format!("seek {file_name}"))?;
        }
        let mut buf = vec![0u8; want];
        let mut filled = 0;
        while filled < want {
            let n = reader
                .read(&mut buf[filled..])
                .with_context(|| format!("read {file_name}"))?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        buf.truncate(filled);
        Ok(buf)
    }

    /// Split "a/b/c.mp4" into (["a","b"], "c.mp4").
    fn split_parent(path: &str) -> Result<(Vec<&str>, &str)> {
        let mut comps: Vec<&str> = path
            .trim_matches('/')
            .split('/')
            .filter(|c| !c.is_empty())
            .collect();
        let file = comps.pop().ok_or_else(|| anyhow!("empty path"))?;
        Ok((comps, file))
    }

    pub fn read_dir(&self, path: &str) -> Result<Vec<CamEntry>> {
        match self {
            CamFs::Fat32(fs) => {
                let dir = if path.is_empty() || path == "/" {
                    fs.root_dir()
                } else {
                    fs.root_dir()
                        .open_dir(path)
                        .with_context(|| format!("open dir {path}"))?
                };
                let mut out = Vec::new();
                for entry in dir.iter() {
                    let entry = entry.context("read dir entry")?;
                    let name = entry.file_name();
                    if name == "." || name == ".." {
                        continue;
                    }
                    out.push(CamEntry {
                        name,
                        is_dir: entry.is_dir(),
                        size: entry.len(),
                    });
                }
                Ok(out)
            }
            CamFs::Exfat(root) => {
                let root = root.lock().unwrap();
                let dirs: Vec<&str> = path
                    .trim_matches('/')
                    .split('/')
                    .filter(|c| !c.is_empty())
                    .collect();
                match Self::exfat_dir_items(&root, &dirs)? {
                    Some(items) => Ok(Self::exfat_entries(&items)),
                    None => Ok(Self::exfat_entries(&root)),
                }
            }
        }
    }

    /// Read a whole file (used for small metadata like event.json).
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        match self {
            CamFs::Fat32(fs) => {
                let mut f = fs
                    .root_dir()
                    .open_file(path)
                    .with_context(|| format!("open file {path}"))?;
                let mut buf = Vec::new();
                f.read_to_end(&mut buf)
                    .with_context(|| format!("read {path}"))?;
                Ok(buf)
            }
            CamFs::Exfat(root) => {
                let mut root = root.lock().unwrap();
                let (dirs, file) = Self::split_parent(path)?;
                match Self::exfat_dir_items(&root, &dirs)? {
                    Some(mut items) => Self::exfat_read_from(&mut items, file, 0, None),
                    None => Self::exfat_read_from(&mut root, file, 0, None),
                }
            }
        }
    }

    /// Read `len` bytes at `offset` of a file (for Range streaming).
    pub fn read_file_range(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        match self {
            CamFs::Fat32(fs) => {
                let mut f = fs
                    .root_dir()
                    .open_file(path)
                    .with_context(|| format!("open file {path}"))?;
                f.seek(SeekFrom::Start(offset))
                    .with_context(|| format!("seek {path}"))?;
                let mut buf = vec![0u8; len];
                let mut filled = 0;
                while filled < len {
                    let n = f.read(&mut buf[filled..]).context("read range")?;
                    if n == 0 {
                        break;
                    }
                    filled += n;
                }
                buf.truncate(filled);
                Ok(buf)
            }
            CamFs::Exfat(root) => {
                let mut root = root.lock().unwrap();
                let (dirs, file) = Self::split_parent(path)?;
                match Self::exfat_dir_items(&root, &dirs)? {
                    Some(mut items) => Self::exfat_read_from(&mut items, file, offset, Some(len)),
                    None => Self::exfat_read_from(&mut root, file, offset, Some(len)),
                }
            }
        }
    }
}
