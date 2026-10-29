//! Timing log for debug builds, to see where delays come from. Written to `autokeylight.log` in the temp directory,
//! timestamps are UTC. Does nothing in release builds.

use std::fmt::Display;

#[cfg(debug_assertions)]
pub fn log(message: impl Display) {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = now.as_secs() % 86_400;
    let timestamp = format!(
        "{:02}:{:02}:{:02}.{:03}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60,
        now.subsec_millis()
    );

    // One write per line, so lines from different threads don't interleave
    let line = format!("{timestamp} {message}\n");
    let path = std::env::temp_dir().join("autokeylight.log");
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line.as_bytes());
    }
}

#[cfg(not(debug_assertions))]
pub fn log(_message: impl Display) {}
