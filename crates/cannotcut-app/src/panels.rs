//! The parts of the window, laid out as in most editors: a bar along the top, the media on the
//! left, the viewer in the middle, the selected clip's details on the right, and the timeline
//! along the bottom under its tools. In a wide window the panels are docked (see `dock`): each
//! can be taken up by its title bar and dropped beside or in place of another, and the gaps
//! between them dragged to share the room differently. In a narrow window the side panels slide
//! out over the viewer instead, from buttons in the top bar, and the gap over the timeline can
//! be dragged; double-clicking it puts it back.

use std::cell::Cell;

use super::*;
use crate::dock::Part;
use crate::icons::{ICON_BUTTON_WIDTH, ICON_GAP, ICON_SIZE, Icon, glyph, icon, ink};
use cannotcut_timeline::{Clip, Item};
use iced::widget::{Row, column, pane_grid, row, scrollable, stack};
use iced::{Padding, Point, Rectangle, mouse};

/// The bar along the top of the window.
const TOP_BAR_HEIGHT: f32 = 44.0;
/// Between panels, where the window shows through.
const GUTTER: f32 = 4.0;
/// How wide the side panels start out, and go back to when their divider is double-clicked.
pub(crate) const MEDIA_WIDTH: f32 = 260.0;
pub(crate) const DETAILS_WIDTH: f32 = 300.0;
/// The least a docked panel is made, across or down.
const MIN_PANE: f32 = 120.0;
/// The least of a docked panel's title bar left free beside its tabs, title or tools: where it
/// is taken up by, to move it.
const GRAB_ROOM: f32 = 32.0;
/// The button on a docked panel's title bar that takes it into a window of its own.
const POP_OUT_ICON_SIZE: f32 = 16.0;
const POP_OUT_PADDING: f32 = 4.0;
const POP_OUT_WIDTH: f32 = POP_OUT_ICON_SIZE + 2.0 * POP_OUT_PADDING + 2.0 * BAR_PADDING;
/// The least the timeline leaves the panels above it.
const MIN_UPPER_HEIGHT: f32 = 120.0;
/// The shortest the timeline is dragged to: the ruler and two lanes.
const MIN_TIMELINE_HEIGHT: f32 = RULER_HEIGHT + LANE_GAP + 2.0 * (LANE_HEIGHT + LANE_GAP);
/// How far either side of the gap between two panels the divider in it can be taken hold of.
const DIVIDER_REACH: f32 = 4.0;
/// The line drawn in a divider the pointer is over, or that is being dragged.
const DIVIDER_LINE: f32 = 2.0;
/// Narrower than this, the side panels slide out instead of being docked.
const DOCKED_WIDTH: f32 = 1000.0;
/// The bar of tools above the tracks.
const TOOLS_HEIGHT: f32 = 40.0;
const VIEWER_TITLE_HEIGHT: f32 = 30.0;
const TRANSPORT_HEIGHT: f32 = 44.0;
/// The viewer's title shows only while the panels above the timeline are at least this tall.
const TITLE_ROOM: f32 = 300.0;
const STATUS_HEIGHT: f32 = 20.0;
const TAB_TEXT_SIZE: f32 = 13.0;
const TAB_HEIGHT: f32 = 36.0;
/// Tabs with an icon over their name.
const ICON_TAB_HEIGHT: f32 = 52.0;
const ICON_TAB_TEXT_SIZE: f32 = 12.0;
const ICON_TAB_ICON_SIZE: f32 = 20.0;
const ICON_TAB_PADDING: [f32; 2] = [4.0, 6.0];
const ICON_TAB_SPACING: f32 = 2.0;
/// The icon by each entry in the media list.
const ENTRY_ICON_SIZE: f32 = 20.0;
const PANEL_PADDING: f32 = 12.0;
const BAR_PADDING: f32 = 8.0;
const DETAIL_TEXT_SIZE: f32 = 13.0;
const DETAIL_LABEL_WIDTH: f32 = 84.0;
const SECTION_TEXT_SIZE: f32 = 15.0;
/// Room left beside a slid-out panel, to tap away from it.
const DRAWER_MARGIN: f32 = 48.0;
const MENU_LABEL: &str = "Menu";

/// A divider between panels in a narrow window; docked panels have the dock's own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Divider {
    /// Between the viewer and the timeline.
    Timeline,
}

/// A panel at the side of the viewer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Side {
    Media,
    Details,
}

impl Side {
    fn part(self) -> Part {
        match self {
            Self::Media => Part::Media,
            Self::Details => Part::Details,
        }
    }
}

/// What the media panel shows.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum MediaTab {
    #[default]
    Media,
    Text,
    Transitions,
    Effects,
}

impl MediaTab {
    const ALL: [MediaTab; 4] = [Self::Media, Self::Text, Self::Transitions, Self::Effects];

    fn name(self) -> &'static str {
        match self {
            Self::Media => "Media",
            Self::Text => "Text",
            Self::Transitions => "Transitions",
            Self::Effects => "Effects",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Self::Media => Icon::MediaFile,
            Self::Text => Icon::Text,
            Self::Transitions => Icon::Transitions,
            Self::Effects => Icon::Effects,
        }
    }
}

/// What the details panel shows of the selected clip.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum DetailsTab {
    #[default]
    Video,
    Audio,
    Speed,
}

impl DetailsTab {
    const ALL: [DetailsTab; 3] = [Self::Video, Self::Audio, Self::Speed];

    fn name(self) -> &'static str {
        match self {
            Self::Video => "Video",
            Self::Audio => "Audio",
            Self::Speed => "Speed",
        }
    }
}

/// How tall the parts of the window are, for a window of a given size.
struct Heights {
    timeline: f32,
    /// The timeline with its tools.
    lower: f32,
    /// The panels above it.
    upper: f32,
}

/// The tallest the timeline is in a window `window_height` tall.
fn most_timeline(window_height: f32) -> f32 {
    let fixed = TOP_BAR_HEIGHT + TOOLS_HEIGHT + STATUS_HEIGHT + 3.0 * GUTTER;
    (window_height - fixed - MIN_UPPER_HEIGHT).max(MIN_TIMELINE_HEIGHT)
}

/// The panels under the top bar, and where the tools' menu opens.
struct Middle<'a> {
    panels: Element<'a, Message>,
    tools_menu: Vec<(String, Option<Message>)>,
    /// Puts the tools' menu in place in a layer over the window, from its top or its bottom.
    tools_menu_place: Padding,
    tools_menu_from_bottom: bool,
    /// Layers over the page: the divider of a narrow window.
    dividers: Vec<Element<'a, Message>>,
}

/// `element` with its top left corner at `left`, `top` in a layer over the window.
fn placed<'a>(element: Element<'a, Message>, left: f32, top: f32) -> Element<'a, Message> {
    container(element)
        .padding(iced::Padding {
            top,
            left,
            ..iced::Padding::ZERO
        })
        .width(Fill)
        .height(Fill)
        .into()
}

impl App {
    /// In a narrow window, the timeline takes the share of the window it was dragged to, or
    /// else as its tracks want; either way it leaves the viewer above it some room.
    fn heights(&self, window: Size) -> Heights {
        let wanted = self.timeline_share.map_or_else(
            || self.panel_height(window.height),
            |share| share * window.height,
        );
        let timeline = wanted.clamp(MIN_TIMELINE_HEIGHT, most_timeline(window.height));
        let lower = TOOLS_HEIGHT + timeline;
        let upper = window.height - TOP_BAR_HEIGHT - lower - STATUS_HEIGHT - 3.0 * GUTTER;
        Heights {
            timeline,
            lower,
            upper,
        }
    }

    /// `divider` dragged to `at` in a window of size `window`: the panels either side take
    /// their new sizes, as far as they can.
    pub(super) fn divider_dragged(&mut self, divider: Divider, at: Point, window: Size) {
        // The pointer is in the middle of the gap.
        match divider {
            Divider::Timeline => {
                let bottom = window.height - STATUS_HEIGHT - GUTTER;
                let timeline = bottom - (at.y + GUTTER / 2.0) - TOOLS_HEIGHT;
                let timeline = timeline.clamp(MIN_TIMELINE_HEIGHT, most_timeline(window.height));
                self.timeline_share = Some(timeline / window.height);
            }
        }
    }

    /// A divider that can be dragged, `width` by `height`: a line shows in it while the
    /// pointer is over it, in the accent colour while it is being dragged.
    fn divider(&self, which: Divider, width: f32, height: f32) -> Element<'_, Message> {
        let color = if self.dividing == Some(which) {
            theme::ACCENT
        } else if self.dividing.is_none() && self.divider_hover == Some(which) {
            theme::DIVIDER_HOVER
        } else {
            Color::TRANSPARENT
        };
        let line = space().width(Fill).height(DIVIDER_LINE);
        let line = container(line).style(move |_| container::background(color));
        mouse_area(center(line).width(width).height(height))
            .interaction(mouse::Interaction::ResizingVertically)
            .on_press(Message::DividerPressed(which))
            // Here too, for a tap too quick for the listening that starts with the press.
            .on_release(Message::DividerReleased)
            .on_double_click(Message::DividerReset(which))
            .on_enter(Message::DividerHovered(Some(which)))
            .on_exit(Message::DividerHovered(None))
            .into()
    }

    /// The divider over the gap above the timeline of a narrow window, a layer of its own; a
    /// little wider than the gap, so that it is easier to take hold of.
    fn dividers(&self, window: Size, upper: f32) -> Vec<Element<'_, Message>> {
        let grip = GUTTER + 2.0 * DIVIDER_REACH;
        let upper_top = TOP_BAR_HEIGHT + GUTTER;
        let mut layers = Vec::new();
        let across = self.divider(Divider::Timeline, window.width - 2.0 * GUTTER, grip);
        layers.push(placed(across, GUTTER, upper_top + upper - DIVIDER_REACH));
        if self.dividing.is_some() {
            // Over everything while dragging, so the pointer keeps its look wherever it goes
            // and nothing else answers it meanwhile.
            let look = mouse::Interaction::ResizingVertically;
            layers.push(mouse_area(space().width(Fill).height(Fill)).interaction(look).into());
        }
        layers
    }

    /// Everything, laid out for a window of this size in logical pixels.
    pub(super) fn layout(&self, window: Size) -> Element<'_, Message> {
        let docked = window.width >= DOCKED_WIDTH;
        let room = window.width - 2.0 * GUTTER;
        let top = self.top_bar(room - 2.0 * BAR_PADDING, docked);
        let top = container(top.row)
            .padding([0.0, BAR_PADDING])
            .height(TOP_BAR_HEIGHT)
            .center_y(TOP_BAR_HEIGHT);
        let Middle {
            panels,
            tools_menu,
            tools_menu_place,
            tools_menu_from_bottom,
            dividers,
        } = if docked {
            self.docked(window)
        } else {
            self.stacked(window)
        };
        let page = column![top, panels, self.status_line()].spacing(GUTTER);
        let page = container(page)
            .width(Fill)
            .height(Fill)
            .style(theme::window);
        let mut layers: Vec<Element<'_, Message>> = vec![page.into()];
        layers.extend(dividers);

        // A side panel slid out over the viewer; tapping beside it puts it away.
        if let Some(side) = self.drawer.filter(|side| !docked && !self.is_away(side.part())) {
            let heights = self.heights(window);
            let most = window.width - DRAWER_MARGIN;
            let (panel, width) = match side {
                Side::Media => {
                    let width = self.media_width.min(most);
                    (self.media_panel(width), width)
                }
                Side::Details => (self.details_panel(), self.details_width.min(most)),
            };
            let away = mouse_area(space().width(Fill).height(Fill)).on_press(Message::DrawerClosed);
            layers.push(away.into());
            let placed = container(opaque(panel.width(width).height(heights.upper)))
                .padding(iced::Padding {
                    top: TOP_BAR_HEIGHT + GUTTER,
                    left: GUTTER,
                    right: GUTTER,
                    bottom: 0.0,
                })
                .width(Fill)
                .height(Fill);
            let placed = match side {
                Side::Media => placed.align_left(Fill),
                Side::Details => placed.align_right(Fill),
            };
            layers.push(placed.into());
        }

        // The main menu opens under its button; the tools' menu by their bar.
        let menu = match self.more {
            Some(Bar::Menu) => {
                let place = Padding {
                    top: TOP_BAR_HEIGHT - BAR_PADDING + MENU_GAP,
                    left: GUTTER + BAR_PADDING,
                    ..Padding::ZERO
                };
                Some((self.main_menu(), place, false))
            }
            Some(Bar::Tools) if !tools_menu.is_empty() => {
                Some((tools_menu, tools_menu_place, tools_menu_from_bottom))
            }
            _ => None,
        };
        if let Some((entries, place, from_bottom)) = menu {
            let placed = container(more_menu(entries)).padding(place);
            let placed = if from_bottom {
                placed.align_bottom(Fill)
            } else {
                placed
            };
            let away = mouse_area(space().width(Fill).height(Fill)).on_press(Message::MoreClosed);
            layers.push(away.into());
            layers.push(placed.width(Fill).height(Fill).into());
        }

        // Over everything, so that it fits even a short window, and nothing behind it can be
        // pressed meanwhile; pressing beside it is Cancel.
        let dialog = if let Some(leave) = self.leaving {
            let cancel = (!self.leave_saving).then_some(Message::LeaveCancel);
            Some((self.leave_dialog(leave), cancel))
        } else if self.export_dialog {
            Some((self.export_dialog(), Some(Message::ExportClosed)))
        } else if self.compound_dialog {
            Some((self.compound_dialog(), Some(Message::CompoundClosed)))
        } else if let Some(page) = self.about {
            Some((self.about_dialog(page, window), Some(Message::AboutClosed)))
        } else if let Some(outputs) = &self.sound_outputs {
            let card = self.sound_output_dialog(outputs.as_ref(), window);
            Some((card, Some(Message::SoundOutputClosed)))
        } else {
            None
        };
        if let Some((card, cancel)) = dialog {
            let dim = container(space().width(Fill).height(Fill))
                .style(|_| container::background(DIALOG_BACKDROP));
            let backdrop: Element<'_, Message> = match cancel {
                Some(cancel) => mouse_area(dim).on_press(cancel).into(),
                None => dim.into(),
            };
            layers.push(opaque(backdrop));
            layers.push(center(opaque(card)).into());
        }
        Stack::with_children(layers).into()
    }

    /// The panels docked in a wide window, each with its title bar to take it up by.
    fn docked(&self, window: Size) -> Middle<'_> {
        let area = Size::new(
            window.width - 2.0 * GUTTER,
            window.height - TOP_BAR_HEIGHT - STATUS_HEIGHT - 2.0 * GUTTER,
        );
        let regions = self.dock.layout().pane_regions(GUTTER, MIN_PANE, area);
        let timeline = self
            .dock
            .iter()
            .find(|(_, part)| **part == Part::Timeline)
            .and_then(|(pane, _)| regions.get(pane))
            .copied()
            .unwrap_or_else(|| Rectangle::with_size(area));
        let tools = self.tools(timeline.width - 2.0 * BAR_PADDING - GRAB_ROOM);
        // Taken by the timeline's pane, the one place it goes.
        let tools_row = Cell::new(Some(Element::from(tools.row)));
        let can_leave = self.dock.len() > 1;
        let grid = pane_grid(&self.dock, |pane, part, _| {
            let size = regions.get(&pane).map_or(area, |region| region.size());
            let leaves = can_leave && *part != Part::Timeline;
            let title_room = size.width - GRAB_ROOM - if leaves { POP_OUT_WIDTH } else { 0.0 };
            let (title, body) = match part {
                Part::Media => (self.media_tabs(title_room), self.media_body()),
                Part::Viewer => (
                    self.viewer_title(title_room),
                    self.viewer_body(size.width, None),
                ),
                Part::Details => (self.details_title(), self.details_body()),
                Part::Timeline => {
                    let tools = tools_row.take().unwrap_or_else(|| space().into());
                    let height = (size.height - TOOLS_HEIGHT).max(0.0);
                    (tools_strip(tools), self.timeline_body(height))
                }
            };
            // The title bar is taken up wherever its content is not.
            let mut title = pane_grid::TitleBar::new(title).style(theme::bar);
            if leaves {
                let height = match part {
                    Part::Media => ICON_TAB_HEIGHT,
                    Part::Viewer => VIEWER_TITLE_HEIGHT,
                    _ => TAB_HEIGHT,
                };
                title = title
                    .controls(pane_grid::Controls::new(pop_out(*part, height)))
                    .always_show_controls();
            }
            pane_grid::Content::new(body)
                .title_bar(title)
                .style(theme::panel)
        })
        .spacing(GUTTER)
        .min_size(MIN_PANE)
        .on_drag(Message::PaneDragged)
        .on_resize(DIVIDER_REACH, Message::PaneResized)
        .style(theme::dock);
        // Opening down from the tools, over the tracks, wherever the timeline is docked.
        let menu_x = timeline.x + BAR_PADDING + tools.menu_x;
        let tools_menu_place = Padding {
            top: TOP_BAR_HEIGHT + GUTTER + timeline.y + TOOLS_HEIGHT + MENU_GAP,
            left: GUTTER + menu_x.min(area.width - MENU_WIDTH).max(0.0),
            ..Padding::ZERO
        };
        Middle {
            panels: container(grid).padding([0.0, GUTTER]).height(Fill).into(),
            tools_menu: tools.menu,
            tools_menu_place,
            tools_menu_from_bottom: false,
            dividers: Vec::new(),
        }
    }

    /// The viewer over the timeline in a narrow window: the timeline takes the share of the
    /// window it was dragged to, or the height its tracks want, the viewer the rest.
    fn stacked(&self, window: Size) -> Middle<'_> {
        let room = window.width - 2.0 * GUTTER;
        let tools = self.tools(room - 2.0 * BAR_PADDING);
        if self.is_away(Part::Viewer) {
            // The timeline has the window to itself.
            let height = (window.height - TOP_BAR_HEIGHT - STATUS_HEIGHT - 2.0 * GUTTER
                - TOOLS_HEIGHT)
                .max(0.0);
            let menu_x = BAR_PADDING + tools.menu_x;
            let tools_menu_place = Padding {
                bottom: STATUS_HEIGHT + GUTTER + TOOLS_HEIGHT + height + MENU_GAP,
                left: GUTTER + menu_x.min(room - MENU_WIDTH).max(0.0),
                ..Padding::ZERO
            };
            let lower = container(column![
                across(tools_strip(tools.row.into())),
                self.timeline_body(height),
            ])
            .style(theme::panel);
            return Middle {
                panels: container(lower).padding([0.0, GUTTER]).height(Fill).into(),
                tools_menu: tools.menu,
                tools_menu_place,
                tools_menu_from_bottom: true,
                dividers: Vec::new(),
            };
        }
        let heights = self.heights(window);
        let viewer = self.viewer_panel(room, heights.upper >= TITLE_ROOM);
        let menu_x = BAR_PADDING + tools.menu_x;
        let tools_menu_place = Padding {
            bottom: STATUS_HEIGHT + GUTTER + heights.lower + MENU_GAP,
            left: GUTTER + menu_x.min(room - MENU_WIDTH).max(0.0),
            ..Padding::ZERO
        };
        let lower = container(column![
            across(tools_strip(tools.row.into())),
            self.timeline_body(heights.timeline),
        ])
        .style(theme::panel);
        let panels = column![
            container(viewer).padding([0.0, GUTTER]).height(Fill),
            container(lower).padding([0.0, GUTTER]),
        ]
        .spacing(GUTTER);
        Middle {
            panels: panels.into(),
            tools_menu: tools.menu,
            tools_menu_place,
            tools_menu_from_bottom: true,
            dividers: self.dividers(window, heights.upper),
        }
    }

    /// The tracks under the ruler, `height` tall, with their buttons beside them.
    fn timeline_body(&self, height: f32) -> Element<'_, Message> {
        let timeline = self.history.timeline();
        let shown = self
            .preview
            .as_ref()
            .map_or(timeline, |preview| &preview.timeline);
        let panel = timeline_panel(
            timeline,
            shown,
            self.playhead,
            self.timeline_view,
            &self.selected,
            &self.waveforms,
            Message::Timeline,
        )
        .selecting(self.selecting)
        .height(height);
        row![self.track_headers(height), panel].into()
    }

    /// The bar along the top, fitted to `room`: the menu, where the project is, the side
    /// panels' buttons while they slide out, and Export.
    fn top_bar(&self, room: f32, docked: bool) -> Fitted<'_, Message> {
        let open = self.more == Some(Bar::Menu);
        let ink = if open { theme::ON_ACCENT } else { theme::ON_BUTTON };
        let menu = button(labelled(Icon::Menu, MENU_LABEL, ink))
            .style(if open { theme::on } else { theme::normal })
            .on_press(if open {
                Message::MoreClosed
            } else {
                Message::MoreOpened(Bar::Menu)
            });
        let mut controls = vec![Control::new(menu, labelled_width(MENU_LABEL))];
        // Whatever room the rest leave: the path, and whether there is anything to save.
        let path = responsive(|room| {
            let unsaved = !self.unsaved().is_empty();
            let note = "Not saved";
            let path_room = room.width - if unsaved { text_width(note) + ROW_SPACING } else { 0.0 };
            let mut path = row![self.project_path_bar(path_room)]
                .spacing(ROW_SPACING)
                .align_y(Center);
            if unsaved && path_room >= self.path_least_width() {
                let note = text(note).size(DETAIL_TEXT_SIZE).font(theme::SMALL);
                path = path.push(note.color(theme::TEXT_DIM));
            }
            container(path).clip(true).into()
        })
        .height(iced::Length::Shrink);
        controls.push(Control::new(path, self.path_least_width()));
        if !docked {
            for (side, which, words) in [
                (Side::Media, Icon::Media, "Media"),
                (Side::Details, Icon::Details, "Details"),
            ] {
                if self.is_away(side.part()) {
                    continue;
                }
                let open = self.drawer == Some(side);
                let ink = if open { theme::ON_ACCENT } else { theme::TEXT };
                let toggle = icon_button(which, ink)
                    .style(if open { theme::on } else { theme::quiet })
                    .on_press(Message::DrawerToggled(side));
                controls.push(Control::new(hint_below(toggle, words), ICON_BUTTON_WIDTH));
            }
        }
        controls.push(self.export_control());
        fit(controls, room, ROW_SPACING, nothing(), 0.0, 0)
    }

    /// What the main menu offers.
    fn main_menu(&self) -> Vec<(String, Option<Message>)> {
        vec![
            ("New project".to_string(), Some(Message::NewRequested)),
            ("Open...".to_string(), Some(Message::OpenRequested)),
            ("Save".to_string(), Some(Message::SaveRequested)),
            ("Add clips...".to_string(), Some(Message::AddRequested)),
            ("Reset panel layout".to_string(), Some(Message::DockReset)),
            ("Sound output...".to_string(), Some(Message::SoundOutputOpened)),
            ("About and licences".to_string(), Some(Message::AboutOpened)),
        ]
    }

    /// The picture, over the buttons to play it; `room` wide. `double_click` is what
    /// double-clicking the picture does, if anything.
    fn viewer_body(&self, room: f32, double_click: Option<Message>) -> Element<'_, Message> {
        let timeline = self.history.timeline();
        let picture: Element<'_, Message> = match self.shown.as_ref().map(|shown| &shown.picture) {
            Some(Displayed::Yuv(planes)) => shader(planes).width(Fill).height(Fill).into(),
            Some(Displayed::Image(allocation)) => image(allocation.handle().clone())
                .content_fit(ContentFit::Contain)
                .width(Fill)
                .height(Fill)
                .into(),
            None if timeline.duration() == 0 => center(text("Add a clip to start")).into(),
            None => center(text("")).into(),
        };
        let picture = container(picture)
            .width(Fill)
            .height(Fill)
            .style(|_| container::background(Color::BLACK));
        let mut picture: Element<'_, Message> =
            sensor(picture).on_resize(Message::ViewerResized).into();
        if self.viewer.is_none() {
            picture = stack![picture, renderer_probe(Message::RendererProbed)].into();
        }
        if let Some(double_click) = double_click {
            picture = mouse_area(picture).on_double_click(double_click).into();
        }
        column![picture, self.transport(room - 2.0 * BAR_PADDING)].into()
    }

    /// A panel in a window of its own, `size` big: its title bar with the buttons to fill the
    /// screen and to put it back, over the panel. The viewer over the whole screen is the
    /// picture and its buttons alone.
    pub(super) fn panel_window(
        &self,
        part: Part,
        window: iced::window::Id,
        full_screen: bool,
        size: Size,
    ) -> Element<'_, Message> {
        let screen = Message::FullScreenToggled(window);
        let body = match part {
            Part::Viewer => self.viewer_body(size.width, Some(screen.clone())),
            Part::Media => self.media_body(),
            Part::Details => self.details_body(),
            // Never away: the main window keeps it.
            Part::Timeline => space().into(),
        };
        if full_screen && part == Part::Viewer {
            return container(body)
                .width(Fill)
                .height(Fill)
                .style(theme::panel)
                .into();
        }
        let room = size.width - 2.0 * ICON_BUTTON_WIDTH - 2.0 * BAR_PADDING - ROW_SPACING;
        let title = match part {
            Part::Viewer => self.viewer_title(room),
            Part::Media => self.media_tabs(room),
            Part::Details => self.details_title(),
            Part::Timeline => space().into(),
        };
        let (which, words) = match (full_screen, part) {
            (true, Part::Viewer) => (Icon::LeaveFullScreen, "Leave full screen (F11 or Esc)"),
            (true, _) => (Icon::LeaveFullScreen, "Leave full screen (Esc)"),
            (false, Part::Viewer) => (Icon::FullScreen, "Full screen (F11, or double-click)"),
            (false, _) => (Icon::FullScreen, "Full screen"),
        };
        let buttons = row![
            hint_below(
                icon_button(which, theme::TEXT).style(theme::quiet).on_press(screen),
                words,
            ),
            hint_below(
                icon_button(Icon::PutBack, theme::TEXT)
                    .style(theme::quiet)
                    .on_press(Message::PutBack(window)),
                "Put back in the main window",
            ),
        ]
        .spacing(4)
        .align_y(Center);
        let bar = row![title, space().width(Fill), buttons]
            .align_y(Center)
            .padding(Padding {
                right: BAR_PADDING,
                ..Padding::ZERO
            });
        container(column![across(bar.into()), body])
            .width(Fill)
            .height(Fill)
            .style(theme::panel)
            .into()
    }

    /// The line over the picture on what it shows, at most `room` wide.
    fn viewer_title(&self, room: f32) -> Element<'_, Message> {
        one_line(self.headline())
            .padding([0.0, PANEL_PADDING])
            .max_width(room.max(0.0))
            .height(VIEWER_TITLE_HEIGHT)
            .center_y(VIEWER_TITLE_HEIGHT)
            .into()
    }

    /// The viewer as a panel of its own, `room` wide, with its title or without.
    fn viewer_panel(&self, room: f32, titled: bool) -> Element<'_, Message> {
        let mut panel = column![];
        if titled {
            panel = panel.push(across(self.viewer_title(room)));
        }
        container(panel.push(self.viewer_body(room, None)))
            .width(Fill)
            .height(Fill)
            .style(theme::panel)
            .into()
    }

    /// Where the playhead is, and the buttons to play, in the middle of `room`.
    fn transport(&self, room: f32) -> Element<'_, Message> {
        let timeline = self.history.timeline();
        let playing = self.playback.is_some();
        let has_clips = timeline.duration() > 0;
        let play = if playing { Icon::Pause } else { Icon::Play };
        let play_ink = if has_clips { theme::ON_ACCENT } else { theme::TEXT_DIM };
        let buttons = row![
            hint(
                icon_button(Icon::Previous, theme::TEXT)
                    .style(theme::quiet)
                    .on_press(Message::Stepped(-1)),
                "Previous frame (Left arrow)",
            ),
            hint(
                container(
                    icon_button(play, play_ink)
                        .style(theme::primary)
                        .on_press_maybe(has_clips.then_some(Message::PlayToggled)),
                )
                .padding([0.0, PLAY_MARGIN]),
                "Play or pause (Space)",
            ),
            hint(
                icon_button(Icon::Next, theme::TEXT)
                    .style(theme::quiet)
                    .on_press(Message::Stepped(1)),
                "Next frame (Right arrow)",
            ),
        ]
        .spacing(EXPORT_SPACING)
        .align_y(Center);
        let buttons_width = 3.0 * ICON_BUTTON_WIDTH + 2.0 * PLAY_MARGIN + 2.0 * EXPORT_SPACING;
        // The time on the left, as much of it as fits beside the buttons.
        let side = (room - buttons_width) / 2.0 - ROW_SPACING;
        let position = timecode(self.playhead, timeline.rate());
        let length = format!(" / {}", timecode(timeline.duration(), timeline.rate()));
        let time: Element<'_, Message> = if text_width(&position) + text_width(&length) <= side {
            row![
                text(position).color(theme::ACCENT).wrapping(text::Wrapping::None),
                text(length).color(theme::TEXT_DIM).wrapping(text::Wrapping::None),
            ]
            .into()
        } else {
            hint(
                text(position.clone()).color(theme::ACCENT).wrapping(text::Wrapping::None),
                format!("{position}{length}"),
            )
        };
        row![
            container(time).width(Fill).clip(true),
            buttons,
            space().width(Fill)
        ]
        .spacing(ROW_SPACING)
        .align_y(Center)
        .padding([0.0, BAR_PADDING])
        .height(TRANSPORT_HEIGHT)
        .into()
    }

    /// The panel left of the viewer: the media the project uses, and later text, transitions
    /// and effects to add.
    /// `width` wide: the tabs' names go when there is no room for them.
    fn media_panel(&self, width: f32) -> container::Container<'_, Message> {
        container(column![across(self.media_tabs(width)), self.media_body()])
            .height(Fill)
            .style(theme::panel)
    }

    /// The media panel's tabs, `width` wide.
    fn media_tabs(&self, width: f32) -> Element<'_, Message> {
        icon_tab_row(
            MediaTab::ALL.map(|tab| {
                (tab.icon(), tab.name(), tab == self.media_tab, Message::MediaTabChosen(tab))
            }),
            width,
        )
    }

    /// What the media panel's chosen tab shows.
    fn media_body(&self) -> Element<'_, Message> {
        match self.media_tab {
            MediaTab::Media => self.media_list(),
            MediaTab::Text => coming("Titles and text over the picture come in a later step."),
            MediaTab::Transitions => {
                coming("Cross-fades and wipes between clips come in a later step.")
            }
            MediaTab::Effects => coming("Colour and filters come in a later step."),
        }
    }

    /// The media files and projects the project uses, each once; pressing one selects the
    /// clips that use it.
    fn media_list(&self) -> Element<'_, Message> {
        let timeline = self.history.timeline();
        let mut media: Vec<(&Arc<TimelineMedia>, Vec<ClipId>)> = Vec::new();
        for track in timeline.tracks() {
            for (_, item) in track.placed() {
                let Item::Clip(clip) = item else {
                    continue;
                };
                let same = |seen: &&mut (&Arc<TimelineMedia>, Vec<ClipId>)| {
                    Arc::ptr_eq(seen.0, &clip.media)
                        || (seen.0.target.is_some() && seen.0.target == clip.media.target)
                };
                match media.iter_mut().find(|seen| same(seen)) {
                    Some((_, clips)) => clips.push(clip.id),
                    None => media.push((&clip.media, vec![clip.id])),
                }
            }
        }
        let import = button(labelled(Icon::Import, "Import", theme::ON_ACCENT))
            .style(theme::primary)
            .on_press(Message::AddRequested);
        let entries = media.into_iter().map(|(media, clips)| {
            let (kind, which) = if media.is_inside() {
                ("Compound clip", Icon::Compound)
            } else if media.is_project() {
                ("Project", Icon::Project)
            } else {
                ("Media", Icon::MediaFile)
            };
            let length = media
                .available
                .map(|available| format!(", {}", timecode(available.duration, media.rate)))
                .unwrap_or_default();
            let used = match clips.len() {
                1 => String::new(),
                count => format!(", used {count} times"),
            };
            // Named as in the project, or by its file.
            let name = if media.name.is_empty() {
                media
                    .file(self.project_folder.as_deref())
                    .map_or_else(|| "Missing file".to_string(), |path| file_name(&path))
            } else {
                media.name.clone()
            };
            let entry = column![
                one_line(name),
                container(
                    text(format!("{kind}{length}{used}"))
                        .font(theme::SMALL)
                        .wrapping(text::Wrapping::None),
                )
                .clip(true)
                .style(|_| container::Style {
                    text_color: Some(theme::TEXT_DIM),
                    ..container::Style::default()
                }),
            ]
            .spacing(2);
            let entry = row![icon(which, ENTRY_ICON_SIZE, theme::TEXT_DIM), entry]
                .spacing(BAR_PADDING)
                .align_y(Center);
            button(entry)
                .width(Fill)
                .style(theme::quiet)
                .on_press(Message::Timeline(Action::Select(clips)))
                .into()
        });
        let list = Column::with_children(entries).spacing(2);
        column![
            import,
            scrollable(list).height(Fill),
        ]
        .spacing(PANEL_PADDING)
        .padding(PANEL_PADDING)
        .height(Fill)
        .into()
    }

    /// The panel right of the viewer: the selected clip, the selected clips, or the project.
    fn details_panel(&self) -> container::Container<'_, Message> {
        container(column![across(self.details_title()), self.details_body()])
            .height(Fill)
            .style(theme::panel)
    }

    /// Across the top of the details panel: the selected clip's tabs, or its name.
    fn details_title(&self) -> Element<'_, Message> {
        let timeline = self.history.timeline();
        let one_clip = self.selected.len() == 1
            && self.selected_clip().and_then(|id| timeline.clip(id)).is_some();
        if one_clip {
            tab_row(DetailsTab::ALL.map(|tab| {
                (tab.name(), tab == self.details_tab, Message::DetailsTabChosen(tab))
            }))
        } else {
            let name = text("Details").size(TAB_TEXT_SIZE).font(theme::SMALL);
            container(name.color(theme::TEXT_DIM))
                .padding([0.0, PANEL_PADDING])
                .height(TAB_HEIGHT)
                .center_y(TAB_HEIGHT)
                .into()
        }
    }

    /// What the details panel shows under its title.
    fn details_body(&self) -> Element<'_, Message> {
        let timeline = self.history.timeline();
        let selected = self.selected_clip().and_then(|id| timeline.clip(id));
        match (self.selected.len(), selected) {
            (1, Some((track, clip, start))) => match self.details_tab {
                DetailsTab::Video => self.clip_details(clip, start),
                DetailsTab::Audio => self.clip_sound(track),
                DetailsTab::Speed => {
                    coming("Faster, slower and reversed clips come in a later step.")
                }
            },
            (0, _) => self.project_details(),
            (count, _) => {
                let about = column![
                    section(format!("{count} clips selected")),
                    text("Compound makes them one clip, which can be stepped inside.")
                        .size(DETAIL_TEXT_SIZE).font(theme::SMALL)
                        .color(theme::TEXT_DIM),
                    button(theme::display("Compound")).on_press(Message::CompoundOpened),
                ]
                .spacing(PANEL_PADDING)
                .padding(PANEL_PADDING);
                about.into()
            }
        }
    }

    /// What the selected clip is.
    fn clip_details(&self, clip: &Clip, start: i64) -> Element<'_, Message> {
        let rate = self.history.timeline().rate();
        let source = if clip.media.is_inside() {
            "Kept inside this project".to_string()
        } else {
            clip.media
                .file(self.project_folder.as_deref())
                .map_or_else(|| "Missing".to_string(), |path| file_name(&path))
        };
        let size = clip
            .media
            .file(self.project_folder.as_deref())
            .and_then(|path| self.picture_sizes.get(&file_key(&path)))
            .map(|(width, height)| format!("{width} x {height}"));
        let mut rows = column![
            section(clip.name.clone()),
            detail("File", source),
            detail("Starts at", timecode(start, rate)),
            detail("Length", timecode(clip.duration, rate)),
        ]
        .spacing(8);
        if let Some(size) = size {
            rows = rows.push(detail("Picture", size));
        }
        if clip.media.project.is_some() {
            let inside = Message::StepInside(clip.id);
            rows = rows.push(button(theme::display("Step inside")).on_press(inside));
        }
        let transform = column![
            section("Transform".to_string()),
            text("Scale, position, rotation and opacity come with layered video.")
                .size(DETAIL_TEXT_SIZE).font(theme::SMALL)
                .color(theme::TEXT_DIM),
        ]
        .spacing(8);
        scrollable(column![rows, rule(), transform].spacing(PANEL_PADDING).padding(PANEL_PADDING))
            .height(Fill)
            .into()
    }

    /// The selected clip's sound: its volume, and its track's.
    fn clip_sound(&self, track: usize) -> Element<'_, Message> {
        let muted = self
            .history
            .timeline()
            .tracks()
            .nth(track)
            .is_some_and(|track| track.muted);
        let (which, words) = if muted {
            (Icon::Muted, "Track muted")
        } else {
            (Icon::Mute, "Mute track")
        };
        let ink = if muted { theme::ON_DANGER } else { theme::ON_BUTTON };
        let mute = button(labelled(which, words, ink))
            .style(if muted { theme::danger } else { theme::normal })
            .on_press(Message::ToggleMute(track));
        let mut rows = column![section("Volume".to_string())].spacing(PANEL_PADDING);
        if let Some(volume) = self.volume_row(Fill) {
            rows = rows.push(volume);
        }
        rows = rows.push(rule()).push(section("Track".to_string())).push(mute);
        container(rows).padding(PANEL_PADDING).into()
    }

    /// The project, while no clip is selected.
    fn project_details(&self) -> Element<'_, Message> {
        let timeline = self.history.timeline();
        let rate = timeline.rate();
        let fps = if rate.den == 1 {
            rate.num.to_string()
        } else {
            format!("{:.3}", rate.as_f64())
        };
        let file = self
            .project_path
            .as_deref()
            .map_or_else(|| "Not saved yet".to_string(), file_name);
        column![
            section(project_name(self.project_path.as_deref(), timeline)),
            detail("File", file),
            detail("Frame rate", format!("{fps} fps")),
            detail("Length", timecode(timeline.duration(), rate)),
            detail("Tracks", timeline.tracks().len().to_string()),
            text("Select a clip to see its details.")
                .size(DETAIL_TEXT_SIZE).font(theme::SMALL)
                .color(theme::TEXT_DIM),
        ]
        .spacing(8)
        .padding(PANEL_PADDING)
        .into()
    }

    /// The tools above the tracks, fitted to `room`; those with no room go to a menu.
    fn tools(&self, room: f32) -> Fitted<'_, Message> {
        let tool = |which: Icon, words: &'static str, does: Option<Message>, keep: u8| {
            tool_control(which, words, words, does, keep)
        };
        let mut controls = vec![
            tool(Icon::Undo, "Undo (Ctrl+Z)", self.history.undo_label().map(|_| Message::Undo), 6),
            tool(Icon::Redo, "Redo (Ctrl+Y)", self.history.redo_label().map(|_| Message::Redo), 5),
            tool(Icon::Split, "Split (Ctrl+B)", self.split_edit().map(|_| Message::Split), 7),
            tool(Icon::Delete, "Delete (Del)", self.delete_edit().map(|_| Message::Delete), 7),
        ];
        controls.extend(self.select_control());
        controls.extend(self.step_inside_control());
        controls.extend(self.compound_control());
        controls.push(tool_control(
            Icon::AddTrack,
            "Add track",
            "Add track",
            Some(Message::AddTrack),
            2,
        ));
        let menu_after = controls.len();
        fit(
            controls,
            room,
            ROW_SPACING,
            more_button(Bar::Tools, self.more),
            ICON_BUTTON_WIDTH,
            menu_after,
        )
    }

    /// Along the bottom: what went wrong, if anything, and the readouts.
    fn status_line(&self) -> Element<'_, Message> {
        let notice = self.notice.clone().map(|notice| {
            text(notice)
                .size(STATUS_TEXT_SIZE).font(theme::SMALL)
                .color(theme::DANGER)
                .wrapping(text::Wrapping::None)
        });
        let readout = container(
            text(self.readout())
                .size(STATUS_TEXT_SIZE).font(theme::SMALL)
                .color(theme::TEXT_DIM)
                .wrapping(text::Wrapping::None),
        )
        .clip(true)
        .align_right(Fill);
        row![Element::from(notice), readout]
            .spacing(ROW_SPACING)
            .padding([0.0, BAR_PADDING])
            .height(STATUS_HEIGHT)
            .align_y(Center)
            .into()
    }
}

/// The `tools` above the tracks, as tall as their bar.
fn tools_strip(tools: Element<'_, Message>) -> Element<'_, Message> {
    container(tools)
        .padding([0.0, BAR_PADDING])
        .height(TOOLS_HEIGHT)
        .center_y(TOOLS_HEIGHT)
        .into()
}

/// The button on a docked panel's title bar, `height` tall, that takes `part` into a window
/// of its own.
fn pop_out<'a>(part: Part, height: f32) -> Element<'a, Message> {
    let open = button(icon(Icon::PopOut, POP_OUT_ICON_SIZE, theme::TEXT_DIM))
        .padding(POP_OUT_PADDING)
        .style(theme::quiet)
        .on_press(Message::PopOut(part));
    container(hint_below(open, "Open in a window of its own, to move to another screen"))
        .padding([0.0, BAR_PADDING])
        .center_y(height)
        .into()
}

/// `content` on a bar across the top of a panel.
fn across(content: Element<'_, Message>) -> Element<'_, Message> {
    container(content).width(Fill).style(theme::bar).into()
}

/// A row of tabs across the top of a panel: words, whether chosen, what choosing does.
fn tab_row<'a, const N: usize>(
    tabs: [(&'static str, bool, Message); N],
) -> Element<'a, Message> {
    let tabs = tabs.into_iter().map(|(words, chosen, choose)| {
        button(text(words).size(TAB_TEXT_SIZE).font(theme::SMALL))
            .padding([6, 8])
            .style(theme::tab(chosen))
            .on_press(choose)
            .into()
    });
    container(Row::with_children(tabs).spacing(2).align_y(Center))
        .padding([0.0, 4.0])
        .height(TAB_HEIGHT)
        .center_y(TAB_HEIGHT)
        .into()
}

/// A panel's word on what is not built yet.
fn coming<'a>(words: &'a str) -> Element<'a, Message> {
    container(text(words).size(DETAIL_TEXT_SIZE).font(theme::SMALL).color(theme::TEXT_DIM))
        .padding(PANEL_PADDING)
        .into()
}

/// A heading in a panel.
fn section<'a>(words: String) -> Element<'a, Message> {
    container(text(words).size(SECTION_TEXT_SIZE).wrapping(text::Wrapping::None))
        .clip(true)
        .into()
}

/// A line of the details panel: what, and its value.
fn detail<'a>(label: &'a str, value: String) -> Element<'a, Message> {
    row![
        text(label)
            .size(DETAIL_TEXT_SIZE).font(theme::SMALL)
            .color(theme::TEXT_DIM)
            .width(DETAIL_LABEL_WIDTH),
        one_line(value),
    ]
    .spacing(8)
    .into()
}

/// A line across a panel between its parts.
fn rule<'a>() -> Element<'a, Message> {
    container(space().width(Fill).height(1.0))
        .style(|_| container::background(theme::LINE))
        .into()
}

/// An icon in `ink` and words beside it, to put on a button.
pub(crate) fn labelled<'a>(which: Icon, words: &'a str, ink: Color) -> Element<'a, Message> {
    row![glyph(which, ink), theme::display(words)]
        .spacing(ICON_GAP)
        .align_y(Center)
        .into()
}

/// A button with an icon and these words on it.
pub(crate) fn labelled_width(words: &str) -> f32 {
    ICON_SIZE + ICON_GAP + button_width(words)
}

/// A button with nothing on it but an icon in `ink`.
pub(crate) fn icon_button<'a>(which: Icon, ink: Color) -> button::Button<'a, Message> {
    button(glyph(which, ink)).padding(crate::icons::ICON_PADDING)
}

/// A tool shown as an icon, with `about` shown while the pointer rests on it; when there is
/// no room in its row it goes to the row's menu as `words`, the lowest `keep` first.
pub(crate) fn tool_control<'a>(
    which: Icon,
    about: &'a str,
    words: &'a str,
    does: Option<Message>,
    keep: u8,
) -> Control<'a, Message> {
    let tool = icon_button(which, ink(does.is_some()))
        .style(theme::quiet)
        .on_press_maybe(does.clone());
    Control::new(hint(tool, about), ICON_BUTTON_WIDTH).or_in_menu(keep, words, does)
}

/// A row of tabs with an icon over each name: icon, name, whether chosen, what choosing does.
/// In `width` too narrow for the names, the icons alone, naming themselves under the pointer.
fn icon_tab_row<'a, const N: usize>(
    tabs: [(Icon, &'static str, bool, Message); N],
    width: f32,
) -> Element<'a, Message> {
    let named_width: f32 = tabs
        .iter()
        .map(|(_, words, _, _)| {
            let words = text_width(words) * ICON_TAB_TEXT_SIZE / 16.0;
            words.max(ICON_TAB_ICON_SIZE) + 2.0 * ICON_TAB_PADDING[1]
        })
        .sum::<f32>()
        + ICON_TAB_SPACING * (N.max(1) - 1) as f32
        + 2.0 * 4.0;
    let named = named_width <= width;
    let tabs = tabs.into_iter().map(|(which, words, chosen, choose)| {
        let ink = if chosen { theme::ACCENT } else { theme::TEXT };
        let face: Element<'a, Message> = if named {
            column![
                icon(which, ICON_TAB_ICON_SIZE, ink),
                text(words)
                    .size(ICON_TAB_TEXT_SIZE)
                    .font(theme::SMALL)
                    .wrapping(text::Wrapping::None),
            ]
            .spacing(2)
            .align_x(Center)
            .into()
        } else {
            icon(which, ICON_TAB_ICON_SIZE, ink).into()
        };
        let tab = button(face)
            .padding(ICON_TAB_PADDING)
            .style(theme::tab(chosen))
            .on_press(choose);
        if named { tab.into() } else { hint(tab, words) }
    });
    container(Row::with_children(tabs).spacing(ICON_TAB_SPACING).align_y(Center))
        .padding([0.0, 4.0])
        .height(ICON_TAB_HEIGHT)
        .center_y(ICON_TAB_HEIGHT)
        .clip(true)
        .into()
}
