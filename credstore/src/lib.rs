//! The host's v3 credential store (design §4.3): pending and active records
//! per slot, tombstones, the wrapping key and its keyring, the invalidation
//! procedure and startup recovery.
//!
//! [`store::CredentialStore`] is the one writer. The session table reads
//! through [`store::StoreCredentials`], an in-memory snapshot that does no
//! I/O.

pub mod disk;
pub mod format;
pub mod keyring;
pub mod store;
#[cfg(feature = "test-hooks")]
pub mod testing;

pub use disk::{Disk, StdDisk};
pub use keyring::Keyring;
pub use store::{
    CredentialSnapshot, CredentialStore, Discarded, Finalize, NewRecord, OpenReport, RecordInfo,
    Recovery, StoreCredentials, StoreError,
};
