//! Windows keyring via the Credential Manager. Port of `src/keyring/windows.ts`.
//!
//! Uses `CredReadW`/`CredWriteW`/`CredDeleteW` through `windows-sys` rather
//! than shelling out to `cmdkey`, which cannot return a stored secret.
//!
//! NOTE: this module is only compiled on Windows and is therefore exercised by
//! the Windows CI job, not by the Linux/macOS test runs.

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{GetLastError, ERROR_NOT_FOUND, FILETIME};
use windows_sys::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
    CRED_TYPE_GENERIC,
};

use super::{KeyringError, SERVICE};

/// Credential Manager entry name, matching the `service:account` convention
/// used by keytar (and therefore by the TypeScript CLI).
fn target_name(account: &str) -> String {
    format!("{SERVICE}:{account}")
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Encode the credential blob as UTF-16LE bytes, matching upstream
/// `encodeWideString()`. The Credential Manager stores the blob verbatim, so a
/// key written by the TypeScript CLI (and by keytar before it) only round-trips
/// if we use the same encoding rather than UTF-8.
fn wide_bytes(value: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len() * 2);
    for unit in value.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

/// Decode a UTF-16LE credential blob. Mirrors `decodeWideString()`, which reads
/// two bytes per code unit; an odd trailing byte is dropped.
fn decode_wide_bytes(blob: &[u8]) -> String {
    let units: Vec<u16> = blob
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// The Credential Manager is part of the OS; no probe needed.
pub fn is_available() -> bool {
    true
}

pub fn get(account: &str) -> Result<Option<String>, KeyringError> {
    let target = wide(&target_name(account));
    let mut credential: *mut CREDENTIALW = std::ptr::null_mut();

    let ok = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) };
    if ok == 0 {
        let code = unsafe { GetLastError() };
        if code == ERROR_NOT_FOUND {
            return Ok(None);
        }
        return Err(KeyringError::new(format!(
            "CredReadW failed for \"{account}\" (error {code})"
        )));
    }

    let value = unsafe {
        let entry = &*credential;
        let size = entry.CredentialBlobSize as usize;
        if size == 0 || entry.CredentialBlob.is_null() {
            String::new()
        } else {
            let blob = std::slice::from_raw_parts(entry.CredentialBlob, size);
            decode_wide_bytes(blob)
        }
    };
    unsafe { CredFree(credential as *const c_void) };

    Ok(if value.is_empty() { None } else { Some(value) })
}

pub fn set(account: &str, password: &str) -> Result<(), KeyringError> {
    let mut target = wide(&target_name(account));
    let mut username = wide(account);
    let mut blob = wide_bytes(password);

    let credential = CREDENTIALW {
        Flags: 0,
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_mut_ptr(),
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
        UserName: username.as_mut_ptr(),
    };

    let ok = unsafe { CredWriteW(&credential, 0) };
    if ok == 0 {
        let code = unsafe { GetLastError() };
        return Err(KeyringError::new(format!(
            "CredWriteW failed for \"{account}\" (error {code})"
        )));
    }
    Ok(())
}

pub fn delete(account: &str) -> Result<(), KeyringError> {
    let target = wide(&target_name(account));
    let ok = unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
    if ok == 0 {
        let code = unsafe { GetLastError() };
        if code == ERROR_NOT_FOUND {
            return Ok(());
        }
        return Err(KeyringError::new(format!(
            "CredDeleteW failed for \"{account}\" (error {code})"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_round_trips_through_utf16le() {
        let key = "lin_api_0123456789abcdef";
        let blob = wide_bytes(key);
        // Two bytes per code unit, little-endian — the layout keytar and the
        // TypeScript CLI write.
        assert_eq!(blob.len(), key.len() * 2);
        assert_eq!(blob[0], b'l');
        assert_eq!(blob[1], 0);
        assert_eq!(decode_wide_bytes(&blob), key);
    }

    #[test]
    fn blob_handles_non_ascii_and_empty() {
        assert_eq!(decode_wide_bytes(&wide_bytes("héllo→")), "héllo→");
        assert_eq!(wide_bytes(""), Vec::<u8>::new());
        assert_eq!(decode_wide_bytes(&[]), "");
    }
}
