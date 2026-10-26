//! An exclusive lock on a small state file, shared by every EternalMonitor
//! process of the user. The lock belongs to the open handle, so a holder that
//! crashes releases it. Callers only try it; one that finds it taken tries
//! again later.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;

pub(crate) struct Locked(File);

/// Lock `path`, creating it. `WouldBlock` when another holder has it.
pub(crate) fn try_lock(path: &Path) -> io::Result<Locked> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Locked(file)),
        Err(TryLockError::WouldBlock) => Err(io::ErrorKind::WouldBlock.into()),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

/// Unlock explicitly: on Unix a child being started at that moment can still
/// share the handle, and with it the lock.
impl Drop for Locked {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_holder_is_turned_away_until_the_first_lets_go() {
        let path = std::env::temp_dir().join(format!(
            "eternal-lock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let first = try_lock(&path).unwrap();
        assert_eq!(
            try_lock(&path).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(first);
        // A child started by another test may briefly share the handle.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while try_lock(&path).is_err() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(try_lock(&path).is_ok());
        std::fs::remove_file(path).unwrap();
    }
}
