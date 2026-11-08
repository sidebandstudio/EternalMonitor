//! Where the keyring blob lives: one item in OS-protected storage, replaced
//! atomically.
//!
//! - macOS: a generic password in the data-protection Keychain, accessible
//!   after first unlock on this device only, never synchronized.
//! - Windows: one DPAPI-protected (current user) binary value under `HKCU`.
//!
//! [`MemKeyring`] (tests and fuzzing) keeps the blob in memory and can fail
//! at any write, which then never happened.

use std::io;

use eternal_wire::v3::Zeroizing;

/// One atomically replaced secret blob.
pub trait Keyring {
    /// The blob, or `None` if there isn't one.
    fn load(&self) -> io::Result<Option<Zeroizing<Vec<u8>>>>;
    /// Replaces the blob. Either the new blob is stored, or the old one
    /// still is.
    fn store(&mut self, blob: &[u8]) -> io::Result<()>;
}

/// An in-memory [`Keyring`] that can fail, for tests and fuzzing.
#[cfg(any(test, feature = "test-hooks"))]
#[derive(Clone, Default)]
pub struct MemKeyring {
    blob: Option<Vec<u8>>,
    calls: usize,
    crash_at: Option<usize>,
}

#[cfg(any(test, feature = "test-hooks"))]
impl MemKeyring {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fails store number `n` (from 0) and every store after it.
    pub fn crash_at(&mut self, n: usize) {
        self.crash_at = Some(n);
    }

    pub fn calls(&self) -> usize {
        self.calls
    }

    /// The keyring after a restart: the last stored blob, no crash pending.
    pub fn after_crash(&self) -> Self {
        Self {
            blob: self.blob.clone(),
            calls: 0,
            crash_at: None,
        }
    }

    /// Replaces or removes the blob directly, for restore tests.
    pub fn put(&mut self, blob: Option<Vec<u8>>) {
        self.blob = blob;
    }

    pub fn get(&self) -> Option<&Vec<u8>> {
        self.blob.as_ref()
    }
}

#[cfg(any(test, feature = "test-hooks"))]
impl Keyring for MemKeyring {
    fn load(&self) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
        Ok(self.blob.clone().map(Zeroizing::new))
    }

    fn store(&mut self, blob: &[u8]) -> io::Result<()> {
        let n = self.calls;
        self.calls += 1;
        if self.crash_at.is_some_and(|at| n >= at) {
            return Err(io::Error::other("simulated crash"));
        }
        self.blob = Some(blob.to_vec());
        Ok(())
    }
}

/// The macOS data-protection Keychain.
#[cfg(target_os = "macos")]
pub struct MacKeychain {
    service: String,
    account: String,
}

#[cfg(target_os = "macos")]
impl MacKeychain {
    /// One item, identified by `service` and `account`.
    pub fn new(service: &str, account: &str) -> Self {
        Self {
            service: service.to_string(),
            account: account.to_string(),
        }
    }

    fn options(&self) -> security_framework::passwords::PasswordOptions {
        let mut options = security_framework::passwords::PasswordOptions::new_generic_password(
            &self.service,
            &self.account,
        );
        options.use_protected_keychain();
        options.set_access_synchronized(Some(false));
        options
    }

    /// Removes the item, for tests.
    pub fn delete(&self) -> io::Result<()> {
        match security_framework::passwords::delete_generic_password_options(self.options()) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
            Err(e) => Err(io::Error::other(e)),
        }
    }
}

#[cfg(target_os = "macos")]
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

#[cfg(target_os = "macos")]
impl Keyring for MacKeychain {
    fn load(&self) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
        match security_framework::passwords::generic_password(self.options()) {
            Ok(blob) => Ok(Some(Zeroizing::new(blob))),
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
            Err(e) => Err(io::Error::other(e)),
        }
    }

    fn store(&mut self, blob: &[u8]) -> io::Result<()> {
        use security_framework::access_control::{ProtectionMode, SecAccessControl};
        let mut options = self.options();
        let access = SecAccessControl::create_with_protection(
            Some(ProtectionMode::AccessibleAfterFirstUnlockThisDeviceOnly),
            0,
        )
        .map_err(io::Error::other)?;
        options.set_access_control(access);
        security_framework::passwords::set_generic_password_options(blob, options)
            .map_err(io::Error::other)
    }
}

/// A DPAPI-protected binary value under `HKCU`, for the current user.
#[cfg(windows)]
pub struct DpapiRegistry {
    subkey: Vec<u16>,
    value: Vec<u16>,
}

#[cfg(windows)]
impl DpapiRegistry {
    /// The value `value` under `HKCU\<subkey>`.
    pub fn new(subkey: &str, value: &str) -> Self {
        let wide = |s: &str| {
            s.encode_utf16()
                .chain(std::iter::once(0))
                .collect::<Vec<u16>>()
        };
        Self {
            subkey: wide(subkey),
            value: wide(value),
        }
    }

    /// Removes the value, for tests.
    pub fn delete(&self) -> io::Result<()> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
        use windows::Win32::System::Registry::{RegDeleteKeyValueW, HKEY_CURRENT_USER};
        let status = unsafe {
            // SAFETY: both strings are NUL-terminated and outlive the call.
            RegDeleteKeyValueW(
                HKEY_CURRENT_USER,
                PCWSTR(self.subkey.as_ptr()),
                PCWSTR(self.value.as_ptr()),
            )
        };
        if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status.0 as i32))
        }
    }
}

#[cfg(windows)]
impl Keyring for DpapiRegistry {
    fn load(&self) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{
            LocalFree, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS, HLOCAL,
        };
        use windows::Win32::Security::Cryptography::{
            CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        };
        use windows::Win32::System::Registry::{
            RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_BINARY,
        };

        let mut len = 0u32;
        let read = |buf: Option<&mut Vec<u8>>, len: &mut u32| unsafe {
            // SAFETY: the strings are NUL-terminated; `buf`, when given, has
            // room for `len` bytes.
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(self.subkey.as_ptr()),
                PCWSTR(self.value.as_ptr()),
                RRF_RT_REG_BINARY,
                None,
                buf.map(|b| b.as_mut_ptr().cast()),
                Some(len),
            )
        };
        let status = read(None, &mut len);
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if status != ERROR_SUCCESS && status != ERROR_MORE_DATA {
            return Err(io::Error::from_raw_os_error(status.0 as i32));
        }
        let mut sealed = vec![0u8; len as usize];
        let status = read(Some(&mut sealed), &mut len);
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status.0 as i32));
        }
        sealed.truncate(len as usize);

        let input = CRYPT_INTEGER_BLOB {
            cbData: sealed.len() as u32,
            pbData: sealed.as_mut_ptr(),
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        unsafe {
            // SAFETY: `input` points at `sealed`, alive for the call; DPAPI
            // allocates `output`, freed below with LocalFree.
            CryptUnprotectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
        .map_err(io::Error::other)?;
        let blob = unsafe {
            // SAFETY: DPAPI returned `cbData` bytes at `pbData`.
            std::slice::from_raw_parts(output.pbData, output.cbData as usize)
        }
        .to_vec();
        unsafe {
            // SAFETY: wipe and free the buffer DPAPI allocated.
            std::ptr::write_bytes(output.pbData, 0, output.cbData as usize);
            let _ = LocalFree(HLOCAL(output.pbData.cast()));
        }
        Ok(Some(Zeroizing::new(blob)))
    }

    fn store(&mut self, blob: &[u8]) -> io::Result<()> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HLOCAL};
        use windows::Win32::Security::Cryptography::{
            CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        };
        use windows::Win32::System::Registry::{
            RegCloseKey, RegCreateKeyExW, RegFlushKey, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
            KEY_SET_VALUE, REG_BINARY, REG_OPTION_NON_VOLATILE,
        };

        let mut plain = Zeroizing::new(blob.to_vec());
        let input = CRYPT_INTEGER_BLOB {
            cbData: plain.len() as u32,
            pbData: plain.as_mut_ptr(),
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        unsafe {
            // SAFETY: `input` points at `plain`, alive for the call; DPAPI
            // allocates `output`, freed below with LocalFree.
            CryptProtectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
        .map_err(io::Error::other)?;
        // One RegSetValueExW call replaces the value atomically; the flush
        // makes it durable before the store's next step.
        let mut key = HKEY::default();
        let mut status = unsafe {
            // SAFETY: the subkey is NUL-terminated; `key` receives the handle.
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(self.subkey.as_ptr()),
                0,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut key,
                None,
            )
        };
        if status == ERROR_SUCCESS {
            unsafe {
                // SAFETY: `key` is open; the value name is NUL-terminated and
                // `output` holds `cbData` bytes.
                status = RegSetValueExW(
                    key,
                    PCWSTR(self.value.as_ptr()),
                    0,
                    REG_BINARY,
                    Some(std::slice::from_raw_parts(
                        output.pbData,
                        output.cbData as usize,
                    )),
                );
                if status == ERROR_SUCCESS {
                    status = RegFlushKey(key);
                }
                let _ = RegCloseKey(key);
            }
        }
        unsafe {
            // SAFETY: free the buffer DPAPI allocated.
            let _ = LocalFree(HLOCAL(output.pbData.cast()));
        }
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status.0 as i32))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_keyring_store_is_all_or_nothing() {
        let mut k = MemKeyring::new();
        assert!(k.load().unwrap().is_none());
        k.store(b"one").unwrap();
        k.crash_at(1);
        assert!(k.store(b"two").is_err());
        assert_eq!(&**k.after_crash().load().unwrap().unwrap(), b"one");
    }

    /// Needs a signed binary or an unlocked login session on some setups;
    /// run with `--ignored` on a Mac.
    // The data-protection Keychain needs a signed binary with a keychain
    // access group; a plain test binary gets errSecMissingEntitlement.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "needs a signed binary with a keychain access group"]
    fn mac_keychain_round_trip() {
        let mut k = MacKeychain::new(
            "EternalMonitor test",
            &format!("credstore-{}", std::process::id()),
        );
        k.delete().unwrap();
        assert!(k.load().unwrap().is_none());
        k.store(b"first").unwrap();
        k.store(b"second").unwrap();
        assert_eq!(&**k.load().unwrap().unwrap(), b"second");
        k.delete().unwrap();
        assert!(k.load().unwrap().is_none());
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_registry_round_trip() {
        let mut k = DpapiRegistry::new(
            r"Software\EternalMonitor-Test\credstore",
            &format!("keyring-{}", std::process::id()),
        );
        k.delete().unwrap();
        assert!(k.load().unwrap().is_none());
        k.store(b"first").unwrap();
        k.store(&[7u8; 300]).unwrap();
        assert_eq!(&**k.load().unwrap().unwrap(), &[7u8; 300][..]);
        k.delete().unwrap();
        assert!(k.load().unwrap().is_none());
        // Remove the test key itself.
        let root: Vec<u16> = "Software\\EternalMonitor-Test\0".encode_utf16().collect();
        let _ = unsafe {
            // SAFETY: a NUL-terminated string, alive for the call.
            windows::Win32::System::Registry::RegDeleteTreeW(
                windows::Win32::System::Registry::HKEY_CURRENT_USER,
                windows::core::PCWSTR(root.as_ptr()),
            )
        };
    }
}
