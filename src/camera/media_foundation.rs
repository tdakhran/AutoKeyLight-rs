//! Windows camera detection using the Media Foundation sensor activity monitor.
//!
//! Windows 11 26200+ no longer records camera usage in the CapabilityAccessManager registry keys,
//! so we ask Media Foundation which processes are streaming instead.
//! https://learn.microsoft.com/en-us/windows/win32/api/mfidl/nn-mfidl-imfsensoractivitymonitor

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

use windows::Win32::Media::MediaFoundation::{
    IMFSensorActivitiesReport, IMFSensorActivitiesReportCallback,
    IMFSensorActivitiesReportCallback_Impl, IMFSensorActivityMonitor,
    MFCreateSensorActivityMonitor, MFStartup, MFSTARTUP_FULL, MF_VERSION,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows_core::{implement, Ref, Result};

type OnChange = Box<dyn Fn(bool) + Send + Sync>;

#[implement(IMFSensorActivitiesReportCallback)]
struct ReportCallback {
    streaming: AtomicBool,
    on_change: OnChange,
}

impl IMFSensorActivitiesReportCallback_Impl for ReportCallback_Impl {
    fn OnActivitiesReport(&self, report: Ref<IMFSensorActivitiesReport>) -> Result<()> {
        let streaming = any_process_streaming(report.ok()?)?;

        // Reports arrive for any change, including apps briefly opening the camera, only pass on actual changes
        if self.streaming.swap(streaming, Ordering::Relaxed) != streaming {
            (self.on_change)(streaming);
        }

        Ok(())
    }
}

/// Apps often open the camera just to enumerate it, so only count the ones actually streaming.
fn any_process_streaming(report: &IMFSensorActivitiesReport) -> Result<bool> {
    unsafe {
        for device_index in 0..report.GetCount()? {
            let device = report.GetActivityReport(device_index)?;

            for process_index in 0..device.GetProcessCount()? {
                if device.GetProcessActivity(process_index)?.GetStreamingState()?.as_bool() {
                    return Ok(true);
                }
            }
        }
    }

    Ok(false)
}

fn start_monitor(on_change: OnChange) -> Result<IMFSensorActivityMonitor> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;

        let callback: IMFSensorActivitiesReportCallback =
            ReportCallback { streaming: AtomicBool::new(false), on_change }.into();
        let monitor = MFCreateSensorActivityMonitor(&callback)?;
        monitor.Start()?;

        Ok(monitor)
    }
}

pub fn spawn_monitor(on_change: OnChange) -> bool {
    let (started_tx, started_rx) = mpsc::channel();

    let spawned = thread::Builder::new()
        .name("camera-monitor".into())
        .spawn(move || match start_monitor(on_change) {
            Ok(_monitor) => {
                let _ = started_tx.send(true);

                // Keep the monitor alive, reports arrive on Media Foundation's own threads
                loop {
                    thread::park();
                }
            }
            Err(error) => {
                eprintln!("Camera monitor unavailable: {error}");
                let _ = started_tx.send(false);
            }
        });

    spawned.is_ok() && started_rx.recv().unwrap_or(false)
}
