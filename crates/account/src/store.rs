//! Where the sign-in lives between runs: one entry in the OS key store (the
//! macOS Keychain, the Windows Credential Manager), never a file or the log.
//! Only the refresh token is kept (with the cell, the session id and what the
//! header shows); the access token stays in memory.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::api::AccountView;
use crate::cell::Cell;
use crate::secret::Secret;

/// The key store's service and account names.
pub const SERVICE: &str = "app.retv.bridge";
pub const ENTRY: &str = "retv-account";

/// One saved sign-in.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Saved {
    pub v: u8,
    pub cell: Cell,
    pub session_id: String,
    pub refresh: Secret,
    pub account: AccountView,
}

impl Saved {
    /// The entry's bytes, in a zeroing buffer.
    pub fn to_bytes(&self) -> Zeroizing<Vec<u8>> {
        let mut buf = Zeroizing::new(Vec::with_capacity(256));
        let _ = serde_json::to_writer(&mut *buf, self);
        buf
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Saved> {
        serde_json::from_slice::<Saved>(bytes)
            .ok()
            .filter(|s| s.v == 1)
    }
}

/// A place for one secret entry.
pub trait KeyStore: Send + Sync {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, String>;
    fn save(&self, bytes: &[u8]) -> Result<(), String>;
    fn delete(&self) -> Result<(), String>;
}

/// In memory only: tests, and systems without a key store Bridge knows
/// (Linux development builds): the sign-in lasts until Bridge quits.
#[derive(Default)]
pub struct MemoryStore(Mutex<Option<Zeroizing<Vec<u8>>>>);

impl MemoryStore {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Zeroizing<Vec<u8>>>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl KeyStore for MemoryStore {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
        Ok(self.lock().clone())
    }
    fn save(&self, bytes: &[u8]) -> Result<(), String> {
        *self.lock() = Some(Zeroizing::new(bytes.to_vec()));
        Ok(())
    }
    fn delete(&self) -> Result<(), String> {
        *self.lock() = None;
        Ok(())
    }
}

/// The OS key store, or memory where there is none.
pub fn os_store() -> Box<dyn KeyStore> {
    #[cfg(any(target_os = "macos", windows))]
    {
        Box::new(OsStore)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Box::new(MemoryStore::default())
    }
}

/// The macOS Keychain or the Windows Credential Manager.
#[cfg(any(target_os = "macos", windows))]
pub struct OsStore;

#[cfg(target_os = "macos")]
impl KeyStore for OsStore {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
        use security_framework::passwords::get_generic_password;
        match get_generic_password(SERVICE, ENTRY) {
            Ok(v) => Ok(Some(Zeroizing::new(v))),
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
            Err(e) => Err(format!("Keychain: {e}")),
        }
    }
    fn save(&self, bytes: &[u8]) -> Result<(), String> {
        security_framework::passwords::set_generic_password(SERVICE, ENTRY, bytes)
            .map_err(|e| format!("Keychain: {e}"))
    }
    fn delete(&self) -> Result<(), String> {
        match security_framework::passwords::delete_generic_password(SERVICE, ENTRY) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
            Err(e) => Err(format!("Keychain: {e}")),
        }
    }
}

#[cfg(target_os = "macos")]
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

#[cfg(windows)]
mod wincred {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_NOT_FOUND, FILETIME};
    use windows_sys::Win32::Security::Credentials::{
        CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
        CRED_TYPE_GENERIC,
    };
    use zeroize::Zeroizing;

    fn target() -> Vec<u16> {
        format!("{}/{}", super::SERVICE, super::ENTRY)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect()
    }

    pub fn load() -> Result<Option<Zeroizing<Vec<u8>>>, String> {
        let name = target();
        let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: `name` is NUL-terminated; on success `cred` points to a block freed below.
        let ok = unsafe { CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &mut cred) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            return if err == ERROR_NOT_FOUND {
                Ok(None)
            } else {
                Err(format!("Credential Manager: error {err}"))
            };
        }
        // SAFETY: CredReadW succeeded, so `cred` and its blob are valid until CredFree.
        let out = unsafe {
            let c = &*cred;
            let blob = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize);
            let copy = Zeroizing::new(blob.to_vec());
            // Wipe the system's copy before freeing it.
            std::ptr::write_bytes(c.CredentialBlob, 0, c.CredentialBlobSize as usize);
            copy
        };
        unsafe { CredFree(cred as *const _) };
        Ok(Some(out))
    }

    pub fn save(bytes: &[u8]) -> Result<(), String> {
        let mut name = target();
        let mut user: Vec<u16> = "ReTV Bridge"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut blob = Zeroizing::new(bytes.to_vec());
        let cred = CREDENTIALW {
            Flags: 0,
            Type: CRED_TYPE_GENERIC,
            TargetName: name.as_mut_ptr(),
            Comment: std::ptr::null_mut(),
            LastWritten: FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            },
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: std::ptr::null_mut(),
            TargetAlias: std::ptr::null_mut(),
            UserName: user.as_mut_ptr(),
        };
        // SAFETY: every pointer in `cred` lives until the call returns.
        let ok = unsafe { CredWriteW(&cred, 0) };
        if ok == 0 {
            return Err(format!("Credential Manager: error {}", unsafe {
                GetLastError()
            }));
        }
        Ok(())
    }

    pub fn delete() -> Result<(), String> {
        let name = target();
        // SAFETY: `name` is NUL-terminated.
        let ok = unsafe { CredDeleteW(name.as_ptr(), CRED_TYPE_GENERIC, 0) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            if err != ERROR_NOT_FOUND {
                return Err(format!("Credential Manager: error {err}"));
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
impl KeyStore for OsStore {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
        wincred::load()
    }
    fn save(&self, bytes: &[u8]) -> Result<(), String> {
        wincred::save(bytes)
    }
    fn delete(&self) -> Result<(), String> {
        wincred::delete()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_round_trips_and_refuses_other_versions() {
        let s = Saved {
            v: 1,
            cell: Cell::Ru,
            session_id: "s1".into(),
            refresh: Secret::new("rtr_x".into()),
            account: AccountView {
                label: Some("p***@mail.ru".into()),
                tier: "pro".into(),
                pro: true,
            },
        };
        let bytes = s.to_bytes();
        assert_eq!(Saved::from_bytes(&bytes), Some(s.clone()));
        let other = String::from_utf8(bytes.to_vec())
            .unwrap()
            .replace("\"v\":1", "\"v\":2");
        assert_eq!(Saved::from_bytes(other.as_bytes()), None);
        assert_eq!(Saved::from_bytes(b"junk"), None);
    }

    #[test]
    fn memory_store_keeps_one_entry() {
        let m = MemoryStore::default();
        assert!(m.load().unwrap().is_none());
        m.save(b"abc").unwrap();
        assert_eq!(m.load().unwrap().unwrap().as_slice(), b"abc");
        m.delete().unwrap();
        assert!(m.load().unwrap().is_none());
    }
}
