//! The window's title bar in the app's colours, so that it does not stand out white above it,
//! even while another window is in front.
//!
//! On Windows 11 the bar takes the window's own colour; Windows 10 cannot colour it, but does
//! draw it dark.

use iced::Task;

/// Colours the title bar of the app's window.
pub fn dress<Message: Send + 'static>() -> Task<Message> {
    #[cfg(windows)]
    {
        use iced::window;
        window::latest()
            .and_then(|id| window::run(id, windows::dress))
            .discard()
    }
    #[cfg(not(windows))]
    {
        Task::none()
    }
}

#[cfg(windows)]
mod windows {
    use std::ffi::c_void;

    use iced::Color;
    use raw_window_handle::RawWindowHandle;

    use crate::theme;

    // From dwmapi.h. Those past 20 are known to Windows 11 only; Windows 10 turns them down,
    // which does no harm.
    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const DWMWA_BORDER_COLOR: u32 = 34;
    const DWMWA_CAPTION_COLOR: u32 = 35;
    const DWMWA_TEXT_COLOR: u32 = 36;

    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(window: isize, attribute: u32, value: *const c_void, size: u32)
        -> i32;
    }

    pub fn dress(window: &dyn iced::window::Window) {
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return;
        };
        let window = handle.hwnd.get();
        let set = |attribute: u32, value: u32| {
            // SAFETY: the window is the app's own and open while this runs, and each value is
            // the 32-bit number DWM reads for its attribute.
            unsafe {
                DwmSetWindowAttribute(window, attribute, (&raw const value).cast(), 4);
            }
        };
        set(DWMWA_USE_IMMERSIVE_DARK_MODE, 1);
        set(DWMWA_CAPTION_COLOR, colorref(theme::WINDOW));
        set(DWMWA_BORDER_COLOR, colorref(theme::WINDOW));
        set(DWMWA_TEXT_COLOR, colorref(theme::TEXT));
    }

    /// A colour as Windows takes it: 0x00BBGGRR.
    fn colorref(color: Color) -> u32 {
        let [red, green, blue, _] = color.into_rgba8();
        u32::from(red) | u32::from(green) << 8 | u32::from(blue) << 16
    }
}
