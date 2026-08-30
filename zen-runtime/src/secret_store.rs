//! The real, shared secret store: Windows Credential Manager accessed via
//! raw `CredReadW`/`CredWriteW`/`CredDeleteW`/`CredEnumerateW` FFI, zero
//! external crate dependencies (just `std` + `extern "system"` linked
//! against `Advapi32`/`Kernel32`).
//!
//! This used to live entirely inside `zen-lang`'s
//! `runtime::plugins::secrets` module, reachable only through zen-lang's own
//! `Executor`. It's moved here so `flux-lang` - which depends on
//! `zen-runtime` but not `zen-lang` - can read the same secrets through its
//! own `secret`/`env` builtins with zero migration and zero compatibility
//! risk: the target-naming scheme below (`APP_NAMESPACE`/`DEFAULT_PROFILE`/
//! per-name target format/legacy fallback) is unchanged byte-for-byte from
//! the original zen-lang module, so a secret set via `zen secrets.set` is
//! immediately readable from a `.flux` script via `secret(name)`, and vice
//! versa.
//!
//! What did **not** move: `SecretsPlugin` (the `ZenPlugin`/`PluginHost`/
//! `.fg`-command wiring for `secrets.set`/`secrets.get`/etc.), the
//! terminal-echo-disabling interactive prompt UX for `secrets.set`
//! (zen-lang-only - Flux has no interactive secret-setting), and
//! `resolve_env_config` (zen-lang's `.fg`-specific `CallConfig`/`Expr`
//! handling for `{ secret: "name" }` env references - `CallConfig`/`Expr`
//! are zen-lang AST types Flux doesn't share). Those all still live in
//! `zen-lang/src/runtime/plugins/secrets.rs` and now call the functions
//! below instead of a local implementation.

use std::ffi::c_void;
use std::io;
use std::ptr;

const APP_NAMESPACE: &str = "zen";
const DEFAULT_PROFILE: &str = "default";
const LEGACY_TARGET_PREFIX: &str = "zen/";

/// Interface for looking up a secret by name.
///
/// The workflow engine's `{ secret: "name" }` env references used to call
/// `crate::runtime::plugins::secrets::read_secret` directly, which pulled
/// the whole secrets plugin module (and its `ZenPlugin`/`Executor` coupling)
/// into the workflow engine's dependency surface. Going through this trait
/// instead means workflow code depends on "something that can look up a
/// secret by name", not the concrete secrets module.
pub trait SecretStore {
    fn read_secret(&self, name: &str) -> Result<Option<String>, String>;
}

/// The concrete, real implementation of `SecretStore` - Windows Credential
/// Manager via the free functions below. Stateless, mirroring
/// `effects.rs`'s `ProcessEffects`/`FsEffects` shape: no fields, just a unit
/// struct that dispatches to the module-level functions.
pub struct CredentialManagerSecretStore;

impl SecretStore for CredentialManagerSecretStore {
    fn read_secret(&self, name: &str) -> Result<Option<String>, String> {
        read_secret(name)
    }
}

/// Reads a secret by name, checking the namespaced target first and falling
/// back to the legacy (pre-namespace) target. Returns `Ok(None)` - not an
/// error - when the secret simply isn't present.
pub fn read_secret(name: &str) -> Result<Option<String>, String> {
    validate_name(name)?;
    if let Some(secret) = credential_read_target(&target_name(name))? {
        return Ok(Some(secret));
    }
    credential_read_target(&legacy_target_name(name))
}

/// Writes a secret under its namespaced target (`zen/default/<name>`,
/// dots replaced with `/`). Always writes the namespaced form, never the
/// legacy one - the legacy target is a read-only fallback for
/// pre-namespace secrets that were never migrated.
pub fn write_secret(name: &str, secret: &str) -> Result<(), String> {
    validate_name(name)?;
    credential_write_target(&target_name(name), secret)
}

/// Deletes a secret by name, trying both the namespaced and legacy targets.
/// Returns whether anything was actually deleted. A real, always-available
/// public function (unlike the zen-lang original, which kept this
/// `#[cfg(test)]`-only) - a shared store should expose full CRUD even
/// though Flux only needs read initially.
pub fn delete_secret(name: &str) -> Result<bool, String> {
    validate_name(name)?;
    let deleted =
        credential_delete_target(&target_name(name))? || credential_delete_target(&legacy_target_name(name))?;
    Ok(deleted)
}

/// Lists every secret name under this app's namespace, without revealing
/// values. A real, always-available public function (the zen-lang original
/// kept `credential_list_targets` private, wrapping it locally in the
/// `secrets.list` command).
pub fn list_secrets() -> Result<Vec<String>, String> {
    credential_list_targets()
}

/// Public because zen-lang's `secrets.save` (bulk-save) needs to
/// fail-fast-validate every name in a batch *before* writing any of them -
/// relying solely on `write_secret`'s own internal validation would let an
/// early name in the batch get written before a later invalid name is
/// discovered.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("secret name cannot be empty".into());
    }
    if name.contains('/') || name.contains('\\') {
        return Err("secret name cannot contain path separators".into());
    }
    if name.split('.').any(str::is_empty) {
        return Err("secret name segments cannot be empty".into());
    }
    Ok(())
}

fn target_name(name: &str) -> String {
    let path = name.replace('.', "/");
    format!("{}/{}/{}", APP_NAMESPACE, DEFAULT_PROFILE, path)
}

fn legacy_target_name(name: &str) -> String {
    format!("{}{}", LEGACY_TARGET_PREFIX, name)
}

fn friendly_name(target: &str) -> Option<String> {
    let structured_prefix = format!("{}/{}/", APP_NAMESPACE, DEFAULT_PROFILE);
    if let Some(name) = target.strip_prefix(&structured_prefix) {
        return Some(name.replace('/', "."));
    }

    target.strip_prefix(LEGACY_TARGET_PREFIX).map(str::to_string)
}

#[cfg(windows)]
fn credential_write_target(target: &str, secret: &str) -> Result<(), String> {
    let mut target_w = wide_null(target);
    let mut user_w = wide_null("zen");
    let mut blob = secret.as_bytes().to_vec();
    let credential = CredentialW {
        flags: 0,
        type_: CRED_TYPE_GENERIC,
        target_name: target_w.as_mut_ptr(),
        comment: ptr::null_mut(),
        last_written: FileTime { dw_low_date_time: 0, dw_high_date_time: 0 },
        credential_blob_size: blob.len() as u32,
        credential_blob: blob.as_mut_ptr(),
        persist: CRED_PERSIST_LOCAL_MACHINE,
        attribute_count: 0,
        attributes: ptr::null_mut(),
        target_alias: ptr::null_mut(),
        user_name: user_w.as_mut_ptr(),
    };

    let ok = unsafe { CredWriteW(&credential, 0) };
    if ok == 0 {
        Err(format!("Failed to save secret '{}': {}", target, io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn credential_write_target(_target: &str, _secret: &str) -> Result<(), String> {
    Err("secrets are only supported on Windows Credential Manager".into())
}

#[cfg(windows)]
fn credential_read_target(target: &str) -> Result<Option<String>, String> {
    let target_w = wide_null(target);
    let mut credential: *mut CredentialW = ptr::null_mut();
    let ok = unsafe { CredReadW(target_w.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) };
    if ok == 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_NOT_FOUND) {
            return Ok(None);
        }
        return Err(format!("Failed to read secret '{}': {}", target, err));
    }

    let result = unsafe {
        let cred = &*credential;
        let bytes = std::slice::from_raw_parts(cred.credential_blob, cred.credential_blob_size as usize);
        String::from_utf8(bytes.to_vec()).map(Some).map_err(|_| format!("Secret '{}' is not valid UTF-8", target))
    };
    unsafe { CredFree(credential.cast()) };
    result
}

#[cfg(not(windows))]
fn credential_read_target(_target: &str) -> Result<Option<String>, String> {
    Err("secrets are only supported on Windows Credential Manager".into())
}

#[cfg(windows)]
fn credential_delete_target(target: &str) -> Result<bool, String> {
    let target_w = wide_null(target);
    let ok = unsafe { CredDeleteW(target_w.as_ptr(), CRED_TYPE_GENERIC, 0) };
    if ok == 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_NOT_FOUND) {
            return Ok(false);
        }
        Err(format!("Failed to delete secret '{}': {}", target, err))
    } else {
        Ok(true)
    }
}

#[cfg(not(windows))]
fn credential_delete_target(_target: &str) -> Result<bool, String> {
    Err("secrets are only supported on Windows Credential Manager".into())
}

#[cfg(windows)]
fn credential_list_targets() -> Result<Vec<String>, String> {
    let filter = wide_null(&format!("{}/*", APP_NAMESPACE));
    let mut count = 0u32;
    let mut credentials: *mut *mut CredentialW = ptr::null_mut();
    let ok = unsafe { CredEnumerateW(filter.as_ptr(), 0, &mut count, &mut credentials) };
    if ok == 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_NOT_FOUND) {
            return Ok(Vec::new());
        }
        return Err(format!("Failed to list secrets: {}", err));
    }

    let mut names = Vec::new();
    unsafe {
        let slice = std::slice::from_raw_parts(credentials, count as usize);
        for credential in slice {
            let target = read_wide_null((**credential).target_name);
            if let Some(name) = friendly_name(&target) {
                names.push(name);
            }
        }
        CredFree(credentials.cast());
    }
    names.sort();
    names.dedup();
    Ok(names)
}

#[cfg(not(windows))]
fn credential_list_targets() -> Result<Vec<String>, String> {
    Err("secrets are only supported on Windows Credential Manager".into())
}

#[cfg(windows)]
fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
unsafe fn read_wide_null(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
}

#[cfg(windows)]
const CRED_TYPE_GENERIC: u32 = 1;
#[cfg(windows)]
const CRED_PERSIST_LOCAL_MACHINE: u32 = 2;
#[cfg(windows)]
const ERROR_NOT_FOUND: i32 = 1168;

#[cfg(windows)]
#[repr(C)]
struct FileTime {
    dw_low_date_time: u32,
    dw_high_date_time: u32,
}

#[cfg(windows)]
#[repr(C)]
struct CredentialAttributeW {
    keyword: *mut u16,
    flags: u32,
    value_size: u32,
    value: *mut u8,
}

#[cfg(windows)]
#[repr(C)]
struct CredentialW {
    flags: u32,
    type_: u32,
    target_name: *mut u16,
    comment: *mut u16,
    last_written: FileTime,
    credential_blob_size: u32,
    credential_blob: *mut u8,
    persist: u32,
    attribute_count: u32,
    attributes: *mut CredentialAttributeW,
    target_alias: *mut u16,
    user_name: *mut u16,
}

#[cfg(windows)]
#[link(name = "Advapi32")]
extern "system" {
    fn CredWriteW(credential: *const CredentialW, flags: u32) -> i32;
    fn CredReadW(target_name: *const u16, type_: u32, flags: u32, credential: *mut *mut CredentialW) -> i32;
    fn CredDeleteW(target_name: *const u16, type_: u32, flags: u32) -> i32;
    fn CredEnumerateW(
        filter: *const u16,
        flags: u32,
        count: *mut u32,
        credentials: *mut *mut *mut CredentialW,
    ) -> i32;
    fn CredFree(buffer: *mut c_void);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_write_read_delete() {
        let name = format!("zen.test.runtime_round_trip.{}", std::process::id());
        write_secret(&name, "round-trip-value").unwrap();

        let read = read_secret(&name).unwrap();
        assert_eq!(read.as_deref(), Some("round-trip-value"));

        let deleted = delete_secret(&name).unwrap();
        assert!(deleted);

        let read_after_delete = read_secret(&name).unwrap();
        assert_eq!(read_after_delete, None);
    }

    #[test]
    fn missing_secret_returns_ok_none_not_an_error() {
        let name = format!("zen.test.runtime_missing.{}", std::process::id());
        let read = read_secret(&name).unwrap();
        assert_eq!(read, None);
    }

    #[test]
    fn legacy_target_fallback_still_works() {
        let name = format!("zen.test.runtime_legacy.{}", std::process::id());
        // Write directly under the legacy (pre-namespace) target, bypassing
        // `write_secret` (which always writes the namespaced form), to
        // simulate a secret that predates the `zen/default/...` namespace.
        credential_write_target(&legacy_target_name(&name), "legacy-value").unwrap();

        let read = read_secret(&name).unwrap();
        assert_eq!(read.as_deref(), Some("legacy-value"));

        let deleted = delete_secret(&name).unwrap();
        assert!(deleted);
    }

    #[test]
    fn credential_manager_secret_store_delegates_to_read_secret() {
        let name = format!("zen.test.runtime_store_trait.{}", std::process::id());
        write_secret(&name, "trait-value").unwrap();

        let store = CredentialManagerSecretStore;
        let read = SecretStore::read_secret(&store, &name).unwrap();
        assert_eq!(read.as_deref(), Some("trait-value"));

        delete_secret(&name).unwrap();
    }

    #[test]
    fn target_names_are_namespaced() {
        assert_eq!(target_name("dropbox.refresh_token"), "zen/default/dropbox/refresh_token");
    }

    #[test]
    fn legacy_target_names_remain_available_for_reads() {
        assert_eq!(legacy_target_name("dropbox.refresh_token"), "zen/dropbox.refresh_token");
    }

    #[test]
    fn friendly_names_hide_internal_profile_path() {
        assert_eq!(friendly_name("zen/default/dropbox/refresh_token").as_deref(), Some("dropbox.refresh_token"));
        assert_eq!(friendly_name("zen/dropbox.refresh_token").as_deref(), Some("dropbox.refresh_token"));
    }

    #[test]
    fn secret_names_reject_path_separators() {
        assert!(validate_name("dropbox.refresh_token").is_ok());
        assert!(validate_name("dropbox..refresh_token").is_err());
        assert!(validate_name("dropbox/refresh_token").is_err());
        assert!(validate_name("dropbox\\refresh_token").is_err());
    }

    #[test]
    fn list_secrets_includes_a_freshly_written_name() {
        let name = format!("zen.test.runtime_list.{}", std::process::id());
        write_secret(&name, "list-me").unwrap();

        let names = list_secrets().unwrap();
        assert!(names.contains(&name));

        delete_secret(&name).unwrap();
    }
}
