//! The timeline as the user sees it: a ruler, a lane per track with its clips, and the playhead.
//! Clicking or dragging anywhere moves the playhead; the wheel scrolls, and zooms with Ctrl.

use cannotcut_timeline::{Item, Rate, Timeline, TrackKind};
use iced::advanced::widget::{Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, renderer, text};
use iced::advanced::{Renderer as _, text::Renderer as _};
use iced::{
    Color, Element, Event, Length, Pixels, Point, Rectangle, Size, Theme, alignment, border,
    keyboard, mouse, touch, window,
};

/// The panel never changes height: the viewer above it must not be squeezed by what it shows.
const PANEL_HEIGHT: f32 = 176.0;
const RULER_HEIGHT: f32 = 24.0;
const LANE_HEIGHT: f32 = 40.0;
const LANE_GAP: f32 = 4.0;
/// Room left of frame zero and right of the last frame when everything is shown.
const MARGIN: f32 = 12.0;
/// How much of an empty timeline is shown.
const EMPTY_SECONDS: f32 = 10.0;
/// Zooming in stops when a frame is this wide.
const MAX_SCALE: f32 = 40.0;
/// One notch of the wheel zooms by this factor.
const ZOOM_STEP: f32 = 1.25;
/// Pixels scrolled per line of the wheel.
const WHEEL_LINE: f32 = 60.0;
/// Labelled ticks on the ruler are at least this far apart.
const MIN_LABEL_GAP: f32 = 90.0;
/// Clips narrower than this get no name.
const MIN_LABELLED_WIDTH: f32 = 28.0;
const LABEL_SIZE: f32 = 12.0;
const PLAYHEAD_WIDTH: f32 = 2.0;
const PLAYHEAD_HANDLE: f32 = 10.0;

/// Which part of the timeline the panel shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// Logical pixels per frame.
    pub scale: f32,
    /// The frame at the panel's left edge. Below zero, there is room before the first frame.
    pub start: f32,
}

impl View {
    /// All of a timeline `duration` frames long, in a panel `width` wide.
    fn fit(duration: i64, rate: Rate, width: f32) -> Self {
        let frames = (duration as f32).max(rate.as_f64() as f32 * EMPTY_SECONDS);
        let scale = (width - 2.0 * MARGIN).max(1.0) / frames;
        Self {
            scale,
            start: -MARGIN / scale,
        }
    }

    /// Distance of a frame from the panel's left edge.
    fn x(self, frame: f32) -> f32 {
        (frame - self.start) * self.scale
    }

    fn frame(self, x: f32) -> f32 {
        self.start + x / self.scale
    }

    /// Zooms by `factor` while the frame under `x` stays where it is.
    fn zoomed(self, factor: f32, x: f32) -> Self {
        let scale = (self.scale * factor).min(MAX_SCALE);
        Self {
            scale,
            start: self.frame(x) - x / scale,
        }
    }

    /// Keeps the timeline from being scrolled out of sight: its start stays at the left margin
    /// or beyond, its end in the right half or beyond.
    fn clamped(self, duration: i64, width: f32) -> Self {
        let earliest = -MARGIN / self.scale;
        let latest = (duration as f32 - width / 2.0 / self.scale).max(earliest);
        Self {
            scale: self.scale,
            start: self.start.clamp(earliest, latest),
        }
    }
}

pub struct TimelinePanel<'a, Message> {
    timeline: &'a Timeline,
    playhead: i64,
    view: Option<View>,
    on_scrub: Box<dyn Fn(i64) -> Message + 'a>,
    on_view: Box<dyn Fn(Option<View>) -> Message + 'a>,
}

/// A panel showing `timeline` with the playhead on frame `playhead`. With `view` None the whole
/// timeline is shown. `on_scrub` gets the frame the user moves the playhead to, `on_view` the
/// view they zoom or scroll to.
pub fn timeline_panel<'a, Message>(
    timeline: &'a Timeline,
    playhead: i64,
    view: Option<View>,
    on_scrub: impl Fn(i64) -> Message + 'a,
    on_view: impl Fn(Option<View>) -> Message + 'a,
) -> TimelinePanel<'a, Message> {
    TimelinePanel {
        timeline,
        playhead,
        view,
        on_scrub: Box::new(on_scrub),
        on_view: Box::new(on_view),
    }
}

#[derive(Default)]
struct State {
    scrubbing: bool,
    modifiers: keyboard::Modifiers,
    /// Where the playhead was when the panel last made sure it is in sight.
    followed: Option<i64>,
}

impl<Message> TimelinePanel<'_, Message> {
    fn fit(&self, width: f32) -> View {
        View::fit(self.timeline.duration(), self.timeline.rate(), width)
    }

    fn view(&self, width: f32) -> View {
        self.view.unwrap_or_else(|| self.fit(width))
    }

    fn last_frame(&self) -> i64 {
        (self.timeline.duration() - 1).max(0)
    }

    /// Moves the playhead to the frame `x` pixels from the panel's left edge.
    fn scrub(&self, x: f32, width: f32, shell: &mut Shell<'_, Message>) {
        let frame = self.view(width).frame(x).floor() as i64;
        let frame = frame.clamp(0, self.last_frame());
        if frame != self.playhead {
            shell.publish((self.on_scrub)(frame));
        }
    }

    fn scroll_or_zoom(
        &self,
        delta: mouse::ScrollDelta,
        x: f32,
        width: f32,
        zoom: bool,
        shell: &mut Shell<'_, Message>,
    ) {
        let (across, along) = match delta {
            mouse::ScrollDelta::Lines { x, y } => (x * WHEEL_LINE, y * WHEEL_LINE),
            mouse::ScrollDelta::Pixels { x, y } => (x, y),
        };
        let (fit, view) = (self.fit(width), self.view(width));
        let duration = self.timeline.duration();
        let changed = if zoom {
            let zoomed = view.zoomed(ZOOM_STEP.powf(along / WHEEL_LINE), x);
            // Zooming out ends where everything is in sight.
            (zoomed.scale > fit.scale).then(|| zoomed.clamped(duration, width))
        } else if self.view.is_some() {
            Some(
                View {
                    start: view.start - (across + along) / view.scale,
                    ..view
                }
                .clamped(duration, width),
            )
        } else {
            // Everything is in sight already; there is nowhere to scroll to.
            None
        };
        if changed != self.view {
            shell.publish((self.on_view)(changed));
        }
    }
}

impl<Message> Widget<Message, Theme, iced::Renderer> for TimelinePanel<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fixed(PANEL_HEIGHT))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::atomic(limits, Length::Fill, PANEL_HEIGHT)
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let state = tree.state.downcast_mut::<State>();
        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                state.modifiers = *modifiers;
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(position) = cursor.position_over(bounds) {
                    state.scrubbing = true;
                    self.scrub(position.x - bounds.x, bounds.width, shell);
                    shell.capture_event();
                }
            }
            Event::Touch(touch::Event::FingerPressed { position, .. })
                if bounds.contains(*position) =>
            {
                state.scrubbing = true;
                self.scrub(position.x - bounds.x, bounds.width, shell);
                shell.capture_event();
            }
            // A drag that started in the panel goes on outside it.
            Event::Mouse(mouse::Event::CursorMoved { position })
            | Event::Touch(touch::Event::FingerMoved { position, .. })
                if state.scrubbing =>
            {
                self.scrub(position.x - bounds.x, bounds.width, shell);
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(
                touch::Event::FingerLifted { .. } | touch::Event::FingerLost { .. },
            ) => state.scrubbing = false,
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                if let Some(position) = cursor.position_over(bounds) {
                    let zoom = state.modifiers.command();
                    self.scroll_or_zoom(*delta, position.x - bounds.x, bounds.width, zoom, shell);
                    shell.capture_event();
                }
            }
            Event::Window(window::Event::RedrawRequested(_)) => {
                // When the playhead moved out of a zoomed-in view, by a key say, the view goes
                // after it. A view the user scrolled away from the playhead is left alone.
                let moved = state.followed.replace(self.playhead) != Some(self.playhead);
                if let Some(view) = self.view.filter(|_| moved && !state.scrubbing) {
                    let x = view.x(self.playhead as f32);
                    if !(0.0..=bounds.width - PLAYHEAD_WIDTH).contains(&x) {
                        let centred = View {
                            start: self.playhead as f32 - bounds.width / 2.0 / view.scale,
                            ..view
                        };
                        let centred = centred.clamped(self.timeline.duration(), bounds.width);
                        shell.publish((self.on_view)(Some(centred)));
                    }
                }
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let palette = theme.extended_palette();
        let view = self.view(bounds.width);
        let rate = self.timeline.rate();
        let ink = palette.background.base.text;
        let faint = Color { a: 0.55, ..ink };

        renderer.with_layer(bounds, |renderer| {
            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    border: border::rounded(6),
                    ..renderer::Quad::default()
                },
                palette.background.weak.color,
            );

            // The ruler: a labelled tick every step, smaller ones in between.
            let step = tick_step(view.scale, rate);
            let parts = if step % 5 == 0 { 5 } else { step.min(2) };
            let minor = step / parts;
            let mut frame = ((view.start / minor as f32).floor() as i64).max(0) * minor;
            while view.x(frame as f32) <= bounds.width {
                let x = (bounds.x + view.x(frame as f32)).round();
                let labelled = frame % step == 0;
                let height = if labelled { RULER_HEIGHT * 0.5 } else { RULER_HEIGHT * 0.25 };
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: Rectangle::new(
                            Point::new(x, bounds.y + RULER_HEIGHT - height),
                            Size::new(1.0, height),
                        ),
                        ..renderer::Quad::default()
                    },
                    faint,
                );
                if labelled {
                    let position = Point::new(x + 4.0, bounds.y + RULER_HEIGHT * 0.4);
                    let label = tick_label(frame, step, rate);
                    fill_label(renderer, label, position, faint, bounds, text::Shaping::Basic);
                }
                frame += minor;
            }

            // The top of the stack is the top lane, as it covers the ones below it.
            for (lane, track) in self.timeline.tracks().rev().enumerate() {
                let y = bounds.y + RULER_HEIGHT + LANE_GAP + lane as f32 * (LANE_HEIGHT + LANE_GAP);
                if y >= bounds.y + bounds.height {
                    break;
                }
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: Rectangle::new(
                            Point::new(bounds.x, y),
                            Size::new(bounds.width, LANE_HEIGHT),
                        ),
                        ..renderer::Quad::default()
                    },
                    palette.background.base.color,
                );
                let pair = match track.kind {
                    TrackKind::Video => palette.primary.base,
                    TrackKind::Audio => palette.success.base,
                };
                for (start, item) in track.placed() {
                    let Item::Clip(clip) = item else {
                        continue;
                    };
                    let left = view.x(start as f32);
                    let right = view.x((start + clip.duration) as f32);
                    if right < 0.0 || left > bounds.width {
                        continue;
                    }
                    // A pixel is left free at the end, so that a cut between two clips shows.
                    let width = (right - left - 1.0).max(1.0);
                    let area = Rectangle::new(
                        Point::new(bounds.x + left, y),
                        Size::new(width, LANE_HEIGHT),
                    );
                    let dimmed = if track.enabled && clip.enabled { 1.0 } else { 0.4 };
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: area,
                            border: border::rounded(4),
                            ..renderer::Quad::default()
                        },
                        Color {
                            a: dimmed,
                            ..pair.color
                        },
                    );
                    if let Some(visible) = area
                        .intersection(&bounds)
                        .filter(|_| width >= MIN_LABELLED_WIDTH)
                    {
                        // The name stays in sight while any of the clip is.
                        let position =
                            Point::new(visible.x + 6.0, y + LANE_HEIGHT / 2.0);
                        let color = Color {
                            a: dimmed,
                            ..pair.text
                        };
                        let (name, shaping) = (clip.name.clone(), text::Shaping::Advanced);
                        fill_label(renderer, name, position, color, visible, shaping);
                    }
                }
            }

            if self.timeline.duration() == 0 {
                let position = Point::new(
                    bounds.x + MARGIN,
                    bounds.y + RULER_HEIGHT + LANE_GAP + LANE_HEIGHT / 2.0,
                );
                let hint = "Clips you add line up here".to_string();
                fill_label(renderer, hint, position, faint, bounds, text::Shaping::Basic);
            }
        });

        // In a layer of its own, or the clips' names would be drawn over it.
        renderer.with_layer(bounds, |renderer| {
            let x = (bounds.x + view.x(self.playhead as f32)).round();
            let mut bar = |left: f32, width: f32, height: f32| {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: Rectangle::new(
                            Point::new(left, bounds.y),
                            Size::new(width, height),
                        ),
                        border: border::rounded(2),
                        ..renderer::Quad::default()
                    },
                    ink,
                );
            };
            bar(x, PLAYHEAD_WIDTH, bounds.height);
            let handle = x - (PLAYHEAD_HANDLE - PLAYHEAD_WIDTH) / 2.0;
            bar(handle, PLAYHEAD_HANDLE, RULER_HEIGHT * 0.5);
        });
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<State>();
        if state.scrubbing {
            mouse::Interaction::Grabbing
        } else if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::None
        }
    }
}

impl<'a, Message: 'a> From<TimelinePanel<'a, Message>> for Element<'a, Message> {
    fn from(panel: TimelinePanel<'a, Message>) -> Self {
        Element::new(panel)
    }
}

/// One line of small text, vertically centred on `position` and cut off at `clip`.
fn fill_label(
    renderer: &mut iced::Renderer,
    content: String,
    position: Point,
    color: Color,
    clip: Rectangle,
    shaping: text::Shaping,
) {
    let label = text::Text {
        content,
        bounds: Size::new(f32::INFINITY, LABEL_SIZE * 2.0),
        size: Pixels(LABEL_SIZE),
        line_height: text::LineHeight::default(),
        font: renderer.default_font(),
        align_x: text::Alignment::Left,
        align_y: alignment::Vertical::Center,
        shaping,
        wrapping: text::Wrapping::None,
    };
    renderer.fill_text(label, position, color, clip);
}

/// Frames in a second of timecode: the rate rounded to a whole number.
pub fn timecode_base(rate: Rate) -> i64 {
    (rate.as_f64().round() as i64).max(1)
}

/// Frames between labelled ticks: the smallest round amount that leaves the labels room.
fn tick_step(scale: f32, rate: Rate) -> i64 {
    const FRAMES: [i64; 4] = [1, 2, 5, 10];
    const SECONDS: [i64; 13] = [1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600];
    let base = timecode_base(rate);
    FRAMES
        .into_iter()
        .filter(|frames| *frames < base)
        .chain(SECONDS.into_iter().map(|seconds| seconds * base))
        .find(|step| *step as f32 * scale >= MIN_LABEL_GAP)
        .unwrap_or(SECONDS[SECONDS.len() - 1] * base)
}

/// The time at `frame` as the ruler writes it; frames only show when ticks are closer than a
/// second, hours only once there are any.
fn tick_label(frame: i64, step: i64, rate: Rate) -> String {
    let base = timecode_base(rate);
    let seconds = frame / base;
    let mut label = if seconds >= 3600 {
        format!("{}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60)
    } else {
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    };
    if step < base {
        label.push_str(&format!(":{:02}", frame % base));
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: Rate = Rate::new(30, 1);

    #[test]
    fn fitting_shows_the_whole_timeline_between_the_margins() {
        let view = View::fit(600, RATE, 1024.0);
        assert!((view.x(0.0) - MARGIN).abs() < 0.01);
        assert!((view.x(600.0) - (1024.0 - MARGIN)).abs() < 0.01);
        // An empty timeline shows ten seconds.
        let empty = View::fit(0, RATE, 1024.0);
        assert!((empty.x(300.0) - (1024.0 - MARGIN)).abs() < 0.01);
    }

    #[test]
    fn zooming_keeps_the_frame_under_the_cursor() {
        let view = View::fit(600, RATE, 1024.0);
        let under_cursor = view.frame(400.0);
        let zoomed = view.zoomed(ZOOM_STEP, 400.0);
        assert!(zoomed.scale > view.scale);
        assert!((zoomed.frame(400.0) - under_cursor).abs() < 0.01);
        assert_eq!(view.zoomed(1e9, 400.0).scale, MAX_SCALE);
    }

    #[test]
    fn the_timeline_cannot_be_scrolled_out_of_sight() {
        let view = View {
            scale: 4.0,
            start: 0.0,
        };
        let before = View { start: -500.0, ..view }.clamped(600, 1000.0);
        assert_eq!(before.x(0.0), MARGIN);
        let after = View { start: 5000.0, ..view }.clamped(600, 1000.0);
        assert_eq!(after.x(600.0), 500.0);
    }

    #[test]
    fn ruler_ticks_are_round_and_readable() {
        // Far out: minutes. Close in: single frames.
        assert_eq!(tick_step(0.05, RATE), 60 * 30);
        assert_eq!(tick_step(3.0, RATE), 30);
        assert_eq!(tick_step(40.0, RATE), 5);
        assert_eq!(tick_step(1e-9, RATE), 3600 * 30);
        assert_eq!(tick_label(30 * 75, 30, RATE), "01:15");
        assert_eq!(tick_label(30 * 75 + 7, 5, RATE), "01:15:07");
        assert_eq!(tick_label(30 * 3725, 1800, RATE), "1:02:05");
        // 29.97 counts thirty frames to the second, as timecode does.
        assert_eq!(timecode_base(Rate::new(30000, 1001)), 30);
    }
}
