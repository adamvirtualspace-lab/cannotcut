//! The window's title bar in the app's colours, so that it does not stand out white above it,
//! even while another window is in front.
//!
//! On Windows 11 the bar takes the window's own colour; Windows 10 cannot colour it, but does
//! draw it dark.
//!
//! The app draws its own minimise, maximise and close buttons. So that Windows 11 still offers
//! its snap layouts over the maximise button, Windows is told where that button is (see
//! [`MaximizeSpot`]).

use iced::advanced::widget::{Operation, Tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer};
use iced::{Element, Event, Length, Rectangle, Size, Task, Theme, Vector, window};

/// Colours the title bar of the window `id`, and lets Windows find its maximise button.
pub fn dress<Message: Send + 'static>(id: window::Id) -> Task<Message> {
    #[cfg(windows)]
    {
        window::run(id, move |window| {
            windows::dress(window);
            windows::watch(window, id);
        })
        .discard()
    }
    #[cfg(not(windows))]
    {
        let _ = id;
        Task::none()
    }
}

#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    use std::sync::Mutex;

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

    /// A window whose maximise button Windows is told about.
    struct Watched {
        id: iced::window::Id,
        window: isize,
        /// What handled the window's messages before: they all go on there.
        before: isize,
        /// Where its maximise button was last drawn, in logical pixels.
        button: Option<iced::Rectangle>,
        /// The pointer is on the button, so Windows counts it as outside the window's inside.
        over: bool,
    }

    static WATCHED: Mutex<Vec<Watched>> = Mutex::new(Vec::new());

    // From winuser.h.
    const GWLP_WNDPROC: i32 = -4;
    const WM_NCDESTROY: u32 = 0x0082;
    const WM_NCHITTEST: u32 = 0x0084;
    const WM_NCMOUSEMOVE: u32 = 0x00A0;
    const WM_NCLBUTTONDOWN: u32 = 0x00A1;
    const WM_NCLBUTTONUP: u32 = 0x00A2;
    const WM_NCLBUTTONDBLCLK: u32 = 0x00A3;
    const WM_NCMOUSELEAVE: u32 = 0x02A2;
    const WM_MOUSEMOVE: u32 = 0x0200;
    const WM_LBUTTONDOWN: u32 = 0x0201;
    const WM_LBUTTONUP: u32 = 0x0202;
    const WM_MOUSELEAVE: u32 = 0x02A3;
    const MK_LBUTTON: usize = 0x0001;
    const HTMAXBUTTON: isize = 9;
    const TME_LEAVE: u32 = 0x0000_0002;
    const TME_NONCLIENT: u32 = 0x0000_0010;

    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    struct TrackMouseEvent {
        size: u32,
        flags: u32,
        window: isize,
        hover_time: u32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn SetWindowLongPtrW(window: isize, index: i32, value: isize) -> isize;
        fn CallWindowProcW(
            before: isize,
            window: isize,
            message: u32,
            wparam: usize,
            lparam: isize,
        ) -> isize;
        fn ScreenToClient(window: isize, point: *mut Point) -> i32;
        fn GetDpiForWindow(window: isize) -> u32;
        fn GetCursorPos(point: *mut Point) -> i32;
        fn GetWindowRect(window: isize, rect: *mut Rect) -> i32;
        fn TrackMouseEvent(track: *mut TrackMouseEvent) -> i32;
    }

    #[repr(C)]
    struct VersionInfo {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        service_pack: [u16; 128],
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(info: *mut VersionInfo) -> i32;
    }

    /// Windows 11 (build 22000 on), the only one with snap layouts. Asked of the system
    /// itself: `GetVersionEx` gives a program without a manifest saying otherwise 8.
    fn has_snap_layouts() -> bool {
        let mut info = VersionInfo {
            size: size_of::<VersionInfo>() as u32,
            major: 0,
            minor: 0,
            build: 0,
            platform: 0,
            service_pack: [0; 128],
        };
        // SAFETY: a structure of our own, its size filled in, for Windows to fill.
        let known = unsafe { RtlGetVersion(&raw mut info) } == 0;
        known && info.major >= 10 && info.build >= 22000
    }

    /// Starts telling Windows where the maximise button of `window` (the app's `id`) is. On
    /// Windows 10, which has no snap layouts, the window is left as it was.
    pub fn watch(window: &dyn iced::window::Window, id: iced::window::Id) {
        if !has_snap_layouts() {
            return;
        }
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return;
        };
        let window = handle.hwnd.get();
        let Ok(mut watched) = WATCHED.lock() else {
            return;
        };
        if watched.iter().any(|watched| watched.window == window) {
            return;
        }
        let procedure: unsafe extern "system" fn(isize, u32, usize, isize) -> isize = spot_proc;
        // SAFETY: the window is the app's own and open; its messages now come here first and
        // all go on to what handled them before.
        let before = unsafe { SetWindowLongPtrW(window, GWLP_WNDPROC, procedure as isize) };
        if before == 0 {
            return;
        }
        watched.push(Watched {
            id,
            window,
            before,
            button: None,
            over: false,
        });
    }

    /// Notes where the maximise button of the app's window `id` is drawn.
    pub fn place(id: iced::window::Id, bounds: iced::Rectangle) {
        if let Ok(mut watched) = WATCHED.lock()
            && let Some(watched) = watched.iter_mut().find(|watched| watched.id == id)
        {
            watched.button = Some(bounds);
        }
    }

    /// The point in `lparam` (screen pixels, as Windows packs them) inside `window`, in
    /// physical pixels.
    fn inside(window: isize, lparam: isize) -> Point {
        let mut point = Point {
            x: i32::from(lparam as u16 as i16),
            y: i32::from((lparam >> 16) as u16 as i16),
        };
        // SAFETY: a point of our own for Windows to change.
        unsafe { ScreenToClient(window, &raw mut point) };
        point
    }

    /// A point inside the window packed as Windows packs one in a message.
    fn pack(point: &Point) -> isize {
        ((point.y as u16 as isize) << 16) | point.x as u16 as isize
    }

    /// Whether `point` (physical pixels inside the window) is on `button` (logical pixels).
    fn on(window: isize, button: Option<iced::Rectangle>, point: &Point) -> bool {
        let Some(button) = button else {
            return false;
        };
        // SAFETY: asks about the app's own open window.
        let dpi = unsafe { GetDpiForWindow(window) }.max(1);
        let scale = dpi as f32 / 96.0 * crate::theme::ui_scale();
        button.contains(iced::Point::new(point.x as f32 / scale, point.y as f32 / scale))
    }

    fn set_over(window: isize, over: bool) {
        if let Ok(mut watched) = WATCHED.lock()
            && let Some(watched) = watched.iter_mut().find(|watched| watched.window == window)
        {
            watched.over = over;
        }
    }

    /// Whether the pointer is off the window altogether.
    fn pointer_away(window: isize) -> bool {
        let mut cursor = Point { x: 0, y: 0 };
        let mut frame = Rect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: values of our own for Windows to fill in, about the app's own window.
        let known = unsafe {
            GetCursorPos(&raw mut cursor) != 0 && GetWindowRect(window, &raw mut frame) != 0
        };
        known
            && !((frame.left..frame.right).contains(&cursor.x)
                && (frame.top..frame.bottom).contains(&cursor.y))
    }

    /// The watched windows' messages, first. Over the maximise button Windows hears "the
    /// maximise button", which is what makes Windows 11 offer its snap layouts. The pointer's
    /// moves and presses there then come as the window's outside ones; they go on to the app
    /// as inside ones, so the button lights up and works as the others do.
    unsafe extern "system" fn spot_proc(
        window: isize,
        message: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize {
        let found = WATCHED.lock().ok().and_then(|watched| {
            let watched = watched.iter().find(|watched| watched.window == window)?;
            Some((watched.before, watched.button, watched.over))
        });
        let Some((before, button, over)) = found else {
            return 0;
        };
        // SAFETY (for every call below): `before` is the procedure the window had, called as
        // Windows would have called it, for the window it belongs to.
        let pass = |message: u32, wparam: usize, lparam: isize| unsafe {
            CallWindowProcW(before, window, message, wparam, lparam)
        };
        let on_button = wparam as isize == HTMAXBUTTON;
        match message {
            WM_NCHITTEST => {
                let here = on(window, button, &inside(window, lparam));
                if here != over {
                    set_over(window, here);
                }
                if here {
                    return HTMAXBUTTON;
                }
            }
            WM_NCMOUSEMOVE if on_button => {
                let mut track = TrackMouseEvent {
                    size: size_of::<TrackMouseEvent>() as u32,
                    flags: TME_LEAVE | TME_NONCLIENT,
                    window,
                    hover_time: 0,
                };
                // SAFETY: a request of our own about the app's window.
                unsafe { TrackMouseEvent(&raw mut track) };
                pass(WM_MOUSEMOVE, 0, pack(&inside(window, lparam)));
            }
            WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if on_button => {
                pass(WM_LBUTTONDOWN, MK_LBUTTON, pack(&inside(window, lparam)));
                return 0;
            }
            WM_NCLBUTTONUP if on_button => {
                pass(WM_LBUTTONUP, 0, pack(&inside(window, lparam)));
                return 0;
            }
            // The app keeps the pointer as on its window while it is on the button.
            WM_MOUSELEAVE if over => return 0,
            WM_NCMOUSELEAVE if over => {
                set_over(window, false);
                if pointer_away(window) {
                    pass(WM_MOUSELEAVE, 0, 0);
                }
            }
            WM_NCDESTROY => {
                if let Ok(mut watched) = WATCHED.lock() {
                    watched.retain(|watched| watched.window != window);
                }
                // SAFETY: the window gets back the procedure it had.
                unsafe { SetWindowLongPtrW(window, GWLP_WNDPROC, before) };
            }
            _ => {}
        }
        pass(message, wparam, lparam)
    }

    /// A colour as Windows takes it: 0x00BBGGRR.
    fn colorref(color: Color) -> u32 {
        let [red, green, blue, _] = color.into_rgba8();
        u32::from(red) | u32::from(green) << 8 | u32::from(blue) << 16
    }
}

/// A window's maximise button, wrapped to note where it is drawn, so that Windows can be told:
/// with the pointer over it, Windows 11 offers its snap layouts.
pub struct MaximizeSpot<'a, Message> {
    window: window::Id,
    content: Element<'a, Message>,
}

impl<'a, Message> MaximizeSpot<'a, Message> {
    pub fn new(window: window::Id, content: impl Into<Element<'a, Message>>) -> Self {
        Self {
            window,
            content: content.into(),
        }
    }
}

impl<Message> Widget<Message, Theme, iced::Renderer> for MaximizeSpot<'_, Message> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let inner = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        layout::Node::with_children(inner.size(), vec![inner])
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        #[cfg(windows)]
        windows::place(self.window, layout.bounds());
        let inner = layout.children().next().expect("the button inside");
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            inner,
            cursor,
            viewport,
        );
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        let inner = layout.children().next().expect("the button inside");
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], inner, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let inner = layout.children().next().expect("the button inside");
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            inner,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let inner = layout.children().next().expect("the button inside");
        self.content
            .as_widget()
            .mouse_interaction(&tree.children[0], inner, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        let inner = layout.children().next().expect("the button inside");
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            inner,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: 'a> From<MaximizeSpot<'a, Message>> for Element<'a, Message> {
    fn from(spot: MaximizeSpot<'a, Message>) -> Self {
        Element::new(spot)
    }
}
