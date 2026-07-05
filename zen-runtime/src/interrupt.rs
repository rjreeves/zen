use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, Once};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
static INSTALL: Once = Once::new();
static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Serializes tests that exercise process-level interrupt state (a single
/// process-wide `AtomicBool`), so they don't race each other. Exposed
/// unconditionally (not `#[cfg(test)]`) because `cfg(test)` only applies
/// when this crate itself is the one under test - it would be compiled out
/// for the `zen` binary's own test runs, which are the actual callers.
pub fn lock_for_test() -> MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(|err| err.into_inner())
}

pub fn install_ctrl_c_handler() -> Result<(), String> {
    let mut result = Ok(());

    INSTALL.call_once(|| {
        result = install_platform_handler();
    });

    result
}

pub fn request_interrupt() {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

pub fn clear_interrupt() {
    INTERRUPTED.store(false, Ordering::SeqCst);
}

pub fn take_interrupt() -> bool {
    INTERRUPTED.swap(false, Ordering::SeqCst)
}

pub fn is_interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

#[cfg(windows)]
fn install_platform_handler() -> Result<(), String> {
    let ok = unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), 1) };
    if ok == 0 {
        Err(format!(
            "Failed to install Ctrl+C handler: {}",
            std::io::Error::last_os_error()
        ))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn install_platform_handler() -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> i32 {
    match ctrl_type {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => {
            request_interrupt();
            1
        }
        _ => 0,
    }
}

#[cfg(windows)]
const CTRL_C_EVENT: u32 = 0;
#[cfg(windows)]
const CTRL_BREAK_EVENT: u32 = 1;

#[cfg(windows)]
extern "system" {
    fn SetConsoleCtrlHandler(
        handler: Option<unsafe extern "system" fn(u32) -> i32>,
        add: i32,
    ) -> i32;
}
