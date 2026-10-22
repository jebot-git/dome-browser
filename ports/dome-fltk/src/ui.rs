// SPDX-License-Identifier: GPL-3.0-or-later
use std::ffi::{CStr, CString, c_char};

#[repr(C)]
pub struct Event {
    pub kind: i32,
    pub a: i32,
    pub b: i32,
    pub c: i32,
    pub d: i32,
    pub text: [c_char; 8192],
}
impl Default for Event {
    fn default() -> Self {
        Self {
            kind: 0,
            a: 0,
            b: 0,
            c: 0,
            d: 0,
            text: [0; 8192],
        }
    }
}
impl Event {
    pub fn text(&self) -> String {
        unsafe {
            CStr::from_ptr(self.text.as_ptr())
                .to_string_lossy()
                .into_owned()
        }
    }
}
unsafe extern "C" {
    pub fn browser_ui_open(url: *const c_char) -> i32;
    pub fn browser_ui_wait(seconds: f64);
    pub fn browser_ui_poll(event: *mut Event) -> i32;
    pub fn browser_ui_wake();
    pub fn browser_ui_close();
    pub fn browser_ui_frame(pixels: *const u8, width: u32, height: u32);
    pub fn browser_ui_title(title: *const c_char, url: *const c_char, back: i32, forward: i32);
    pub fn browser_ui_tabs(json: *const c_char, active: i32);
    pub fn browser_ui_status(text: *const c_char);
    pub fn browser_ui_cursor(cursor: i32);
    pub fn browser_ui_fullscreen(enabled: i32);
    pub fn browser_ui_ask(title: *const c_char, message: *const c_char) -> i32;
    pub fn browser_ui_prompt(message: *const c_char, value: *const c_char) -> *const c_char;
    pub fn browser_ui_password(message: *const c_char) -> *const c_char;
    pub fn browser_ui_color(red: *mut u8, green: *mut u8, blue: *mut u8) -> i32;
    pub fn browser_ui_copy(text: *const c_char);
    pub fn browser_ui_paste() -> *const c_char;
    pub fn browser_ui_file(multiple: i32) -> *const c_char;
    pub fn browser_ui_select(json: *const c_char, x: i32, y: i32) -> i32;
    pub fn browser_ui_notification(json: *const c_char);
    pub fn browser_ui_width() -> i32;
    pub fn browser_ui_height() -> i32;
    pub fn browser_ui_light() -> i32;
    pub fn browser_ui_screenshot(path: *const c_char) -> i32;
}
pub fn c(text: &str) -> CString {
    CString::new(text.replace('\0', "�")).unwrap()
}
pub fn ask(title: &str, message: &str) -> bool {
    unsafe { browser_ui_ask(c(title).as_ptr(), c(message).as_ptr()) != 0 }
}
pub fn status(text: &str) {
    unsafe { browser_ui_status(c(text).as_ptr()) }
}
pub fn string(pointer: *const c_char) -> Option<String> {
    if pointer.is_null() {
        None
    } else {
        Some(
            unsafe { CStr::from_ptr(pointer) }
                .to_string_lossy()
                .into_owned(),
        )
    }
}
pub fn dimensions() -> dpi::PhysicalSize<u32> {
    unsafe {
        dpi::PhysicalSize::new(
            browser_ui_width().max(1) as u32,
            browser_ui_height().max(1) as u32,
        )
    }
}
