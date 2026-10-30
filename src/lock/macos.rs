//! The screen's lock state, from the current login session's dictionary, which has `CGSSessionScreenIsLocked` set
//! while it's locked.

use std::ffi::c_void;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGSessionCopyCurrentDictionary() -> *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(allocator: *const c_void, string: *const std::ffi::c_char, encoding: u32) -> *const c_void;
    fn CFDictionaryGetValue(dictionary: *const c_void, key: *const c_void) -> *const c_void;
    fn CFBooleanGetValue(boolean: *const c_void) -> u8;
    fn CFRelease(object: *const c_void);
}

const UTF8_ENCODING: u32 = 0x0800_0100;

pub fn is_locked() -> Option<bool> {
    unsafe {
        let session = CGSessionCopyCurrentDictionary();
        if session.is_null() {
            return None;
        }

        let key = CFStringCreateWithCString(std::ptr::null(), c"CGSSessionScreenIsLocked".as_ptr(), UTF8_ENCODING);
        let value = CFDictionaryGetValue(session, key);
        // Only present while the screen is locked
        let locked = !value.is_null() && CFBooleanGetValue(value) != 0;

        CFRelease(key);
        CFRelease(session);
        Some(locked)
    }
}
