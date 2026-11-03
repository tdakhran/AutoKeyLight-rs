//! Hands memory the app freed back to the operating system.
//!
//! Freed memory normally stays with the allocator, ready for the next allocation. That's the right call while the app
//! is busy, but in the tray it can wait hours for the next one, so it's returned after bursts of work instead: loading
//! the calendar, closing the window, looking for lights.

use crate::log::log;

pub fn release() {
    #[cfg(debug_assertions)]
    log(format_args!(
        "memory: rust holds {} KB in {} allocations",
        counting::BYTES.load(std::sync::atomic::Ordering::Relaxed) / 1024,
        counting::ALLOCATIONS.load(std::sync::atomic::Ordering::Relaxed)
    ));

    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Memory::{GetProcessHeap, HeapCompact, HEAP_FLAGS};

        // Rust allocates from the process heap, compacting it decommits its free ranges
        if let Ok(heap) = GetProcessHeap() {
            let largest_free = HeapCompact(heap, HEAP_FLAGS(0));
            log(format_args!("memory: heap compacted, largest free block {largest_free} bytes"));
        }
    }

    #[cfg(target_os = "macos")]
    unsafe {
        extern "C" {
            fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
        }

        // All zones, as much as possible
        let released = malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
        log(format_args!("memory: released {released} bytes"));
    }
}

/// Counts what Rust code holds in debug builds, to tell it apart from what the system libraries allocate
#[cfg(debug_assertions)]
mod counting {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    pub static BYTES: AtomicUsize = AtomicUsize::new(0);
    pub static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

    struct Counting;

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let pointer = unsafe { System.alloc(layout) };
            if !pointer.is_null() {
                BYTES.fetch_add(layout.size(), Relaxed);
                ALLOCATIONS.fetch_add(1, Relaxed);
            }
            pointer
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let pointer = unsafe { System.alloc_zeroed(layout) };
            if !pointer.is_null() {
                BYTES.fetch_add(layout.size(), Relaxed);
                ALLOCATIONS.fetch_add(1, Relaxed);
            }
            pointer
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            unsafe { System.dealloc(pointer, layout) };
            BYTES.fetch_sub(layout.size(), Relaxed);
            ALLOCATIONS.fetch_sub(1, Relaxed);
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            let new_pointer = unsafe { System.realloc(pointer, layout, new_size) };
            if !new_pointer.is_null() {
                BYTES.fetch_add(new_size, Relaxed);
                BYTES.fetch_sub(layout.size(), Relaxed);
            }
            new_pointer
        }
    }

    #[global_allocator]
    static ALLOCATOR: Counting = Counting;
}
