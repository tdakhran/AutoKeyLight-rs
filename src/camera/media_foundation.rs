//! Windows camera detection using the Media Foundation sensor activity monitor.
//!
//! Windows 11 26200+ no longer records camera usage in the CapabilityAccessManager registry keys,
//! so we ask Media Foundation which processes are streaming instead.
//! https://learn.microsoft.com/en-us/windows/win32/api/mfidl/nn-mfidl-imfsensoractivitymonitor

use std::sync::{mpsc, Mutex, Once};
use std::thread;

use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFAttributes, IMFSensorActivitiesReport, IMFSensorActivitiesReportCallback,
    IMFSensorActivitiesReportCallback_Impl, IMFSensorActivityMonitor, IMFSensorActivityReport, MFCreateAttributes,
    MFCreateSensorActivityMonitor, MFEnumDeviceSources, MFStartup, MFSTARTUP_FULL, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID, MF_VERSION,
};
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};
use windows_core::{implement, Ref, Result, PWSTR};

type OnChange = Box<dyn Fn(Vec<String>) + Send + Sync>;

#[implement(IMFSensorActivitiesReportCallback)]
struct ReportCallback {
    streaming_cameras: Mutex<Vec<String>>,
    on_change: OnChange,
}

impl IMFSensorActivitiesReportCallback_Impl for ReportCallback_Impl {
    fn OnActivitiesReport(&self, report: Ref<IMFSensorActivitiesReport>) -> Result<()> {
        let cameras = streaming_cameras(report.ok()?)?;

        // Reports arrive for any change, including apps briefly opening the camera, only pass on actual changes
        let mut previous = self.streaming_cameras.lock().unwrap();
        if *previous != cameras {
            *previous = cameras.clone();
            (self.on_change)(cameras);
        }

        Ok(())
    }
}

/// Names of the cameras that are streaming. Apps often open a camera just to enumerate it,
/// so only cameras with a process actually streaming count.
fn streaming_cameras(report: &IMFSensorActivitiesReport) -> Result<Vec<String>> {
    let mut cameras = Vec::new();

    unsafe {
        for device_index in 0..report.GetCount()? {
            let device = report.GetActivityReport(device_index)?;

            let mut streaming = false;
            for process_index in 0..device.GetProcessCount()? {
                streaming |= device.GetProcessActivity(process_index)?.GetStreamingState()?.as_bool();
            }

            if streaming {
                cameras.push(friendly_name(&device).unwrap_or_else(|_| "Unknown camera".to_string()));
            }
        }
    }

    // The same camera can be reported more than once
    cameras.sort();
    cameras.dedup();
    Ok(cameras)
}

fn friendly_name(device: &IMFSensorActivityReport) -> Result<String> {
    let mut name = [0u16; 256];
    let mut written = 0u32;

    unsafe { device.GetFriendlyName(&mut name, &mut written)? };

    let length = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    Ok(String::from_utf16_lossy(&name[..length]))
}

/// Names of the connected cameras. Lists them without opening them, so it doesn't count as using a camera.
pub fn connected_cameras() -> Vec<String> {
    static STARTUP: Once = Once::new();

    unsafe {
        // Fails harmlessly when the thread already set up COM differently, enumerating works either way
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        STARTUP.call_once(|| {
            let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
        });
    }

    let mut cameras = enumerate_cameras().unwrap_or_default();
    cameras.sort();
    cameras.dedup();
    cameras
}

fn enumerate_cameras() -> Result<Vec<String>> {
    unsafe {
        let mut attributes: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attributes, 1)?;
        let attributes = attributes.ok_or_else(windows_core::Error::empty)?;
        attributes.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)?;

        let mut devices: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        MFEnumDeviceSources(&attributes, &mut devices, &mut count)?;

        let mut names = Vec::new();
        for index in 0..count as usize {
            // Taking each one releases it when it goes out of scope
            if let Some(device) = (*devices.add(index)).take() {
                let mut name = PWSTR::null();
                let mut length = 0u32;
                if device.GetAllocatedString(&MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, &mut name, &mut length).is_ok() {
                    names.push(name.to_string().unwrap_or_default());
                    CoTaskMemFree(Some(name.0.cast()));
                }
            }
        }
        CoTaskMemFree(Some(devices.cast()));

        Ok(names)
    }
}

fn start_monitor(on_change: OnChange) -> Result<IMFSensorActivityMonitor> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;

        let callback: IMFSensorActivitiesReportCallback =
            ReportCallback { streaming_cameras: Mutex::new(Vec::new()), on_change }.into();
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
