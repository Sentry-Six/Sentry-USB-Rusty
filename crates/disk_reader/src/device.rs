//! Device access layer.
//!
//! Phase 1 supports plain image files, which already exercises the whole
//! parsing stack (`probe`/`ls`/`cat` on an image). Raw-device support
//! (macOS `/dev/rdiskN`, Windows `\\.\PhysicalDriveN` with sector-aligned
//! reads, Linux `/dev/sdX`) lands in Phase 2 behind this same trait.

use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};

/// A byte-addressable, cloneable, thread-safe view of a disk. Clones share
/// one file handle and serialize their positioned reads through a mutex.
#[derive(Clone)]
pub struct Disk {
    inner: Arc<Mutex<File>>,
    pub size: u64,
}

impl Disk {
    pub fn open_image(path: &Path) -> Result<Self> {
        let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let mut size = file
            .metadata()
            .with_context(|| format!("stat {}", path.display()))?
            .len();
        if size == 0 {
            // Block devices report len 0 via metadata (e.g. /dev/sdX on the
            // Pi); the seekable end is the real media size.
            size = file
                .seek(SeekFrom::End(0))
                .with_context(|| format!("size {}", path.display()))?;
            file.seek(SeekFrom::Start(0))?;
        }
        #[cfg(target_os = "macos")]
        if size == 0 {
            // macOS /dev/(r)diskN doesn't support SEEK_END; ask the driver.
            size = macos_media_size(&file)
                .with_context(|| format!("media size of {}", path.display()))?;
        }
        if size == 0 {
            anyhow::bail!("cannot determine size of {}", path.display());
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(file)),
            size,
        })
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let mut f = self.inner.lock().unwrap();
        f.seek(SeekFrom::Start(offset))?;
        f.read(buf)
    }

    /// A `Read + Seek` window over `[start, start+len)` of the disk.
    pub fn window(&self, start: u64, len: u64) -> Window {
        Window {
            disk: self.clone(),
            start,
            len,
            pos: 0,
        }
    }

    /// Window over the whole disk.
    pub fn whole(&self) -> Window {
        self.window(0, self.size)
    }
}

#[cfg(target_os = "macos")]
fn macos_media_size(file: &File) -> Result<u64> {
    use std::os::fd::AsRawFd;
    // <sys/disk.h>: DKIOCGETBLOCKSIZE = _IOR('d', 24, u32),
    //               DKIOCGETBLOCKCOUNT = _IOR('d', 25, u64)
    const DKIOCGETBLOCKSIZE: libc::c_ulong = 0x40046418;
    const DKIOCGETBLOCKCOUNT: libc::c_ulong = 0x40086419;
    let fd = file.as_raw_fd();
    let mut blocksize: u32 = 0;
    let mut blockcount: u64 = 0;
    // SAFETY: both ioctls only write to the out-param of the given size.
    let r1 = unsafe { libc::ioctl(fd, DKIOCGETBLOCKSIZE, &mut blocksize) };
    let r2 = unsafe { libc::ioctl(fd, DKIOCGETBLOCKCOUNT, &mut blockcount) };
    if r1 != 0 || r2 != 0 {
        anyhow::bail!("DKIOCGETBLOCKSIZE/COUNT ioctl failed (not a disk device?)");
    }
    Ok(blockcount * u64::from(blocksize))
}

pub struct Window {
    disk: Disk,
    start: u64,
    len: u64,
    pos: u64,
}

impl Window {
    pub fn len(&self) -> u64 {
        self.len
    }
}

impl Read for Window {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len {
            return Ok(0);
        }
        let want = buf.len().min((self.len - self.pos) as usize);
        let n = self.disk.read_at(self.start + self.pos, &mut buf[..want])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Window {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let newpos = match pos {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::End(off) => self.len.checked_add_signed(off),
            SeekFrom::Current(off) => self.pos.checked_add_signed(off),
        };
        match newpos {
            Some(p) => {
                self.pos = p;
                Ok(p)
            }
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek out of range",
            )),
        }
    }
}
