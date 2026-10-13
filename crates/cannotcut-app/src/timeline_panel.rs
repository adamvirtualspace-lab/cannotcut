//! The timeline as the user sees and edits it: a ruler, a lane per track with its clips, and the
//! playhead. Dragging in the ruler or on empty space moves the playhead; clips are picked up and
//! moved by their body and trimmed by their edges; the wheel scrolls, and zooms with Ctrl.

use cannotcut_timeline::{
    ClipId, Edge, Edit, Item, Placement, Rate, Timeline, Track, TrackKind,
};
use iced::advanced::widget::{Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, renderer, text};
use iced::advanced::{Renderer as _, text::Renderer as _};
use iced::keyboard::{self, key};
use iced::{
    Border, Color, Element, Event, Length, Pixels, Point, Rectangle, Size, Theme, alignment,
    border, mouse, touch, window,
};

/// The track whose clips always sit back to back: taking one out closes the hole, and clips
/// dropped on it go in between the others.
pub const MAIN_TRACK: usize = 0;
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
/// The strip at each end of a clip that trims it instead of moving it.
const EDGE_GRIP: f32 = 7.0;
/// A press on a clip becomes a drag once the pointer has moved this far.
const DRAG_THRESHOLD: f32 = 4.0;
/// An edge being dragged jumps to a nearby edge or the playhead within this distance.
const SNAP_DISTANCE: f32 = 8.0;

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

/// What the user did in the panel.
#[derive(Clone, Debug)]
pub enum Action {
    /// Moved the playhead to this frame.
    Scrub(i64),
    /// Zoomed or scrolled; None shows everything.
    View(Option<View>),
    Select(Option<ClipId>),
    /// Is dragging towards this edit: show it without making it.
    Preview(Edit),
    /// Let go: make the edit.
    Commit(Edit),
    /// The drag came back to where nothing changes, or was given up.
    Cancel,
}

pub struct TimelinePanel<'a, Message> {
    /// The timeline as it is. Drags are worked out against it.
    timeline: &'a Timeline,
    /// What is drawn: the timeline with the edit being dragged already made.
    shown: &'a Timeline,
    playhead: i64,
    view: Option<View>,
    selected: Option<ClipId>,
    on_action: Box<dyn Fn(Action) -> Message + 'a>,
}

/// A panel for `timeline`, drawn as `shown` (the same, or with a previewed edit made), with the
/// playhead on frame `playhead`. With `view` None the whole timeline is in sight.
pub fn timeline_panel<'a, Message>(
    timeline: &'a Timeline,
    shown: &'a Timeline,
    playhead: i64,
    view: Option<View>,
    selected: Option<ClipId>,
    on_action: impl Fn(Action) -> Message + 'a,
) -> TimelinePanel<'a, Message> {
    TimelinePanel {
        timeline,
        shown,
        playhead,
        view,
        selected,
        on_action: Box::new(on_action),
    }
}

#[derive(Default)]
struct State {
    gesture: Gesture,
    /// The edit the drag stands at; made when the pointer lets go.
    pending: Option<Edit>,
    /// The frame the dragged edge has jumped to, shown as a line.
    snapped: Option<i64>,
    modifiers: keyboard::Modifiers,
    /// Where the playhead was when the panel last made sure it is in sight.
    followed: Option<i64>,
}

/// What a press that is still held down is doing.
#[derive(Clone, Copy, Default, PartialEq)]
enum Gesture {
    #[default]
    None,
    Scrub,
    /// On a clip's body. It is a click until the pointer has moved far enough, then a move.
    Clip {
        id: ClipId,
        press: Point,
        /// Frames from the clip's start to where it was grabbed.
        grab: f32,
        moving: bool,
    },
    Trim {
        id: ClipId,
        edge: Edge,
        press_x: f32,
    },
}

/// What is under a point of the panel.
#[derive(Clone, Copy, PartialEq)]
enum Hit {
    /// The ruler, or a lane where no clip is.
    Empty { lane: bool },
    Body { id: ClipId, start: i64 },
    Grip { id: ClipId, edge: Edge },
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

    fn publish(&self, action: Action, shell: &mut Shell<'_, Message>) {
        shell.publish((self.on_action)(action));
    }

    /// The track drawn in the lane at height `y`; the top of the stack is the top lane.
    fn track_at(&self, y: f32) -> Option<usize> {
        let lane = ((y - RULER_HEIGHT - LANE_GAP / 2.0) / (LANE_HEIGHT + LANE_GAP)).floor();
        let tracks = self.timeline.tracks().len();
        (y >= RULER_HEIGHT && lane >= 0.0 && (lane as usize) < tracks)
            .then(|| tracks - 1 - lane as usize)
    }

    fn hit(&self, view: View, point: Point) -> Hit {
        let Some(track) = self.track_at(point.y) else {
            return Hit::Empty { lane: false };
        };
        let clips = self.timeline.track(track).into_iter().flat_map(|track| track.clips());
        for (start, clip) in clips {
            let (left, right) = (view.x(start as f32), view.x((start + clip.duration) as f32));
            if (left..right).contains(&point.x) {
                // Short clips keep a body to move them by.
                let grip = EDGE_GRIP.min((right - left) / 3.0);
                return if point.x < left + grip {
                    Hit::Grip {
                        id: clip.id,
                        edge: Edge::Start,
                    }
                } else if point.x >= right - grip {
                    Hit::Grip {
                        id: clip.id,
                        edge: Edge::End,
                    }
                } else {
                    Hit::Body { id: clip.id, start }
                };
            }
        }
        Hit::Empty { lane: true }
    }

    /// Moves the playhead to the frame `x` pixels from the panel's left edge.
    fn scrub(&self, x: f32, width: f32, shell: &mut Shell<'_, Message>) {
        let frame = self.view(width).frame(x).floor() as i64;
        let frame = frame.clamp(0, self.last_frame());
        if frame != self.playhead {
            self.publish(Action::Scrub(frame), shell);
        }
    }

    /// The clip edge of `timeline`, or the playhead, near `frame`: what a dragged edge jumps to.
    /// Clips for which `skip` holds (given their track and id) offer no edges.
    fn snap(
        &self,
        view: View,
        timeline: &Timeline,
        frame: i64,
        skip: impl Fn(usize, ClipId) -> bool,
    ) -> Option<i64> {
        let skip = &skip;
        let edges = timeline.tracks().enumerate().flat_map(move |(index, track)| {
            track
                .clips()
                .filter(move |(_, clip)| !skip(index, clip.id))
                .flat_map(|(start, clip)| [start, start + clip.duration])
        });
        edges
            .chain([0, self.playhead])
            .min_by_key(|edge| (edge - frame).abs())
            .filter(|edge| (edge - frame).abs() as f32 * view.scale <= SNAP_DISTANCE)
    }

    /// The trim a drag of `distance` pixels on a clip's edge stands for, and the frame the edge
    /// snapped to. On the main track the clips after it move along.
    fn trim(
        &self,
        view: View,
        id: ClipId,
        edge: Edge,
        distance: f32,
    ) -> (Option<Edit>, Option<i64>) {
        let Some((track, clip, start)) = self.timeline.clip(id) else {
            return (None, None);
        };
        let ripple = track == MAIN_TRACK;
        let Some(range) = self.timeline.trim_range(id, edge, ripple) else {
            return (None, None);
        };
        let mut frames = (distance / view.scale).round() as i64;
        // Where the dragged edge ends up on the timeline. A rippling start stays where it is.
        let edge_at = match edge {
            Edge::Start if ripple => None,
            Edge::Start => Some(start + frames),
            Edge::End => Some(start + clip.duration + frames),
        };
        // With ripple the rest of the track moves too, so its edges are nothing to line up with.
        let skip = |other, clip| clip == id || (ripple && other == track);
        let snapped = edge_at.and_then(|at| {
            let to = self.snap(view, self.timeline, at, skip)?;
            range.contains(&(frames + to - at)).then(|| {
                frames += to - at;
                to
            })
        });
        let frames = frames.clamp(*range.start(), *range.end());
        if frames == 0 {
            return (None, None);
        }
        let edit = Edit::Trim {
            clip: id,
            edge,
            frames,
            ripple,
        };
        (Some(edit), snapped)
    }

    /// The move a clip dragged to `point` stands for, and the frame it snapped to. `grab` is how
    /// far into the clip it is held, in frames.
    fn relocate(
        &self,
        view: View,
        id: ClipId,
        grab: f32,
        point: Point,
    ) -> (Option<Edit>, Option<i64>) {
        let Some((from, clip, start)) = self.timeline.clip(id) else {
            return (None, None);
        };
        let tracks = self.timeline.tracks().len();
        let lane = ((point.y - RULER_HEIGHT - LANE_GAP / 2.0) / (LANE_HEIGHT + LANE_GAP)).floor();
        let to = tracks - 1 - (lane.max(0.0) as usize).min(tracks - 1);
        let mut wanted = (view.frame(point.x) - grab).round() as i64;
        let ripple = from == MAIN_TRACK;

        // Where it can go is judged without it: on the main track the others have closed up.
        let mut rest = self.timeline.clone();
        if rest.apply(&Edit::Remove { clip: id, ripple }).is_err() {
            return (None, None);
        }
        let Some(target) = rest.track(to) else {
            return (None, None);
        };
        let (at, mode, snapped) = if to == MAIN_TRACK {
            // In between the clips, wherever that is nearest.
            let at = target
                .borders()
                .min_by_key(|border| (border - wanted).abs())
                .unwrap_or(0);
            // Going back in between the same two clips is no move, even where a project from
            // elsewhere has a gap that putting the clip in again would close.
            let clips_before = |track: Option<&Track>, frame: i64| {
                let clips = track.into_iter().flat_map(Track::clips);
                clips.filter(|(start, _)| *start < frame).count()
            };
            let before = clips_before(self.timeline.track(MAIN_TRACK), start);
            if from == MAIN_TRACK && clips_before(Some(target), at) == before {
                return (None, None);
            }
            (at, Placement::Insert, None)
        } else {
            // Either end may line up with something.
            let snap = |frame| self.snap(view, &rest, frame, |_, clip| clip == id);
            let mut snapped = snap(wanted);
            match snapped {
                Some(to) => wanted = to,
                None => {
                    snapped = snap(wanted + clip.duration);
                    wanted = snapped.map_or(wanted, |to| to - clip.duration);
                }
            }
            // Clips do not cover each other: one in the way pushes it to the nearest free spot.
            let at = target.free_start(wanted, clip.duration);
            (at, Placement::Overwrite, snapped.filter(|_| at == wanted))
        };
        let edit = Edit::Move {
            clip: id,
            track: to,
            at,
            mode,
            ripple,
        };
        // Dropping it where it came from is no edit.
        let mut moved = self.timeline.clone();
        let changes = moved.apply(&edit).is_ok() && moved != *self.timeline;
        (changes.then_some(edit), snapped.filter(|_| changes))
    }

    /// Takes the drag to `point`: works out the edit it stands for and has it shown.
    fn drag(&self, state: &mut State, point: Point, width: f32, shell: &mut Shell<'_, Message>) {
        let view = self.view(width);
        let (edit, snapped) = match &mut state.gesture {
            Gesture::None => return,
            Gesture::Scrub => return self.scrub(point.x, width, shell),
            Gesture::Clip {
                id,
                press,
                grab,
                moving,
            } => {
                *moving |= press.distance(point) >= DRAG_THRESHOLD;
                if !*moving {
                    return;
                }
                self.relocate(view, *id, *grab, point)
            }
            Gesture::Trim { id, edge, press_x } => self.trim(view, *id, *edge, point.x - *press_x),
        };
        state.snapped = snapped;
        if edit != state.pending {
            state.pending = edit.clone();
            self.publish(edit.map_or(Action::Cancel, Action::Preview), shell);
        }
        shell.request_redraw();
    }

    fn press(&self, state: &mut State, point: Point, width: f32, shell: &mut Shell<'_, Message>) {
        let view = self.view(width);
        let hit = self.hit(view, point);
        let picked = match hit {
            Hit::Body { id, .. } | Hit::Grip { id, .. } => Some(id),
            // A press on the ruler leaves the selection alone; one on empty track clears it.
            Hit::Empty { lane: false } => self.selected,
            Hit::Empty { lane: true } => None,
        };
        if picked != self.selected {
            self.publish(Action::Select(picked), shell);
        }
        state.gesture = match hit {
            Hit::Empty { .. } => {
                self.scrub(point.x, width, shell);
                Gesture::Scrub
            }
            Hit::Body { id, start } => Gesture::Clip {
                id,
                press: point,
                grab: view.frame(point.x) - start as f32,
                moving: false,
            },
            Hit::Grip { id, edge } => Gesture::Trim {
                id,
                edge,
                press_x: point.x,
            },
        };
        shell.capture_event();
    }

    fn release(&self, state: &mut State, width: f32, shell: &mut Shell<'_, Message>) {
        match std::mem::take(&mut state.gesture) {
            Gesture::None | Gesture::Scrub => {}
            // A click on a clip also puts the playhead there.
            Gesture::Clip {
                press,
                moving: false,
                ..
            } => self.scrub(press.x, width, shell),
            Gesture::Clip { .. } | Gesture::Trim { .. } => {
                let ended = state.pending.take().map_or(Action::Cancel, Action::Commit);
                self.publish(ended, shell);
            }
        }
        state.snapped = None;
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
            self.publish(Action::View(changed), shell);
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
        // Positions inside the panel count from its corner.
        let inside = |position: Point| Point::new(position.x - bounds.x, position.y - bounds.y);
        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                state.modifiers = *modifiers;
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(key::Named::Escape),
                ..
            }) if !matches!(state.gesture, Gesture::None | Gesture::Scrub) => {
                // Gives the drag up: everything goes back to where it was.
                *state = State {
                    modifiers: state.modifiers,
                    followed: state.followed,
                    ..State::default()
                };
                self.publish(Action::Cancel, shell);
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(position) = cursor.position_over(bounds) {
                    self.press(state, inside(position), bounds.width, shell);
                }
            }
            Event::Touch(touch::Event::FingerPressed { position, .. })
                if bounds.contains(*position) =>
            {
                self.press(state, inside(*position), bounds.width, shell);
            }
            // A drag that started in the panel goes on outside it.
            Event::Mouse(mouse::Event::CursorMoved { position })
            | Event::Touch(touch::Event::FingerMoved { position, .. })
                if state.gesture != Gesture::None =>
            {
                self.drag(state, inside(*position), bounds.width, shell);
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(
                touch::Event::FingerLifted { .. } | touch::Event::FingerLost { .. },
            ) => self.release(state, bounds.width, shell),
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
                let idle = state.gesture == Gesture::None;
                if let Some(view) = self.view.filter(|_| moved && idle) {
                    let x = view.x(self.playhead as f32);
                    if !(0.0..=bounds.width - PLAYHEAD_WIDTH).contains(&x) {
                        let centred = View {
                            start: self.playhead as f32 - bounds.width / 2.0 / view.scale,
                            ..view
                        };
                        let centred = centred.clamped(self.timeline.duration(), bounds.width);
                        self.publish(Action::View(Some(centred)), shell);
                    }
                }
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let state = tree.state.downcast_ref::<State>();
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
            for (lane, track) in self.shown.tracks().rev().enumerate() {
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
                    let selected = self.selected == Some(clip.id);
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: area,
                            border: Border {
                                color: if selected { ink } else { Color::TRANSPARENT },
                                width: if selected { 2.0 } else { 0.0 },
                                radius: 4.0.into(),
                            },
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
                            Point::new(visible.x + 8.0, y + LANE_HEIGHT / 2.0);
                        let color = Color {
                            a: dimmed,
                            ..pair.text
                        };
                        let (name, shaping) = (clip.name.clone(), text::Shaping::Advanced);
                        fill_label(renderer, name, position, color, visible, shaping);
                    }
                }
            }

            if self.shown.duration() == 0 {
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
            let mut bar = |frame: i64, width: f32, height: f32, color: Color| {
                let x = (bounds.x + view.x(frame as f32)).round() - (width - PLAYHEAD_WIDTH) / 2.0;
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: Rectangle::new(Point::new(x, bounds.y), Size::new(width, height)),
                        border: border::rounded(2),
                        ..renderer::Quad::default()
                    },
                    color,
                );
            };
            // Where a dragged edge has lined up with something.
            if let Some(frame) = state.snapped {
                bar(frame, PLAYHEAD_WIDTH, bounds.height, palette.warning.base.color);
            }
            bar(self.playhead, PLAYHEAD_WIDTH, bounds.height, ink);
            bar(self.playhead, PLAYHEAD_HANDLE, RULER_HEIGHT * 0.5, ink);
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
        let bounds = layout.bounds();
        match tree.state.downcast_ref::<State>().gesture {
            Gesture::Trim { .. } => return mouse::Interaction::ResizingHorizontally,
            Gesture::Clip { moving: true, .. } => return mouse::Interaction::Grabbing,
            Gesture::Scrub | Gesture::Clip { .. } => return mouse::Interaction::Pointer,
            Gesture::None => {}
        }
        let Some(position) = cursor.position_over(bounds) else {
            return mouse::Interaction::None;
        };
        let point = Point::new(position.x - bounds.x, position.y - bounds.y);
        match self.hit(self.view(bounds.width), point) {
            Hit::Grip { .. } => mouse::Interaction::ResizingHorizontally,
            Hit::Body { .. } => mouse::Interaction::Grab,
            Hit::Empty { .. } => mouse::Interaction::Pointer,
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
    use std::path::Path;
    use std::sync::Arc;

    use cannotcut_timeline::{Media, NewClip};

    use super::*;

    const RATE: Rate = Rate::new(30, 1);
    /// One pixel per frame, frame zero at the left edge: pixels and frames read the same.
    const PLAIN: View = View {
        scale: 1.0,
        start: 0.0,
    };

    /// A main track with clips of 100, 200 and 300 frames, and above it a track with a clip of
    /// 50 frames starting at frame 400. The ids are returned in that order.
    fn project() -> (Timeline, [ClipId; 4]) {
        let media = Arc::new(Media::from_path(Path::new("/clips/a.mp4"), RATE, 1000));
        let clip = |source_start, duration| NewClip {
            name: "a".to_string(),
            media: media.clone(),
            source_start,
            duration,
        };
        let mut timeline = Timeline::new("test", RATE);
        let add_track = Edit::AddTrack {
            index: 1,
            kind: TrackKind::Video,
            name: "Overlay".to_string(),
        };
        timeline.apply(&add_track).unwrap();
        let mut place = |track, at, clip, mode| {
            let edit = Edit::Place {
                track,
                at,
                clip,
                mode,
            };
            timeline.apply(&edit).unwrap().unwrap()
        };
        let ids = [
            place(MAIN_TRACK, 0, clip(100, 100), Placement::Insert),
            place(MAIN_TRACK, 100, clip(0, 200), Placement::Insert),
            place(MAIN_TRACK, 300, clip(0, 300), Placement::Insert),
            place(1, 400, clip(0, 50), Placement::Overwrite),
        ];
        (timeline, ids)
    }

    fn panel(timeline: &Timeline, playhead: i64) -> TimelinePanel<'_, ()> {
        timeline_panel(timeline, timeline, playhead, Some(PLAIN), None, |_| ())
    }

    /// The height of the middle of a track's lane.
    fn lane_y(timeline: &Timeline, track: usize) -> f32 {
        let lane = (timeline.tracks().len() - 1 - track) as f32;
        RULER_HEIGHT + LANE_GAP + lane * (LANE_HEIGHT + LANE_GAP) + LANE_HEIGHT / 2.0
    }

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

    #[test]
    fn a_point_is_on_a_clips_body_its_edges_or_nothing() {
        let (timeline, [first, second, _, overlay]) = project();
        let panel = panel(&timeline, 0);
        let (main, upper) = (lane_y(&timeline, MAIN_TRACK), lane_y(&timeline, 1));
        let at = |x, y| panel.hit(PLAIN, Point::new(x, y));
        assert!(at(50.0, main) == Hit::Body { id: first, start: 0 });
        assert!(at(2.0, main) == Hit::Grip { id: first, edge: Edge::Start });
        assert!(at(98.0, main) == Hit::Grip { id: first, edge: Edge::End });
        assert!(at(101.0, main) == Hit::Grip { id: second, edge: Edge::Start });
        assert!(at(420.0, upper) == Hit::Body { id: overlay, start: 400 });
        assert!(at(200.0, upper) == Hit::Empty { lane: true });
        assert!(at(200.0, RULER_HEIGHT / 2.0) == Hit::Empty { lane: false });
        assert!(at(200.0, PANEL_HEIGHT - 2.0) == Hit::Empty { lane: false });
    }

    #[test]
    fn trimming_on_the_main_track_ripples_and_stops_at_the_medias_ends() {
        let (timeline, [first, second, ..]) = project();
        let panel = panel(&timeline, 0);
        let trim = |clip, edge, distance| panel.trim(PLAIN, clip, edge, distance).0;
        assert_eq!(
            trim(second, Edge::End, -40.0),
            Some(Edit::Trim {
                clip: second,
                edge: Edge::End,
                frames: -40,
                ripple: true
            })
        );
        // The first clip starts a hundred frames into its media: its start can go back that far
        // and no further, and forward until one frame is left.
        let frames = |edit| match edit {
            Some(Edit::Trim { frames, .. }) => frames,
            _ => 0,
        };
        assert_eq!(frames(trim(first, Edge::Start, -500.0)), -100);
        assert_eq!(frames(trim(first, Edge::Start, 500.0)), 99);
        // Back where it started is no edit.
        assert_eq!(trim(first, Edge::End, 0.3), None);
    }

    #[test]
    fn a_trimmed_edge_jumps_to_the_playhead_and_to_other_tracks_edges() {
        let (timeline, [.., third, overlay]) = project();
        // The third clip ends at 600; the overlay runs from 400 to 450 and the playhead is at
        // 520.
        let panel = panel(&timeline, 520);
        let (edit, snapped) = panel.trim(PLAIN, third, Edge::End, -75.0);
        assert_eq!(snapped, Some(520));
        assert!(matches!(edit, Some(Edit::Trim { frames: -80, .. })));
        let (edit, snapped) = panel.trim(PLAIN, third, Edge::End, -155.0);
        assert_eq!(snapped, Some(450));
        assert!(matches!(edit, Some(Edit::Trim { frames: -150, .. })));
        // Too far from anything: it goes where it is dragged.
        let (edit, snapped) = panel.trim(PLAIN, third, Edge::End, -30.0);
        assert_eq!(snapped, None);
        assert!(matches!(edit, Some(Edit::Trim { frames: -30, .. })));
        // On a free track the neighbours stay put, and the start snaps too.
        let (edit, snapped) = panel.trim(PLAIN, overlay, Edge::End, 73.0);
        assert_eq!(snapped, Some(520));
        assert!(matches!(edit, Some(Edit::Trim { frames: 70, ripple: false, .. })));
    }

    #[test]
    fn clips_dragged_along_the_main_track_change_places() {
        let (timeline, [first, second, third, _]) = project();
        let panel = panel(&timeline, 0);
        let main = lane_y(&timeline, MAIN_TRACK);
        // Held 10 frames in and dragged so that it would start at 290: with the first clip
        // gone the others close up to 0..200 and 200..500, and 200 is the nearest border.
        let (edit, _) = panel.relocate(PLAIN, first, 10.0, Point::new(300.0, main));
        let Some(edit) = edit else {
            panic!("the drag should move the clip");
        };
        let mut moved = timeline.clone();
        moved.apply(&edit).unwrap();
        let main_track = moved.track(MAIN_TRACK).unwrap();
        let order: Vec<ClipId> = main_track.clips().map(|(_, clip)| clip.id).collect();
        assert_eq!(order, [second, first, third]);
        assert_eq!(moved.duration(), timeline.duration());

        // A small drag leaves it where it is: no edit.
        assert_eq!(panel.relocate(PLAIN, first, 10.0, Point::new(40.0, main)).0, None);
    }

    #[test]
    fn nudging_a_clip_on_the_main_track_leaves_a_gap_before_it_alone() {
        // A project from elsewhere: 100 frames, a gap of 50, then another 100.
        let media = Arc::new(Media::from_path(Path::new("/clips/a.mp4"), RATE, 1000));
        let mut timeline = Timeline::new("test", RATE);
        let mut place = |at| {
            let edit = Edit::Place {
                track: MAIN_TRACK,
                at,
                clip: NewClip {
                    name: "a".to_string(),
                    media: media.clone(),
                    source_start: 0,
                    duration: 100,
                },
                mode: Placement::Overwrite,
            };
            timeline.apply(&edit).unwrap().unwrap()
        };
        let (first, second) = (place(0), place(150));
        let panel = panel(&timeline, 0);
        let main = lane_y(&timeline, MAIN_TRACK);

        // A click that slips a few pixels, or a short drag either way: nothing changes.
        for x in [195.0, 200.0, 207.0, 140.0, 400.0] {
            let (edit, _) = panel.relocate(PLAIN, second, 50.0, Point::new(x, main));
            assert_eq!(edit, None, "dragged to {x}");
        }
        // Dragged in front of the other clip, they change places.
        let (edit, _) = panel.relocate(PLAIN, second, 50.0, Point::new(60.0, main));
        let mut moved = timeline.clone();
        moved.apply(&edit.unwrap()).unwrap();
        let order: Vec<ClipId> = moved
            .track(MAIN_TRACK)
            .unwrap()
            .clips()
            .map(|(_, clip)| clip.id)
            .collect();
        assert_eq!(order, [second, first]);
    }

    #[test]
    fn clips_dragged_to_a_free_track_keep_clear_of_the_clips_there() {
        let (timeline, [first, _, _, overlay]) = project();
        let panel = panel(&timeline, 0);
        let upper = lane_y(&timeline, 1);
        // The first clip, 100 frames, dropped so that it would cover the overlay at 400..450:
        // it lands right before it instead, and the main track closes up.
        let (edit, _) = panel.relocate(PLAIN, first, 0.0, Point::new(360.0, upper));
        let mut moved = timeline.clone();
        moved.apply(&edit.unwrap()).unwrap();
        let (track, _, start) = moved.clip(first).unwrap();
        assert_eq!((track, start), (1, 300));
        assert_eq!(moved.clip(overlay).unwrap().2, 400);
        assert_eq!(moved.track(MAIN_TRACK).unwrap().duration(), 500);

        // Dragged along its own free track, the overlay snaps to the cut at 300 below it.
        let (edit, snapped) = panel.relocate(PLAIN, overlay, 0.0, Point::new(304.0, upper));
        assert_eq!(snapped, Some(300));
        assert!(matches!(edit, Some(Edit::Move { at: 300, ripple: false, .. })));
    }
}
