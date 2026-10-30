//! The session's lock state, from the Remote Desktop Services API that also covers the local session.

use windows::core::PWSTR;
use windows::Win32::System::RemoteDesktop::{
    WTSFreeMemory, WTSQuerySessionInformationW, WTSSessionInfoEx, WTSINFOEXW, WTS_CURRENT_SERVER_HANDLE,
    WTS_CURRENT_SESSION, WTS_SESSIONSTATE_LOCK,
};

pub fn is_locked() -> Option<bool> {
    let mut buffer = PWSTR::null();
    let mut size = 0u32;

    unsafe {
        WTSQuerySessionInformationW(
            Some(WTS_CURRENT_SERVER_HANDLE),
            WTS_CURRENT_SESSION,
            WTSSessionInfoEx,
            &mut buffer,
            &mut size,
        )
        .ok()?;

        let info = &*(buffer.0 as *const WTSINFOEXW);
        let locked = (info.Level == 1).then(|| info.Data.WTSInfoExLevel1.SessionFlags as u32 == WTS_SESSIONSTATE_LOCK);

        WTSFreeMemory(buffer.0.cast());
        locked
    }
}
