//! The icons on buttons and tabs: Tabler Icons (MIT, see `icons/LICENSE`), kept in the binary.
//!
//! An icon is drawn in one colour, given where it is used: a button's words take their colour
//! from the button's style, but an icon cannot, so it is told.

use std::sync::LazyLock;

use iced::widget::svg::{self, Svg};
use iced::{Color, Element};

use crate::theme;

/// The size of an icon on a button, in logical pixels.
pub const ICON_SIZE: f32 = 18.0;
/// Room around an icon on a button that has nothing else on it.
pub const ICON_PADDING: f32 = 7.0;
/// A button with nothing but an icon on it.
pub const ICON_BUTTON_WIDTH: f32 = ICON_SIZE + 2.0 * ICON_PADDING;
/// Between an icon and the words beside it.
pub const ICON_GAP: f32 = 6.0;

macro_rules! icons {
    ($($name:ident = $file:literal,)*) => {
        #[derive(Clone, Copy, Debug, PartialEq)]
        pub enum Icon {
            $($name,)*
        }

        impl Icon {
            /// Read once, then shared: iced keeps what it drew of each by the handle.
            fn handle(self) -> svg::Handle {
                match self {
                    $(Self::$name => {
                        static HANDLE: LazyLock<svg::Handle> = LazyLock::new(|| {
                            let bytes: &'static [u8] =
                                include_bytes!(concat!("../icons/", $file, ".svg"));
                            svg::Handle::from_memory(bytes)
                        });
                        HANDLE.clone()
                    })*
                }
            }
        }
    };
}

icons! {
    AddTrack = "row-insert-bottom",
    Compound = "stack-2",
    Delete = "trash",
    Details = "layout-sidebar-right",
    Effects = "sparkles",
    Export = "file-export",
    FullScreen = "maximize",
    Import = "plus",
    LeaveFullScreen = "minimize",
    Media = "layout-sidebar",
    MediaFile = "movie",
    Menu = "menu-2",
    More = "dots",
    Mute = "volume",
    Muted = "volume-off",
    Next = "chevron-right",
    Pause = "player-pause-filled",
    Play = "player-play-filled",
    Previous = "chevron-left",
    PopOut = "external-link",
    Project = "folder",
    PutBack = "external-link-off",
    Redo = "arrow-forward-up",
    Select = "select-all",
    Split = "scissors",
    StepInside = "login-2",
    Text = "typography",
    Transitions = "transition-right",
    Undo = "arrow-back-up",
}

/// `icon` in `color`, `size` logical pixels square.
pub fn icon<'a>(icon: Icon, size: f32, color: Color) -> Svg<'a> {
    iced::widget::svg(icon.handle())
        .width(size)
        .height(size)
        .style(move |_, _| svg::Style { color: Some(color) })
}

/// What an icon on a button is drawn in: the words' colour while the button can be used,
/// dimmed while it cannot.
pub fn ink(usable: bool) -> Color {
    if usable { theme::TEXT } else { theme::TEXT_DIM }
}

/// An icon at button size, in `color`, to put on a button.
pub fn glyph<'a, Message: 'a>(which: Icon, color: Color) -> Element<'a, Message> {
    icon(which, ICON_SIZE, color).into()
}
