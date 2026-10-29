//! Detects whether any app is currently streaming from a camera.

#[cfg(windows)]
mod media_foundation;

/// Starts watching camera usage in the background.
///
/// `on_change` is called with whether any camera is streaming, each time that changes.
/// Returns `false` if camera detection isn't supported on this platform.
pub fn spawn_monitor(on_change: impl Fn(bool) + Send + Sync + 'static) -> bool {
    #[cfg(windows)]
    return media_foundation::spawn_monitor(Box::new(on_change));

    #[cfg(not(windows))]
    {
        let _ = on_change;
        false
    }
}
