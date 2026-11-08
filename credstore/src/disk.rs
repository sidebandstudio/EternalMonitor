//! The file operations the store needs, behind a trait so tests can crash
//! between any two of them.
//!
//! A database write is always: write the temporary file and sync it, rename
//! it over the database, sync the directory. [`StdDisk`] does that with
//! `std::fs`; `MemDisk` (tests and fuzzing) keeps files in memory and can
//! fail at any call, losing whatever wasn't durable yet.

use std::io;
use std::path::PathBuf;

/// File operations in one directory. Names are plain file names.
pub trait Disk {
    /// The file's contents, or `None` if it doesn't exist.
    fn read(&self, name: &str) -> io::Result<Option<Vec<u8>>>;
    /// The names of the files in the directory.
    fn list(&self) -> io::Result<Vec<String>>;
    /// Creates or replaces `name` with `bytes`, synced: on disk before any
    /// later rename, and durable once [`sync_dir`](Self::sync_dir) returns.
    fn write_synced(&mut self, name: &str, bytes: &[u8]) -> io::Result<()>;
    /// Renames `from` over `to`. Durable only after [`sync_dir`](Self::sync_dir).
    fn rename(&mut self, from: &str, to: &str) -> io::Result<()>;
    /// Makes earlier renames and removals durable.
    fn sync_dir(&mut self) -> io::Result<()>;
    /// Removes `name` if it exists.
    fn remove(&mut self, name: &str) -> io::Result<()>;
}

/// [`Disk`] on a real directory.
pub struct StdDisk {
    dir: PathBuf,
}

impl StdDisk {
    /// Uses `dir`, creating it if needed.
    pub fn new(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }
}

impl Disk for StdDisk {
    fn read(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        match std::fs::read(self.dir.join(name)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn list(&self) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            if let Ok(name) = entry?.file_name().into_string() {
                names.push(name);
            }
        }
        Ok(names)
    }

    fn write_synced(&mut self, name: &str, bytes: &[u8]) -> io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::File::create(self.dir.join(name))?;
        file.write_all(bytes)?;
        // On macOS a barrier sync writes the file and orders it before the
        // rename; the directory's full sync then flushes the drive's cache
        // for both, one full flush per write instead of two.
        #[cfg(target_os = "macos")]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: fcntl on a file descriptor this function owns.
            if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_BARRIERFSYNC) } != -1 {
                return Ok(());
            }
        }
        file.sync_all()
    }

    #[cfg(not(windows))]
    fn rename(&mut self, from: &str, to: &str) -> io::Result<()> {
        std::fs::rename(self.dir.join(from), self.dir.join(to))
    }

    /// On Windows the rename is written through: durable when it returns,
    /// since a directory can't be synced there.
    #[cfg(windows)]
    fn rename(&mut self, from: &str, to: &str) -> io::Result<()> {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };
        let wide = |name: &str| {
            self.dir
                .join(name)
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect::<Vec<u16>>()
        };
        let (from, to) = (wide(from), wide(to));
        unsafe {
            // SAFETY: both paths are NUL-terminated and outlive the call.
            MoveFileExW(
                PCWSTR(from.as_ptr()),
                PCWSTR(to.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(io::Error::other)
    }

    fn sync_dir(&mut self) -> io::Result<()> {
        // On Windows the rename was written through instead.
        #[cfg(unix)]
        std::fs::File::open(&self.dir)?.sync_all()?;
        Ok(())
    }

    fn remove(&mut self, name: &str) -> io::Result<()> {
        match std::fs::remove_file(self.dir.join(name)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

/// An in-memory [`Disk`] that can crash, for tests and fuzzing.
///
/// Durable state is what survives a crash: synced file contents, and
/// renames and removals followed by a directory sync. A crash at call `n`
/// makes that call and every later one fail; a write it interrupts leaves
/// the first half of its bytes. [`MemDisk::after_crash`] returns the durable
/// state, with unsynced renames either kept or lost.
#[cfg(any(test, feature = "test-hooks"))]
#[derive(Clone, Default)]
pub struct MemDisk {
    /// What a reader sees now.
    visible: std::collections::BTreeMap<String, Vec<u8>>,
    /// What survives a crash.
    durable: std::collections::BTreeMap<String, Vec<u8>>,
    calls: usize,
    crash_at: Option<usize>,
}

#[cfg(any(test, feature = "test-hooks"))]
impl MemDisk {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fails call number `n` (from 0) and every call after it.
    pub fn crash_at(&mut self, n: usize) {
        self.crash_at = Some(n);
    }

    /// Calls made so far.
    pub fn calls(&self) -> usize {
        self.calls
    }

    /// The disk after a crash and restart. With `keep_unsynced`, renames
    /// and removals since the last directory sync happened to reach the
    /// disk; without it, they're lost.
    pub fn after_crash(&self, keep_unsynced: bool) -> Self {
        let files = if keep_unsynced {
            self.visible.clone()
        } else {
            self.durable.clone()
        };
        Self {
            visible: files.clone(),
            durable: files,
            calls: 0,
            crash_at: None,
        }
    }

    /// The disk after the process restarts in the same boot: everything a
    /// reader saw is still there, and nothing more is durable.
    pub fn restart(&self) -> Self {
        Self {
            calls: 0,
            crash_at: None,
            ..self.clone()
        }
    }

    /// Replaces a file directly, for restore tests.
    pub fn put(&mut self, name: &str, bytes: Vec<u8>) {
        self.visible.insert(name.to_string(), bytes.clone());
        self.durable.insert(name.to_string(), bytes);
    }

    pub fn get(&self, name: &str) -> Option<&Vec<u8>> {
        self.visible.get(name)
    }

    /// Removes a file directly and durably, for restore tests.
    pub fn remove_now(&mut self, name: &str) {
        self.visible.remove(name);
        self.durable.remove(name);
    }

    /// What a crash in the middle of writing `name` leaves: the first half
    /// of `bytes`, which may have reached the disk.
    pub fn write_torn(&mut self, name: &str, bytes: &[u8]) {
        self.put(name, bytes[..bytes.len() / 2].to_vec());
    }

    fn step(&mut self) -> io::Result<()> {
        let n = self.calls;
        self.calls += 1;
        if self.crash_at.is_some_and(|at| n >= at) {
            return Err(io::Error::other("simulated crash"));
        }
        Ok(())
    }
}

#[cfg(any(test, feature = "test-hooks"))]
impl Disk for MemDisk {
    fn read(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(self.visible.get(name).cloned())
    }

    fn list(&self) -> io::Result<Vec<String>> {
        Ok(self.visible.keys().cloned().collect())
    }

    fn write_synced(&mut self, name: &str, bytes: &[u8]) -> io::Result<()> {
        if let Err(e) = self.step() {
            self.write_torn(name, bytes);
            return Err(e);
        }
        // The file's data is synced; its directory entry for a new file is
        // treated as durable too, which only matters for temporary files.
        self.visible.insert(name.to_string(), bytes.to_vec());
        self.durable.insert(name.to_string(), bytes.to_vec());
        Ok(())
    }

    fn rename(&mut self, from: &str, to: &str) -> io::Result<()> {
        self.step()?;
        let bytes = self
            .visible
            .remove(from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, from.to_string()))?;
        self.visible.insert(to.to_string(), bytes);
        Ok(())
    }

    fn sync_dir(&mut self) -> io::Result<()> {
        self.step()?;
        self.durable = self.visible.clone();
        Ok(())
    }

    fn remove(&mut self, name: &str) -> io::Result<()> {
        self.step()?;
        self.visible.remove(name);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn std_disk_round_trip() {
        let dir = std::env::temp_dir().join(format!("em-credstore-disk-{}", std::process::id()));
        let mut disk = StdDisk::new(&dir).unwrap();
        assert_eq!(disk.read("a").unwrap(), None);
        disk.write_synced("a.tmp", b"one").unwrap();
        disk.rename("a.tmp", "a").unwrap();
        disk.sync_dir().unwrap();
        assert_eq!(disk.read("a").unwrap().unwrap(), b"one");
        assert_eq!(disk.read("a.tmp").unwrap(), None);
        assert_eq!(disk.list().unwrap(), vec!["a".to_string()]);
        disk.remove("a").unwrap();
        disk.remove("a").unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mem_disk_loses_unsynced_renames_on_crash() {
        let mut disk = MemDisk::new();
        disk.write_synced("db.tmp", b"new").unwrap();
        disk.rename("db.tmp", "db").unwrap();
        let lost = disk.after_crash(false);
        assert_eq!(lost.get("db"), None);
        assert_eq!(lost.get("db.tmp").unwrap(), b"new");
        let kept = disk.after_crash(true);
        assert_eq!(kept.get("db").unwrap(), b"new");
        let mut disk = MemDisk::new();
        disk.crash_at(1);
        disk.write_synced("x", b"1").unwrap();
        assert!(disk.rename("x", "y").is_err());
        assert!(disk.sync_dir().is_err());
        assert!(disk.write_synced("x", b"abcd").is_err());
        assert_eq!(disk.after_crash(false).get("x").unwrap(), b"ab");
    }
}
