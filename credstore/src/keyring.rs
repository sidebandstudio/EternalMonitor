//! Where the keyring blob lives: one item in OS-protected storage, replaced
//! atomically.
//!
//! - macOS: a generic password in the data-protection Keychain, accessible
//!   after first unlock on this device only, never synchronized.
//! - Windows: one DPAPI-protected (current user) binary value under `HKCU`,
//!   flushed before `store` returns.
//!
//! A store that returns an error may still have replaced the blob, and on
//! macOS a stored blob may not be durable yet: the store's recovery never
//! relies on either (see `store.rs`). `MemKeyring` (tests and fuzzing)
//! models both.

use std::io;

use eternal_wire::v3::Zeroizing;

/// One atomically replaced secret blob.
pub trait Keyring {
    /// The blob, or `None` if there isn't one.
    fn load(&self) -> io::Result<Option<Zeroizing<Vec<u8>>>>;
    /// Replaces the blob. Either the new blob is stored, or the old one
    /// still is; an error doesn't say which.
    fn store(&mut self, blob: &[u8]) -> io::Result<()>;
    /// Makes the visible blob durable where the platform can, and says when
    /// a visible blob is known to be durable.
    fn flush(&mut self) -> io::Result<Durability>;
}

/// When a keyring blob is known to be durable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Durability {
    /// Now: [`Keyring::flush`] and every successful store flushed it
    /// (Windows).
    Durable,
    /// Once a later boot sees it (the macOS Keychain has no flush). Holds
    /// this boot's id.
    LaterBoot(Vec<u8>),
}

/// An in-memory [`Keyring`] for tests and fuzzing that separates what a
/// reader sees from what survives a crash.
///
/// Every stored blob is visible at once. Unless the keyring flushes, a crash
/// may keep any blob stored since the last durable point, or the one before
/// them all: [`MemKeyring::after_crash`] picks one.
#[cfg(any(test, feature = "test-hooks"))]
#[derive(Clone, Default)]
pub struct MemKeyring {
    visible: Option<Vec<u8>>,
    /// What a crash may leave, oldest first: the last durable blob, then
    /// every blob stored since.
    durable: Vec<Option<Vec<u8>>>,
    /// A successful store is durable at once (Windows); otherwise a stored
    /// blob may be lost (the macOS Keychain has no flush).
    flushes: bool,
    /// The boot this keyring is in; a crash is a power loss and a new boot.
    boot: u64,
    /// This boot's id can't be read.
    boot_unreadable: bool,
    calls: usize,
    crash_at: Option<usize>,
}

#[cfg(any(test, feature = "test-hooks"))]
impl MemKeyring {
    /// A keyring whose stores may be lost on a crash.
    pub fn new() -> Self {
        Self::default()
    }

    /// A keyring whose successful stores are durable.
    pub fn flushing() -> Self {
        Self {
            flushes: true,
            ..Self::default()
        }
    }

    /// Fails store number `n` (from 0) and every store after it.
    pub fn crash_at(&mut self, n: usize) {
        self.crash_at = Some(n);
    }

    pub fn calls(&self) -> usize {
        self.calls
    }

    /// How many different blobs a crash now may leave.
    pub fn crash_choices(&self) -> usize {
        self.durable.len().max(1)
    }

    /// The keyring after a power loss and the next boot, keeping choice `n`
    /// (from 0, oldest first; clamped) of what may survive.
    pub fn after_crash(&self, n: usize) -> Self {
        let kept = match self.durable.len() {
            0 => self.visible.clone(),
            len => self.durable[n.min(len - 1)].clone(),
        };
        Self {
            visible: kept.clone(),
            durable: vec![kept],
            flushes: self.flushes,
            boot: self.boot + 1,
            boot_unreadable: self.boot_unreadable,
            calls: 0,
            crash_at: None,
        }
    }

    /// The keyring after the process restarts in the same boot: nothing is
    /// lost, and nothing more is durable.
    pub fn restart(&self) -> Self {
        Self {
            calls: 0,
            crash_at: None,
            ..self.clone()
        }
    }

    pub fn boot(&self) -> u64 {
        self.boot
    }

    /// Makes this boot's id unreadable (or readable again).
    pub fn set_boot_unreadable(&mut self, unreadable: bool) {
        self.boot_unreadable = unreadable;
    }

    /// Makes a stored blob visible, and durable only if `durable`.
    pub fn store_visible(&mut self, blob: &[u8], durable: bool) {
        if self.durable.is_empty() {
            self.durable.push(self.visible.clone());
        }
        self.visible = Some(blob.to_vec());
        if durable {
            self.durable = vec![self.visible.clone()];
        } else {
            self.durable.push(self.visible.clone());
        }
    }

    /// Replaces or removes the blob directly and durably, for restore tests.
    pub fn put(&mut self, blob: Option<Vec<u8>>) {
        self.visible = blob;
        self.durable = vec![self.visible.clone()];
    }

    pub fn get(&self) -> Option<&Vec<u8>> {
        self.visible.as_ref()
    }

    pub fn flushes(&self) -> bool {
        self.flushes
    }
}

#[cfg(any(test, feature = "test-hooks"))]
impl Keyring for MemKeyring {
    fn load(&self) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
        Ok(self.visible.clone().map(Zeroizing::new))
    }

    fn store(&mut self, blob: &[u8]) -> io::Result<()> {
        let n = self.calls;
        self.calls += 1;
        if self.crash_at.is_some_and(|at| n >= at) {
            return Err(io::Error::other("simulated crash"));
        }
        self.store_visible(blob, self.flushes);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<Durability> {
        if self.flushes {
            self.durable = vec![self.visible.clone()];
            Ok(Durability::Durable)
        } else {
            if self.boot_unreadable {
                return Err(io::Error::other("boot id unreadable"));
            }
            Ok(Durability::LaterBoot(self.boot.to_le_bytes().to_vec()))
        }
    }
}

/// The macOS data-protection Keychain.
#[cfg(target_os = "macos")]
pub struct MacKeychain {
    service: String,
    account: String,
}

#[cfg(target_os = "macos")]
mod mac {
    use core_foundation::base::{kCFAllocatorNull, CFType, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::data::CFData;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::{CFString, CFStringRef};
    use core_foundation_sys::data::CFDataCreateWithBytesNoCopy;
    use security_framework_sys::base::{errSecDuplicateItem, errSecItemNotFound, errSecSuccess};
    use security_framework_sys::item::{
        kSecAttrAccount, kSecAttrService, kSecAttrSynchronizable, kSecClass,
        kSecClassGenericPassword, kSecReturnData, kSecUseDataProtectionKeychain, kSecValueData,
    };
    use security_framework_sys::keychain_item::{
        SecItemAdd, SecItemCopyMatching, SecItemDelete, SecItemUpdate,
    };
    use std::io;

    use super::Zeroizing;

    #[link(name = "Security", kind = "framework")]
    extern "C" {
        static kSecAttrAccessible: CFStringRef;
        static kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly: CFStringRef;
    }

    fn key(k: CFStringRef) -> CFString {
        // SAFETY: a Security framework constant, valid for the process.
        unsafe { CFString::wrap_under_get_rule(k) }
    }

    fn error(status: i32) -> io::Error {
        io::Error::other(format!("Keychain status {status}"))
    }

    /// The item: generic password, service and account, in the
    /// data-protection Keychain, never synchronized.
    fn query(service: &str, account: &str) -> Vec<(CFString, CFType)> {
        // SAFETY (each `key`): Security framework constants.
        unsafe {
            vec![
                (key(kSecClass), key(kSecClassGenericPassword).as_CFType()),
                (key(kSecAttrService), CFString::new(service).as_CFType()),
                (key(kSecAttrAccount), CFString::new(account).as_CFType()),
                (
                    key(kSecUseDataProtectionKeychain),
                    CFBoolean::true_value().as_CFType(),
                ),
                (
                    key(kSecAttrSynchronizable),
                    CFBoolean::false_value().as_CFType(),
                ),
            ]
        }
    }

    pub fn load(service: &str, account: &str) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
        let mut q = query(service, account);
        // SAFETY: a Security framework constant.
        q.push((
            unsafe { key(kSecReturnData) },
            CFBoolean::true_value().as_CFType(),
        ));
        let q = CFDictionary::from_CFType_pairs(&q);
        let mut out = std::ptr::null();
        // SAFETY: `q` is a valid dictionary; `out` receives a +1 reference.
        let status = unsafe { SecItemCopyMatching(q.as_concrete_TypeRef(), &mut out) };
        if status == errSecItemNotFound {
            return Ok(None);
        }
        if status != errSecSuccess {
            return Err(error(status));
        }
        // SAFETY: with kSecReturnData and one match, the result is a CFData
        // we own.
        let data = unsafe { CFData::wrap_under_create_rule(out as _) };
        Ok(Some(Zeroizing::new(data.bytes().to_vec())))
    }

    /// The blob as a CFData that points at it rather than copying it, so no
    /// unwiped copy of the keys is left behind.
    fn borrowed_data(blob: &[u8]) -> CFData {
        // SAFETY: with kCFAllocatorNull the CFData only borrows `blob`; every
        // use is within this module's calls, which `blob` outlives.
        unsafe {
            CFData::wrap_under_create_rule(CFDataCreateWithBytesNoCopy(
                std::ptr::null(),
                blob.as_ptr(),
                blob.len() as _,
                kCFAllocatorNull,
            ))
        }
    }

    /// Adds the item, or replaces only its data and protection class if it
    /// exists.
    pub fn store(service: &str, account: &str, blob: &[u8]) -> io::Result<()> {
        let value = borrowed_data(blob);
        let mut add = query(service, account);
        // SAFETY: Security framework constants.
        unsafe {
            add.push((
                key(kSecAttrAccessible),
                key(kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly).as_CFType(),
            ));
            add.push((key(kSecValueData), value.as_CFType()));
        }
        let add = CFDictionary::from_CFType_pairs(&add);
        // SAFETY: `add` is a valid dictionary; no result is requested.
        let status = unsafe { SecItemAdd(add.as_concrete_TypeRef(), std::ptr::null_mut()) };
        if status == errSecSuccess {
            return Ok(());
        }
        if status != errSecDuplicateItem {
            return Err(error(status));
        }
        let search = CFDictionary::from_CFType_pairs(&query(service, account));
        // SAFETY: Security framework constants.
        let update = unsafe {
            CFDictionary::from_CFType_pairs(&[
                (key(kSecValueData), value.as_CFType()),
                (
                    key(kSecAttrAccessible),
                    key(kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly).as_CFType(),
                ),
            ])
        };
        // SAFETY: both dictionaries are valid.
        let status =
            unsafe { SecItemUpdate(search.as_concrete_TypeRef(), update.as_concrete_TypeRef()) };
        if status == errSecSuccess {
            Ok(())
        } else {
            Err(error(status))
        }
    }

    /// Whether the item's protection class is after first unlock, this
    /// device only, for the signed round trip.
    #[cfg(test)]
    pub fn after_first_unlock_this_device(service: &str, account: &str) -> io::Result<bool> {
        use core_foundation::base::CFTypeRef;
        use security_framework_sys::item::kSecReturnAttributes;
        let mut q = query(service, account);
        // SAFETY: a Security framework constant.
        q.push((
            unsafe { key(kSecReturnAttributes) },
            CFBoolean::true_value().as_CFType(),
        ));
        let q = CFDictionary::from_CFType_pairs(&q);
        let mut out: CFTypeRef = std::ptr::null();
        // SAFETY: `q` is valid; `out` receives a +1 dictionary.
        let status = unsafe { SecItemCopyMatching(q.as_concrete_TypeRef(), &mut out) };
        if status != errSecSuccess {
            return Err(error(status));
        }
        // SAFETY: with kSecReturnAttributes and one match, a dictionary we
        // own.
        let attrs: CFDictionary<CFString, CFType> =
            unsafe { CFDictionary::wrap_under_create_rule(out as _) };
        // SAFETY: Security framework constants.
        let (class, want) = unsafe {
            (
                key(kSecAttrAccessible),
                key(kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly),
            )
        };
        Ok(attrs
            .find(&class)
            .and_then(|v| v.downcast::<CFString>())
            .is_some_and(|v| v == want))
    }

    /// This boot's id, `kern.bootsessionuuid`.
    pub fn boot_id() -> io::Result<Vec<u8>> {
        let mut buf = [0u8; 64];
        let mut len = buf.len();
        // SAFETY: `buf` has room for `len` bytes; no new value is set.
        let rc = unsafe {
            libc::sysctlbyname(
                c"kern.bootsessionuuid".as_ptr(),
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || len == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(buf[..len].to_vec())
    }

    pub fn delete(service: &str, account: &str) -> io::Result<()> {
        let q = CFDictionary::from_CFType_pairs(&query(service, account));
        // SAFETY: a valid dictionary.
        let status = unsafe { SecItemDelete(q.as_concrete_TypeRef()) };
        if status == errSecSuccess || status == errSecItemNotFound {
            Ok(())
        } else {
            Err(error(status))
        }
    }
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

    /// Removes the item, for tests.
    pub fn delete(&self) -> io::Result<()> {
        mac::delete(&self.service, &self.account)
    }
}

#[cfg(target_os = "macos")]
impl Keyring for MacKeychain {
    fn load(&self) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
        mac::load(&self.service, &self.account)
    }

    fn store(&mut self, blob: &[u8]) -> io::Result<()> {
        mac::store(&self.service, &self.account, blob)
    }

    fn flush(&mut self) -> io::Result<Durability> {
        Ok(Durability::LaterBoot(mac::boot_id()?))
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
        use windows::Win32::System::Registry::RegFlushKey;
        // SAFETY: `store_with` passes a key it holds open.
        self.store_with(blob, |key| unsafe { RegFlushKey(key) })
    }

    fn flush(&mut self) -> io::Result<Durability> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
        use windows::Win32::System::Registry::{
            RegCloseKey, RegFlushKey, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE,
            KEY_SET_VALUE,
        };
        let mut key = HKEY::default();
        // SAFETY: the subkey is NUL-terminated; `key` receives the handle.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(self.subkey.as_ptr()),
                0,
                KEY_SET_VALUE | KEY_QUERY_VALUE,
                &mut key,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            // Nothing stored: nothing to flush.
            return Ok(Durability::Durable);
        }
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status.0 as i32));
        }
        // SAFETY: `key` is open, and closed right after.
        let status = unsafe {
            let status = RegFlushKey(key);
            let _ = RegCloseKey(key);
            status
        };
        if status == ERROR_SUCCESS {
            Ok(Durability::Durable)
        } else {
            Err(io::Error::from_raw_os_error(status.0 as i32))
        }
    }
}

#[cfg(windows)]
impl DpapiRegistry {
    /// Seals `blob`, writes it, and runs `flush` on the open key. An error
    /// is returned unless every step succeeded, even if the value was
    /// written.
    fn store_with(
        &mut self,
        blob: &[u8],
        flush: impl FnOnce(
            windows::Win32::System::Registry::HKEY,
        ) -> windows::Win32::Foundation::WIN32_ERROR,
    ) -> io::Result<()> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HLOCAL};
        use windows::Win32::Security::Cryptography::{
            CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        };
        use windows::Win32::System::Registry::{
            RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE,
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
                // RegFlushKey is documented to need KEY_QUERY_VALUE too.
                KEY_SET_VALUE | KEY_QUERY_VALUE,
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
                    status = flush(key);
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
    fn mem_keyring_separates_visible_from_durable() {
        let mut k = MemKeyring::new();
        assert!(k.load().unwrap().is_none());
        k.store(b"one").unwrap();
        k.store(b"two").unwrap();
        assert_eq!(&**k.load().unwrap().unwrap(), b"two");
        // A crash may keep nothing, "one" or "two".
        assert_eq!(k.crash_choices(), 3);
        assert!(k.after_crash(0).get().is_none());
        assert_eq!(k.after_crash(1).get().unwrap(), b"one");
        assert_eq!(k.after_crash(2).get().unwrap(), b"two");
        // A failed store never happened.
        k.crash_at(2);
        assert!(k.store(b"three").is_err());
        assert_eq!(k.crash_choices(), 3);
        // A flushing keyring keeps only the last successful store.
        let mut f = MemKeyring::flushing();
        f.store(b"one").unwrap();
        f.store(b"two").unwrap();
        assert_eq!(f.crash_choices(), 1);
        assert_eq!(f.after_crash(0).get().unwrap(), b"two");
        // Unless it was only made visible.
        f.store_visible(b"three", false);
        assert_eq!(f.crash_choices(), 2);
        assert_eq!(f.after_crash(0).get().unwrap(), b"two");
    }

    #[test]
    fn mem_keyring_durability_follows_its_kind() {
        let mut f = MemKeyring::flushing();
        f.store_visible(b"one", false);
        assert_eq!(f.flush().unwrap(), Durability::Durable);
        assert_eq!(f.crash_choices(), 1, "the flush made it durable");
        let mut m = MemKeyring::new();
        let boot = m.flush().unwrap();
        assert_eq!(boot, Durability::LaterBoot(0u64.to_le_bytes().to_vec()));
        assert_eq!(m.restart().flush().unwrap(), boot, "same boot");
        assert_ne!(m.after_crash(0).flush().unwrap(), boot, "the next boot");
    }

    /// The boot id the store records in each keyring blob: present, and the
    /// same for the whole boot.
    #[cfg(target_os = "macos")]
    #[test]
    fn mac_boot_id_is_stable_within_a_boot() {
        let mut k = MacKeychain::new("EternalMonitor test", "boot-id");
        let Durability::LaterBoot(a) = k.flush().unwrap() else {
            panic!("the Keychain has no flush");
        };
        let Durability::LaterBoot(b) = k.flush().unwrap() else {
            panic!("the Keychain has no flush");
        };
        assert!(!a.is_empty());
        assert_eq!(a, b);
    }

    // The data-protection Keychain needs a binary signed with a keychain
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
        // Add, then update twice: the update path replaces the data and
        // keeps the protection class.
        k.store(b"first").unwrap();
        assert!(mac::after_first_unlock_this_device(&k.service, &k.account).unwrap());
        k.store(b"second").unwrap();
        k.store(&[7u8; 300]).unwrap();
        assert_eq!(&**k.load().unwrap().unwrap(), &[7u8; 300][..]);
        assert!(mac::after_first_unlock_this_device(&k.service, &k.account).unwrap());
        k.delete().unwrap();
        assert!(k.load().unwrap().is_none());
    }

    #[cfg(windows)]
    /// Removes `HKCU\<root>` and everything under it.
    fn remove_test_key(root: &str) {
        let root: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
        let _ = unsafe {
            // SAFETY: a NUL-terminated string, alive for the call.
            windows::Win32::System::Registry::RegDeleteTreeW(
                windows::Win32::System::Registry::HKEY_CURRENT_USER,
                windows::core::PCWSTR(root.as_ptr()),
            )
        };
    }

    /// Removes its test key when dropped, also when a test panics.
    #[cfg(windows)]
    struct TestKey(&'static str);

    #[cfg(windows)]
    impl Drop for TestKey {
        fn drop(&mut self) {
            remove_test_key(self.0);
        }
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_registry_round_trip() {
        let _cleanup = TestKey(r"Software\EternalMonitor-Test-RoundTrip");
        let mut k = DpapiRegistry::new(
            r"Software\EternalMonitor-Test-RoundTrip\credstore",
            &format!("keyring-{}", std::process::id()),
        );
        k.delete().unwrap();
        assert!(k.load().unwrap().is_none());
        k.store(b"first").unwrap();
        k.store(&[7u8; 300]).unwrap();
        assert_eq!(&**k.load().unwrap().unwrap(), &[7u8; 300][..]);
        k.delete().unwrap();
        assert!(k.load().unwrap().is_none());
    }

    /// A failed flush is reported, even though the value was written.
    #[cfg(windows)]
    #[test]
    fn dpapi_registry_reports_a_failed_flush() {
        use windows::Win32::Foundation::ERROR_ACCESS_DENIED;
        let _cleanup = TestKey(r"Software\EternalMonitor-Test-Flush");
        let mut k = DpapiRegistry::new(
            r"Software\EternalMonitor-Test-Flush\credstore",
            &format!("keyring-{}", std::process::id()),
        );
        k.delete().unwrap();
        // Nothing stored yet: nothing to flush.
        assert_eq!(k.flush().unwrap(), Durability::Durable);
        assert!(k.store_with(b"written", |_| ERROR_ACCESS_DENIED).is_err());
        assert_eq!(&**k.load().unwrap().unwrap(), b"written");
        // The real flush succeeds, on its own and as part of a store.
        assert_eq!(k.flush().unwrap(), Durability::Durable);
        k.store(b"flushed").unwrap();
        assert_eq!(&**k.load().unwrap().unwrap(), b"flushed");
        k.delete().unwrap();
    }
}
