//! Windows DPAPI (`CryptProtectData`/`CryptUnprotectData`) via raw
//! `extern "system"` FFI, zero external crate dependencies (just `std`
//! linked against `Crypt32`/`Kernel32`) - the same style as
//! `secret_store.rs`'s Credential Manager FFI.
//!
//! This is the primitive a durable journal uses to store a value (e.g. a
//! secret read via a `secret(name)` builtin) that must never be written to
//! disk as plaintext, but must still be recoverable byte-for-byte on replay.
//! DPAPI with no explicit entropy ties the ciphertext to the current Windows
//! user account - the same trust boundary `secret_store.rs`'s Credential
//! Manager reads already use - so a journal file that leaks off this machine
//! (or off this user account) is not, by itself, enough to recover the
//! plaintext.

#[cfg(windows)]
use std::io;
#[cfg(windows)]
use std::ptr;

/// Encrypts `plaintext` for the current Windows user account via
/// `CryptProtectData`. The result can only be decrypted (via `unprotect`) by
/// the same user on the same machine.
#[cfg(windows)]
pub fn protect(plaintext: &[u8]) -> Result<Vec<u8>, String> {
    crypt_protect_data(plaintext)
}

/// Decrypts ciphertext produced by `protect`.
#[cfg(windows)]
pub fn unprotect(ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    crypt_unprotect_data(ciphertext)
}

#[cfg(not(windows))]
pub fn protect(_plaintext: &[u8]) -> Result<Vec<u8>, String> {
    Err("DPAPI encryption is only supported on Windows".into())
}

#[cfg(not(windows))]
pub fn unprotect(_ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    Err("DPAPI encryption is only supported on Windows".into())
}

#[cfg(windows)]
fn crypt_protect_data(plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let mut input_bytes = plaintext.to_vec();
    let input_blob = DataBlob { cb_data: input_bytes.len() as u32, pb_data: input_bytes.as_mut_ptr() };
    let mut output_blob = DataBlob { cb_data: 0, pb_data: ptr::null_mut() };

    let ok = unsafe {
        CryptProtectData(
            &input_blob,
            ptr::null(),
            ptr::null(),
            ptr::null_mut(),
            ptr::null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output_blob,
        )
    };
    if ok == 0 {
        return Err(format!("Failed to encrypt data with DPAPI: {}", io::Error::last_os_error()));
    }

    let result = unsafe {
        std::slice::from_raw_parts(output_blob.pb_data, output_blob.cb_data as usize).to_vec()
    };
    unsafe { LocalFree(output_blob.pb_data.cast()) };
    Ok(result)
}

#[cfg(windows)]
fn crypt_unprotect_data(ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    let mut input_bytes = ciphertext.to_vec();
    let input_blob = DataBlob { cb_data: input_bytes.len() as u32, pb_data: input_bytes.as_mut_ptr() };
    let mut output_blob = DataBlob { cb_data: 0, pb_data: ptr::null_mut() };

    let ok = unsafe {
        CryptUnprotectData(
            &input_blob,
            ptr::null_mut(),
            ptr::null(),
            ptr::null_mut(),
            ptr::null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output_blob,
        )
    };
    if ok == 0 {
        return Err(format!("Failed to decrypt data with DPAPI: {}", io::Error::last_os_error()));
    }

    let result = unsafe {
        std::slice::from_raw_parts(output_blob.pb_data, output_blob.cb_data as usize).to_vec()
    };
    unsafe { LocalFree(output_blob.pb_data.cast()) };
    Ok(result)
}

/// `dwFlags = CRYPTPROTECT_UI_FORBIDDEN` on both calls so DPAPI can never
/// block on an interactive UI prompt.
#[cfg(windows)]
const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;

#[cfg(windows)]
#[repr(C)]
struct DataBlob {
    cb_data: u32,
    pb_data: *mut u8,
}

#[cfg(windows)]
#[link(name = "Crypt32")]
extern "system" {
    fn CryptProtectData(
        data_in: *const DataBlob,
        data_descr: *const u16,
        optional_entropy: *const DataBlob,
        reserved: *mut std::ffi::c_void,
        prompt_struct: *mut std::ffi::c_void,
        dw_flags: u32,
        data_out: *mut DataBlob,
    ) -> i32;

    fn CryptUnprotectData(
        data_in: *const DataBlob,
        data_descr: *mut *mut u16,
        optional_entropy: *const DataBlob,
        reserved: *mut std::ffi::c_void,
        prompt_struct: *mut std::ffi::c_void,
        dw_flags: u32,
        data_out: *mut DataBlob,
    ) -> i32;
}

#[cfg(windows)]
#[link(name = "Kernel32")]
extern "system" {
    fn LocalFree(mem: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_arbitrary_bytes() {
        let plaintext = b"the quick brown fox jumps over the lazy dog";
        let ciphertext = protect(plaintext).unwrap();
        let decrypted = unprotect(&ciphertext).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn round_trip_empty_slice() {
        let plaintext: &[u8] = &[];
        let ciphertext = protect(plaintext).unwrap();
        let decrypted = unprotect(&ciphertext).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn round_trip_byte_boundary_values() {
        let plaintext: Vec<u8> = (0u8..=255).chain(0u8..=255).collect();
        let ciphertext = protect(&plaintext).unwrap();
        let decrypted = unprotect(&ciphertext).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn ciphertext_never_contains_plaintext_as_substring() {
        let plaintext = b"correct horse battery staple, do not leak me";
        let ciphertext = protect(plaintext).unwrap();
        assert!(
            !contains_subslice(&ciphertext, plaintext),
            "ciphertext must not contain the original plaintext as a substring"
        );
    }

    fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
        if needle.is_empty() {
            return true;
        }
        if needle.len() > haystack.len() {
            return false;
        }
        haystack.windows(needle.len()).any(|window| window == needle)
    }
}
