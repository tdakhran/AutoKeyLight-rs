//! Whether the computer is locked, checked when a meeting starts so reminders aren't blinked to an empty desk.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

/// Whether the computer is locked right now. `false` if it can't be told, so reminders still blink.
pub fn is_locked() -> bool {
    #[cfg(windows)]
    return windows::is_locked().unwrap_or(false);

    #[cfg(target_os = "macos")]
    return macos::is_locked().unwrap_or(false);

    #[cfg(not(any(windows, target_os = "macos")))]
    false
}
