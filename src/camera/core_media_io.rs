//! macOS camera detection using CoreMediaIO.
//!
//! Each camera has a "running somewhere" property that's set while any process streams from it. We listen for changes
//! to it on every camera, and to the list of cameras so plugged in cameras are watched too. Listeners are blocks
//! dispatched on our own queue, the run loop based listeners never fire without the main run loop.

use std::ffi::{c_char, c_void, CStr};
use std::mem::size_of;
use std::ptr;
use std::sync::{Mutex, OnceLock};

use block2::{Block, RcBlock};

type OsStatus = i32;
type ObjectId = u32;
type DispatchQueue = *mut c_void;
type ListenerBlock = Block<dyn Fn(u32, *const c_void)>;

#[repr(C)]
struct PropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

const fn four_char_code(code: &[u8; 4]) -> u32 {
    (code[0] as u32) << 24 | (code[1] as u32) << 16 | (code[2] as u32) << 8 | code[3] as u32
}

const SYSTEM_OBJECT: ObjectId = 1;
const HARDWARE_PROPERTY_DEVICES: u32 = four_char_code(b"dev#");
const DEVICE_PROPERTY_IS_RUNNING_SOMEWHERE: u32 = four_char_code(b"gone");
const OBJECT_PROPERTY_NAME: u32 = four_char_code(b"lnam");
const SCOPE_GLOBAL: u32 = four_char_code(b"glob");
const ELEMENT_MAIN: u32 = 0;
const UTF8_ENCODING: u32 = 0x0800_0100;

#[link(name = "CoreMediaIO", kind = "framework")]
extern "C" {
    fn CMIOObjectGetPropertyDataSize(
        object: ObjectId,
        address: *const PropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        data_size: *mut u32,
    ) -> OsStatus;
    fn CMIOObjectGetPropertyData(
        object: ObjectId,
        address: *const PropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        data_size: u32,
        data_used: *mut u32,
        data: *mut c_void,
    ) -> OsStatus;
    fn CMIOObjectAddPropertyListenerBlock(
        object: ObjectId,
        address: *const PropertyAddress,
        queue: DispatchQueue,
        listener: &ListenerBlock,
    ) -> OsStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringGetCString(string: *const c_void, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFRelease(object: *const c_void);
}

// libdispatch is part of libSystem, which every binary links
extern "C" {
    fn dispatch_queue_create(label: *const c_char, attributes: *const c_void) -> DispatchQueue;
}

fn address(selector: u32) -> PropertyAddress {
    PropertyAddress { selector, scope: SCOPE_GLOBAL, element: ELEMENT_MAIN }
}

fn devices() -> Vec<ObjectId> {
    let address = address(HARDWARE_PROPERTY_DEVICES);
    let mut size = 0u32;

    unsafe {
        if CMIOObjectGetPropertyDataSize(SYSTEM_OBJECT, &address, 0, ptr::null(), &mut size) != 0 {
            return Vec::new();
        }

        let mut devices = vec![0 as ObjectId; size as usize / size_of::<ObjectId>()];
        let mut used = 0u32;
        let status = CMIOObjectGetPropertyData(
            SYSTEM_OBJECT,
            &address,
            0,
            ptr::null(),
            size,
            &mut used,
            devices.as_mut_ptr().cast(),
        );
        if status != 0 {
            return Vec::new();
        }

        devices.truncate(used as usize / size_of::<ObjectId>());
        devices
    }
}

fn is_running_somewhere(device: ObjectId) -> bool {
    let address = address(DEVICE_PROPERTY_IS_RUNNING_SOMEWHERE);
    let mut running = 0u32;
    let mut used = 0u32;

    let status = unsafe {
        CMIOObjectGetPropertyData(
            device,
            &address,
            0,
            ptr::null(),
            size_of::<u32>() as u32,
            &mut used,
            (&mut running as *mut u32).cast(),
        )
    };

    status == 0 && running != 0
}

fn name(device: ObjectId) -> Option<String> {
    let address = address(OBJECT_PROPERTY_NAME);
    let mut string: *const c_void = ptr::null();
    let mut used = 0u32;

    unsafe {
        let status = CMIOObjectGetPropertyData(
            device,
            &address,
            0,
            ptr::null(),
            size_of::<*const c_void>() as u32,
            &mut used,
            (&mut string as *mut *const c_void).cast(),
        );
        if status != 0 || string.is_null() {
            return None;
        }

        let mut buffer = [0 as c_char; 256];
        let converted = CFStringGetCString(string, buffer.as_mut_ptr(), buffer.len() as isize, UTF8_ENCODING) != 0;
        CFRelease(string);

        converted.then(|| CStr::from_ptr(buffer.as_ptr()).to_string_lossy().into_owned())
    }
}

/// Names of the cameras that are streaming
pub fn streaming_cameras() -> Vec<String> {
    let mut cameras: Vec<String> = devices()
        .into_iter()
        .filter(|&device| is_running_somewhere(device))
        .map(|device| name(device).unwrap_or_else(|| "Unknown camera".to_string()))
        .collect();

    cameras.sort();
    cameras.dedup();
    cameras
}

type OnChange = Box<dyn Fn(Vec<String>) + Send + Sync>;

struct Monitor {
    on_change: OnChange,
    /// Serial queue the listeners run on, so checks never overlap
    queue: usize,
    streaming_cameras: Mutex<Vec<String>>,
    /// Cameras with a listener on their "running somewhere" property
    watched: Mutex<Vec<ObjectId>>,
}

static MONITOR: OnceLock<Monitor> = OnceLock::new();

impl Monitor {
    fn listen(&self, object: ObjectId, selector: u32) -> OsStatus {
        // CoreMediaIO keeps its own reference to the block
        let listener = RcBlock::new(|_address_count: u32, _addresses: *const c_void| {
            if let Some(monitor) = MONITOR.get() {
                monitor.check();
            }
        });

        unsafe { CMIOObjectAddPropertyListenerBlock(object, &address(selector), self.queue as DispatchQueue, &listener) }
    }

    fn watch_new_devices(&self) {
        let mut watched = self.watched.lock().unwrap();

        for device in devices() {
            if !watched.contains(&device) && self.listen(device, DEVICE_PROPERTY_IS_RUNNING_SOMEWHERE) == 0 {
                watched.push(device);
            }
        }
    }

    /// Only passes on actual changes, listeners can fire without the set of streaming cameras changing
    fn check(&self) {
        self.watch_new_devices();

        let cameras = streaming_cameras();
        let mut previous = self.streaming_cameras.lock().unwrap();
        if *previous != cameras {
            *previous = cameras.clone();
            (self.on_change)(cameras);
        }
    }
}

pub fn spawn_monitor(on_change: OnChange) -> bool {
    let queue = unsafe { dispatch_queue_create(c"camera-monitor".as_ptr(), ptr::null()) };
    if queue.is_null() {
        return false;
    }

    let monitor = Monitor {
        on_change,
        queue: queue as usize,
        streaming_cameras: Mutex::new(Vec::new()),
        watched: Mutex::new(Vec::new()),
    };
    if MONITOR.set(monitor).is_err() {
        return false;
    }
    let monitor = MONITOR.get().unwrap();

    // Cameras being plugged in or out
    if monitor.listen(SYSTEM_OBJECT, HARDWARE_PROPERTY_DEVICES) != 0 {
        return false;
    }

    monitor.check();
    true
}
