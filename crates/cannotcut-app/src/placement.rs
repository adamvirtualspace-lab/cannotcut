//! Where a window is on the screens, so that a panel's own window opens where it was left the
//! next time: its place and size when not maximized, and whether it is maximized.
//!
//! Kept in the screens' own pixels, as Windows keeps it, so that it means the same whatever
//! each screen's scaling. A place on a screen that is no longer there is not used. Elsewhere
//! than Windows, nothing is kept and windows open where the system puts them.

use iced::{Task, window};
use serde_json::{Value, json};

/// A window's place on the screens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// Left, top, right and bottom when not maximized, in screen pixels.
    pub normal: [i32; 4],
    pub maximized: bool,
}

impl Placement {
    pub fn value(&self) -> Value {
        json!({ "normal": self.normal, "maximized": self.maximized })
    }

    pub fn from_value(value: &Value) -> Option<Self> {
        let normal = value.get("normal")?.as_array()?;
        let mut sides = [0; 4];
        for (side, kept) in sides.iter_mut().zip(normal) {
            *side = i32::try_from(kept.as_i64()?).ok()?;
        }
        let [left, top, right, bottom] = sides;
        (normal.len() == 4 && right > left && bottom > top).then_some(Self {
            normal: sides,
            maximized: value.get("maximized")?.as_bool()?,
        })
    }
}

/// Where the window `id` is now.
pub fn read(id: window::Id) -> Task<Option<Placement>> {
    #[cfg(windows)]
    {
        window::run(id, windows::read)
    }
    #[cfg(not(windows))]
    {
        let _ = id;
        Task::done(None)
    }
}

/// Puts the window `id` where `placement` says, if that is on a screen there now.
pub fn apply<Message: Send + 'static>(id: window::Id, placement: Placement) -> Task<Message> {
    #[cfg(windows)]
    {
        window::run(id, move |window| windows::apply(window, placement)).discard()
    }
    #[cfg(not(windows))]
    {
        let _ = (id, placement);
        Task::none()
    }
}

#[cfg(windows)]
mod windows {
    use raw_window_handle::RawWindowHandle;

    use super::Placement;

    const SW_SHOWNORMAL: u32 = 1;
    const SW_SHOWMAXIMIZED: u32 = 3;
    const MONITOR_DEFAULTTONULL: u32 = 0;

    #[repr(C)]
    #[derive(Default)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    /// WINDOWPLACEMENT, from winuser.h.
    #[repr(C)]
    #[derive(Default)]
    struct WindowPlacement {
        length: u32,
        flags: u32,
        show: u32,
        least: Point,
        most: Point,
        normal: Rect,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetWindowPlacement(window: isize, placement: *mut WindowPlacement) -> i32;
        fn SetWindowPlacement(window: isize, placement: *const WindowPlacement) -> i32;
        fn MonitorFromRect(rect: *const Rect, flags: u32) -> isize;
    }

    fn handle(window: &dyn iced::window::Window) -> Option<isize> {
        let handle = window.window_handle().ok()?;
        match handle.as_raw() {
            RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
            _ => None,
        }
    }

    fn blank() -> WindowPlacement {
        WindowPlacement {
            length: size_of::<WindowPlacement>() as u32,
            ..WindowPlacement::default()
        }
    }

    pub fn read(window: &dyn iced::window::Window) -> Option<Placement> {
        let window = handle(window)?;
        let mut placement = blank();
        // SAFETY: the window is the app's own and open while this runs; the struct is
        // WINDOWPLACEMENT with its length set, as the call asks.
        let read = unsafe { GetWindowPlacement(window, &raw mut placement) };
        if read == 0 {
            return None;
        }
        let normal = &placement.normal;
        Some(Placement {
            normal: [normal.left, normal.top, normal.right, normal.bottom],
            maximized: placement.show == SW_SHOWMAXIMIZED,
        })
    }

    pub fn apply(window: &dyn iced::window::Window, placement: Placement) {
        let Some(window) = handle(window) else {
            return;
        };
        let [left, top, right, bottom] = placement.normal;
        let normal = Rect {
            left,
            top,
            right,
            bottom,
        };
        // SAFETY: a plain rectangle, read and not kept.
        let on_a_screen = unsafe { MonitorFromRect(&raw const normal, MONITOR_DEFAULTTONULL) };
        if on_a_screen == 0 {
            return;
        }
        let wanted = WindowPlacement {
            show: if placement.maximized { SW_SHOWMAXIMIZED } else { SW_SHOWNORMAL },
            normal,
            ..blank()
        };
        // SAFETY: as in `read`; Windows copies the struct.
        unsafe {
            SetWindowPlacement(window, &raw const wanted);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_placement_comes_back_as_kept_and_nonsense_is_refused() {
        let placement = Placement {
            normal: [1920, 431, 3000, 1100],
            maximized: true,
        };
        assert_eq!(Placement::from_value(&placement.value()), Some(placement));
        let flat = json!({ "normal": [10, 10, 10, 50], "maximized": false });
        assert_eq!(Placement::from_value(&flat), None);
        let short = json!({ "normal": [10, 10, 50], "maximized": false });
        assert_eq!(Placement::from_value(&short), None);
    }
}
