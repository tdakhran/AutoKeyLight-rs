//! Detects whether any app is currently streaming from a camera.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[cfg(windows)]
mod media_foundation;

/// Starts watching camera usage in the background.
///
/// Returns a flag that is kept up to date with whether any camera is streaming,
/// or `None` if camera detection isn't supported on this platform.
pub fn spawn_monitor() -> Option<Arc<AtomicBool>> {
    #[cfg(windows)]
    return media_foundation::spawn_monitor();

    #[cfg(not(windows))]
    return None;
}
