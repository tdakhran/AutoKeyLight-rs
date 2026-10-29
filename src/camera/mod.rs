//! Detects whether any app is currently streaming from a camera.

#[cfg(windows)]
mod media_foundation;

/// Starts watching camera usage in the background.
///
/// `on_change` is called with the names of the cameras that are streaming, each time that changes.
/// An empty list means no camera is in use. Returns `false` if camera detection isn't supported on this platform.
pub fn spawn_monitor(on_change: impl Fn(Vec<String>) + Send + Sync + 'static) -> bool {
    #[cfg(windows)]
    return media_foundation::spawn_monitor(Box::new(on_change));

    #[cfg(not(windows))]
    {
        let _ = on_change;
        false
    }
}
