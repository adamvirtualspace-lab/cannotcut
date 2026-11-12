//! Things that move: the splash a click flashes round a button, the wipe menus open and close
//! with, and the timing everything else that moves goes by (side panels, menus, dialogs).
//! Buttons themselves answer the pointer at once.
//!
//! Nothing here keeps the window drawing once it has arrived: a splash takes two frames, one to
//! show it and one to take it away, and the app watches the frames only while something of its
//! own is moving. With motion turned off under Appearance, everything arrives at once and
//! clicks flash no splash.

use std::time::{Duration, Instant};

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer};
use iced::widget::button::{Status, Style};
use iced::{
    Background, Border, Color, Element, Event, Length, Padding, Point, Rectangle, Size, Theme,
    Vector, touch, window,
};

use crate::theme;

/// How long a docked side panel takes to close or open.
pub const PANEL: Duration = Duration::from_millis(320);
/// How long a side panel takes to slide out over the viewer in a narrow window, or back.
pub const DRAWER: Duration = Duration::from_millis(300);
/// How long a menu takes to be wiped into view.
pub const MENU: Duration = Duration::from_millis(200);
/// How long a dialog takes to rise into place, and the dim behind it to come.
pub const DIALOG: Duration = Duration::from_millis(240);
/// The least share of its full time a move takes when it only goes part of the way, as when a
/// menu is opened again while it was still wiping away.
const LEAST_SHARE: f32 = 0.35;

/// Whether things move, as chosen under Appearance.
pub fn on() -> bool {
    theme::look().motion
}

/// `duration`, or none when motion is off.
fn lasting(duration: Duration) -> Duration {
    if on() { duration } else { Duration::ZERO }
}

/// How far along a move is, `t` of the way through its time (0 to 1): a quintic ease in and
/// out. It leaves and arrives almost still, its ends flat, and makes up for that in the
/// middle, where it goes five times its average speed.
fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        16.0 * t.powi(5)
    } else {
        1.0 - (2.0 - 2.0 * t).powi(5) / 2.0
    }
}

/// A number on its way from one value to another over a while.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tween {
    from: f32,
    to: f32,
    start: Instant,
    duration: Duration,
}

impl Tween {
    /// One standing at `value`.
    pub fn at(value: f32) -> Self {
        Self {
            from: value,
            to: value,
            start: Instant::now(),
            duration: Duration::ZERO,
        }
    }

    /// One on its way from `from` to `to`, starting now.
    pub fn new(from: f32, to: f32, duration: Duration) -> Self {
        Self {
            from,
            to,
            start: Instant::now(),
            duration: lasting(duration),
        }
    }

    /// Sends it on to `to` from wherever it is at `now`. `duration` is for going the whole way
    /// (from 0 to 1, or back); part of the way takes that share of it, but not much less.
    pub fn go(&mut self, to: f32, duration: Duration, now: Instant) {
        if to == self.to {
            return;
        }
        self.from = self.value(now);
        self.to = to;
        self.start = now;
        let share = (to - self.from).abs().clamp(LEAST_SHARE, 1.0);
        self.duration = lasting(duration).mul_f32(share);
    }

    /// Where it is at `now`.
    pub fn value(&self, now: Instant) -> f32 {
        if self.duration.is_zero() {
            return self.to;
        }
        let t = now.saturating_duration_since(self.start).as_secs_f32()
            / self.duration.as_secs_f32();
        self.from + (self.to - self.from) * ease(t)
    }

    /// Whether it is still on its way at `now`.
    pub fn moving(&self, now: Instant) -> bool {
        now < self.start + self.duration
    }
}

/// How long the splash of a click shows: a flash.
pub const SPLASH: Duration = Duration::from_millis(110);
/// How far apart the strokes of a splash are, round the oval they are spread on; and the
/// fewest and most strokes a splash has, whatever the button's size.
const SPLASH_SPACING: f32 = 56.0;
const SPLASH_FEWEST: f32 = 6.0;
const SPLASH_MOST: f32 = 9.0;
/// How much bigger, each way, than the button the oval the strokes are spread round is. The
/// bigger it is, the rounder: a small button gets an even burst all round, a wide one strokes
/// along its length too.
const SPLASH_OVAL: f32 = 20.0;
/// How far out from the button's edge a stroke starts: this much, and up to this much more.
const SPLASH_GAP: f32 = 4.0;
const SPLASH_GAP_SPREAD: f32 = 8.0;
/// A stroke's length (give or take a third), how far it waves to either side, and the length
/// of a wave.
const SPLASH_LENGTH: f32 = 13.0;
const SPLASH_WAVE: f32 = 1.9;
const SPLASH_WAVELENGTH: f32 = 12.0;
/// How thick a stroke is, and the gap between the dots it is drawn with.
const SPLASH_THICKNESS: f32 = 2.0;
const SPLASH_STEP: f32 = 0.6;

/// A button, as iced's own: a style gives how it looks in each [`Status`], and it changes at
/// once as the pointer comes, goes and presses. A click also flashes a white splash of wavy
/// strokes all round it, over everything (unless motion is off).
pub struct Button<'a, Message> {
    content: Element<'a, Message>,
    on_press: Option<Message>,
    width: Length,
    height: Length,
    padding: Padding,
    clip: bool,
    style: Box<dyn Fn(&Theme, Status) -> Style + 'a>,
}

/// As iced's buttons have it.
const DEFAULT_PADDING: Padding = Padding {
    top: 5.0,
    bottom: 5.0,
    right: 10.0,
    left: 10.0,
};

impl<'a, Message> Button<'a, Message> {
    pub fn new(content: impl Into<Element<'a, Message>>) -> Self {
        let content = content.into();
        let size = content.as_widget().size_hint();
        Self {
            content,
            on_press: None,
            width: size.width.fluid(),
            height: size.height.fluid(),
            padding: DEFAULT_PADDING,
            clip: false,
            style: Box::new(theme::normal),
        }
    }

    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.padding = padding.into();
        self
    }

    pub fn on_press(mut self, message: Message) -> Self {
        self.on_press = Some(message);
        self
    }

    pub fn on_press_maybe(mut self, message: Option<Message>) -> Self {
        self.on_press = message;
        self
    }

    pub fn clip(mut self, clip: bool) -> Self {
        self.clip = clip;
        self
    }

    pub fn style(mut self, style: impl Fn(&Theme, Status) -> Style + 'a) -> Self {
        self.style = Box::new(style);
        self
    }
}

/// A splash flashed by a click: when.
#[derive(Clone, Copy, Debug)]
struct Splash {
    start: Instant,
    /// Turns each splash a little differently.
    seed: u32,
}

#[derive(Default)]
struct State {
    pressed: bool,
    /// What it was last drawn as, to tell when that changes.
    status: Option<Status>,
    splash: Option<Splash>,
}

impl<'a, Message: Clone + 'a> Widget<Message, Theme, iced::Renderer> for Button<'a, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::padded(limits, self.width, self.height, self.padding, |limits| {
            self.content
                .as_widget_mut()
                .layout(&mut tree.children[0], renderer, limits)
        })
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                layout.children().next().expect("a button holds its content"),
                renderer,
                operation,
            );
        });
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
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout.children().next().expect("a button holds its content"),
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        if shell.is_event_captured() {
            return;
        }
        let bounds = layout.bounds();
        let state = tree.state.downcast_mut::<State>();
        let now = Instant::now();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. }) => {
                if self.on_press.is_some()
                    && let Some(position) = cursor.position_over(bounds)
                {
                    state.pressed = true;
                    if on() {
                        state.splash = Some(Splash {
                            start: now,
                            seed: now.elapsed().subsec_nanos() ^ (position.x as u32) << 8
                                ^ position.y as u32,
                        });
                        // Shown now, and gone when its time is up: two frames, no more.
                        shell.request_redraw();
                        shell.request_redraw_at(window::RedrawRequest::At(now + SPLASH));
                    }
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerLifted { .. }) => {
                if let Some(message) = &self.on_press
                    && state.pressed
                {
                    state.pressed = false;
                    if cursor.is_over(bounds) {
                        shell.publish(message.clone());
                    }
                    shell.capture_event();
                }
            }
            Event::Touch(touch::Event::FingerLost { .. }) => state.pressed = false,
            Event::Window(window::Event::RedrawRequested(_)) => match state.splash {
                Some(splash) if now - splash.start < SPLASH => {
                    shell.request_redraw_at(window::RedrawRequest::At(splash.start + SPLASH));
                }
                Some(_) => state.splash = None,
                None => {}
            },
            _ => {}
        }

        let status = if self.on_press.is_none() {
            Status::Disabled
        } else if cursor.is_over(bounds) {
            if state.pressed { Status::Pressed } else { Status::Hovered }
        } else {
            Status::Active
        };
        if state.status != Some(status) {
            if state.status.is_some() {
                shell.request_redraw();
            }
            state.status = Some(status);
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        use iced::advanced::Renderer as _;

        let bounds = layout.bounds();
        let state = tree.state.downcast_ref::<State>();
        let status = if self.on_press.is_none() {
            Status::Disabled
        } else {
            state.status.unwrap_or(Status::Active)
        };
        let style = (self.style)(theme, status);
        if style.background.is_some() || style.border.width > 0.0 || style.shadow.color.a > 0.0 {
            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    border: style.border,
                    shadow: style.shadow,
                    snap: style.snap,
                },
                style.background.unwrap_or(Background::Color(Color::TRANSPARENT)),
            );
        }
        let viewport = if self.clip {
            bounds.intersection(viewport).unwrap_or(*viewport)
        } else {
            *viewport
        };
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            &renderer::Style {
                text_color: style.text_color,
            },
            layout.children().next().expect("a button holds its content"),
            cursor,
            &viewport,
        );
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        if self.on_press.is_some() && cursor.is_over(layout.bounds()) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        // A splash goes over everything, so that nothing around the button cuts it off.
        if let Some(splash) = tree.state.downcast_ref::<State>().splash
            && splash.start.elapsed() < SPLASH
        {
            let button = layout.bounds() + translation;
            return Some(overlay::Element::new(Box::new(SplashOverlay { button, splash })));
        }
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout.children().next().expect("a button holds its content"),
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: Clone + 'a> From<Button<'a, Message>> for Element<'a, Message> {
    fn from(button: Button<'a, Message>) -> Self {
        Element::new(button)
    }
}

/// A splash round `button`, drawn over everything. It takes up no room, so it never comes
/// between the pointer and what is under it.
struct SplashOverlay {
    button: Rectangle,
    splash: Splash,
}

impl<Message> overlay::Overlay<Message, Theme, iced::Renderer> for SplashOverlay {
    fn layout(&mut self, _renderer: &iced::Renderer, _bounds: Size) -> layout::Node {
        layout::Node::new(Size::ZERO).move_to(self.button.center())
    }

    fn draw(
        &self,
        renderer: &mut iced::Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
    ) {
        use iced::advanced::Renderer as _;

        if self.splash.start.elapsed() >= SPLASH {
            return;
        }
        let middle = self.button.center();
        let half = Size::new(self.button.width / 2.0, self.button.height / 2.0);
        for (center, radius) in splash_dots(half, self.splash.seed) {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle::new(
                        Point::new(middle.x + center.x - radius, middle.y + center.y - radius),
                        Size::new(2.0 * radius, 2.0 * radius),
                    ),
                    border: Border {
                        radius: radius.into(),
                        ..Border::default()
                    },
                    ..renderer::Quad::default()
                },
                Color::WHITE,
            );
        }
    }
}

/// A number from 0 to 1 that looks random, the same each time for the same `seed` and `index`.
fn scatter(seed: u32, index: u32) -> f32 {
    let mut x = seed ^ index.wrapping_mul(0x9e37_79b9);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    (x & 0xffff) as f32 / 65535.0
}

/// How far from the middle of a box `half` its size its edge is, going `out` (one long).
fn to_edge(half: Size, out: Vector) -> f32 {
    let across = if out.x.abs() > 1e-6 { half.width / out.x.abs() } else { f32::MAX };
    let down = if out.y.abs() > 1e-6 { half.height / out.y.abs() } else { f32::MAX };
    across.min(down)
}

/// `vector` made one long.
fn unit(vector: Vector) -> Vector {
    let length = (vector.x * vector.x + vector.y * vector.y).sqrt().max(1e-6);
    vector * (1.0 / length)
}

/// The dots a splash round a button `half` its size is drawn with: their centres, from the
/// middle of the button, and their radii. Wavy strokes burst out all round, spread evenly
/// round an oval a little bigger than the button, each pointing away from the middle and
/// starting a little way out from the button's edge.
fn splash_dots(half: Size, seed: u32) -> Vec<(Vector, f32)> {
    let radius = SPLASH_THICKNESS / 2.0;
    let oval = Size::new(half.width + SPLASH_OVAL, half.height + SPLASH_OVAL);
    // Round an oval, near enough.
    let around = std::f32::consts::TAU * ((oval.width.powi(2) + oval.height.powi(2)) / 2.0).sqrt();
    let strokes = (around / SPLASH_SPACING).round().clamp(SPLASH_FEWEST, SPLASH_MOST) as u32;
    let mut dots = Vec::new();
    for stroke in 0..strokes {
        let jitter = |n: u32| scatter(seed, stroke * 4 + n);
        // From the top, round.
        let turn = (stroke as f32 + (jitter(0) - 0.5) * 0.6) / strokes as f32;
        let angle = turn * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
        let out = unit(Vector::new(oval.width * angle.cos(), oval.height * angle.sin()));
        let across = Vector::new(-out.y, out.x);
        let inner = to_edge(half, out) + SPLASH_GAP + SPLASH_GAP_SPREAD * jitter(1);
        let length = SPLASH_LENGTH * (0.7 + 0.6 * jitter(2));
        let phase = jitter(3) * std::f32::consts::TAU;
        let mut step = 0.0;
        while step <= length {
            let wave = (step / SPLASH_WAVELENGTH * std::f32::consts::TAU + phase).sin();
            dots.push((out * (inner + step) + across * (wave * SPLASH_WAVE), radius));
            step += SPLASH_STEP;
        }
    }
    dots
}

/// `content`, drawn only as far as `shown` (0 to 1) of the way across it from one edge: a
/// wipe. It is laid out whole; only its drawing is cut. Unless `live`, as while it wipes away,
/// it does not answer the pointer or the keys.
pub fn wipe<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    shown: f32,
    from: Edge,
    live: bool,
) -> Element<'a, Message> {
    Element::new(Wipe {
        content: content.into(),
        shown: shown.clamp(0.0, 1.0),
        from,
        live,
    })
}

/// The edge a wipe comes from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Edge {
    Top,
    Bottom,
}

struct Wipe<'a, Message> {
    content: Element<'a, Message>,
    shown: f32,
    from: Edge,
    live: bool,
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Wipe<'_, Message> {
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
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
        // Going away, it neither answers nor shows the pointer over it.
        let cursor = if self.live { cursor } else { mouse::Cursor::Unavailable };
        if !self.live && !matches!(event, Event::Window(_)) {
            return;
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
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
        use iced::advanced::Renderer as _;

        if self.shown <= 0.0 {
            return;
        }
        let bounds = layout.bounds();
        let height = bounds.height * self.shown;
        let shown = Rectangle {
            y: match self.from {
                Edge::Top => bounds.y,
                Edge::Bottom => bounds.y + bounds.height - height,
            },
            height,
            ..bounds
        };
        let Some(clip) = shown.intersection(viewport) else {
            return;
        };
        let cursor = if self.live { cursor } else { mouse::Cursor::Unavailable };
        renderer.with_layer(clip, |renderer| {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                layout,
                cursor,
                &clip,
            );
        });
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        if !self.live {
            return mouse::Interaction::default();
        }
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tween_eases_from_where_it_is_to_where_it_goes() {
        let start = Instant::now();
        let mut tween = Tween {
            from: 0.0,
            to: 1.0,
            start,
            duration: Duration::from_millis(100),
        };
        assert_eq!(tween.value(start), 0.0);
        // Barely moving at the start and near the end, and halfway at half time.
        let early = tween.value(start + Duration::from_millis(10));
        assert!(early < 0.001, "flat start: {early}");
        let half = tween.value(start + Duration::from_millis(50));
        assert!((half - 0.5).abs() < 1e-6, "{half}");
        let late = tween.value(start + Duration::from_millis(90));
        assert!(late > 0.999, "flat end: {late}");
        assert_eq!(tween.value(start + Duration::from_millis(100)), 1.0);
        assert!(!tween.moving(start + Duration::from_millis(100)));
        // Turned back halfway, it starts back from where it had got to.
        let turned = start + Duration::from_millis(50);
        tween.duration = Duration::from_millis(100);
        let there = tween.value(turned);
        tween.from = there;
        tween.to = 0.0;
        tween.start = turned;
        assert_eq!(tween.value(turned), there);
    }

    #[test]
    fn the_ease_is_even_about_the_middle_and_fastest_there() {
        assert_eq!(ease(0.0), 0.0);
        assert!((ease(0.5) - 0.5).abs() < 1e-6);
        assert_eq!(ease(1.0), 1.0);
        assert!((ease(0.25) + ease(0.75) - 1.0).abs() < 1e-6);
        // Its speed at the middle, five times the average; nearly none at the ends.
        let speed = |t: f32| (ease(t + 0.0005) - ease(t - 0.0005)) / 0.001;
        assert!((speed(0.5) - 5.0).abs() < 0.01, "{}", speed(0.5));
        assert!(speed(0.02) < 0.001 && speed(0.98) < 0.001);
    }

    #[test]
    fn part_of_the_way_takes_part_of_the_time() {
        let start = Instant::now();
        let mut tween = Tween::at(0.5);
        // Halfway lit, then the pointer leaves: back in half the time, if motion is on.
        tween.go(0.0, Duration::from_millis(200), start);
        if on() {
            assert_eq!(tween.duration, Duration::from_millis(100));
        }
    }

    #[test]
    fn a_splash_rings_the_button_just_outside_its_edges() {
        for half in [Size::new(30.0, 12.0), Size::new(100.0, 26.0), Size::new(16.0, 16.0)] {
            let dots = splash_dots(half, 7);
            // Nothing on the button itself (the wave lets a stroke lean in a little).
            let inside = |at: &Vector| {
                at.x.abs() < half.width - SPLASH_WAVE && at.y.abs() < half.height - SPLASH_WAVE
            };
            assert!(!dots.iter().any(|(at, _)| inside(at)), "{half:?}");
            // Not far from it either, and on every side of it.
            let beyond = SPLASH_GAP + SPLASH_GAP_SPREAD + SPLASH_LENGTH * 1.3 + SPLASH_WAVE;
            assert!(dots.iter().all(|(at, _)| {
                at.x.abs() <= half.width + beyond && at.y.abs() <= half.height + beyond
            }));
            for (x, y) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
                let side = if x != 0.0 { half.width } else { half.height };
                assert!(dots.iter().any(|(at, _)| at.x * x + at.y * y > side), "{half:?} {x},{y}");
            }
            // Wide buttons get more strokes, spread along their long sides too.
            let ends = dots.iter().filter(|(at, _)| at.x.abs() > half.width * 0.6).count();
            assert!(ends * 4 >= dots.len() / 2, "{half:?}: {ends} of {}", dots.len());
        }
        // The same splash for the same seed; another for another.
        let half = Size::new(30.0, 12.0);
        assert_eq!(splash_dots(half, 7)[5].0, splash_dots(half, 7)[5].0);
        assert_ne!(splash_dots(half, 7)[5].0, splash_dots(half, 8)[5].0);
    }
}
