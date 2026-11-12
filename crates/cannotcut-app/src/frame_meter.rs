//! How smoothly the main window is drawn: a widget a pixel square that draws nothing (iced
//! skips drawing what has no size), drawn last, that notes when each frame was drawn. Noting it changes nothing on screen, so watching does not itself make the
//! window draw again; the status line reads the notes once a second.
//!
//! With `CANNOTCUT_FRAME_LOG` set to a file, every frame is also written there as it is drawn:
//! milliseconds since the one before, how long building the window's widgets took, and how long
//! from the end of building to the end of drawing (laying them out and drawing them). For
//! finding what makes frames late.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use iced::advanced::widget::Tree;
use iced::advanced::{Layout, Widget, layout, renderer};
use iced::{Element, Length, Rectangle, Size, Theme, mouse};

/// How far back frames are counted.
const WINDOW: Duration = Duration::from_secs(1);
/// A frame drawn this long after the one before counts as the start of a new run of drawing,
/// not as a late frame: the window had nothing to draw meanwhile.
const IDLE_GAP: Duration = Duration::from_millis(250);

#[derive(Default)]
struct Notes {
    /// When frames were drawn, the newest last; none older than `WINDOW`.
    drawn: VecDeque<Instant>,
    /// How long the last build of the window's widgets took, and when it ended.
    built: Option<(Duration, Instant)>,
    log: Option<BufWriter<File>>,
}

static NOTES: LazyLock<Mutex<Notes>> = LazyLock::new(|| {
    let log = std::env::var_os("CANNOTCUT_FRAME_LOG")
        .and_then(|path| File::create(path).ok())
        .map(BufWriter::new);
    Mutex::new(Notes {
        log,
        ..Notes::default()
    })
});

/// How the window was drawn over the last second.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Smoothness {
    /// Frames drawn per second, while drawing.
    pub fps: f64,
    /// The longest wait between two frames of a run of drawing.
    pub longest_gap: Duration,
}

/// Notes how long building the window's widgets took, `started` being when it began.
pub fn built(started: Instant) {
    if let Ok(mut notes) = NOTES.lock() {
        notes.built = Some((started.elapsed(), Instant::now()));
    }
}

/// How smoothly the window was drawn over the last second; none if it drew too little to say,
/// which is the case while nothing on it changes.
pub fn smoothness() -> Option<Smoothness> {
    let mut notes = NOTES.lock().ok()?;
    if let Some(log) = &mut notes.log {
        let _ = log.flush();
    }
    let now = Instant::now();
    while notes.drawn.front().is_some_and(|drawn| now - *drawn > WINDOW) {
        notes.drawn.pop_front();
    }
    measure(notes.drawn.iter().copied())
}

/// How smooth frames drawn at `drawn` (oldest first) were: the rate across runs of drawing,
/// leaving out the pauses between them.
fn measure(drawn: impl IntoIterator<Item = Instant>) -> Option<Smoothness> {
    let mut previous: Option<Instant> = None;
    let mut drawing = Duration::ZERO;
    let mut gaps = 0u32;
    let mut longest_gap = Duration::ZERO;
    for at in drawn {
        if let Some(previous) = previous {
            let gap = at - previous;
            if gap < IDLE_GAP {
                drawing += gap;
                gaps += 1;
                longest_gap = longest_gap.max(gap);
            }
        }
        previous = Some(at);
    }
    // A few frames in a row at the least, for a rate worth showing.
    (gaps >= 5).then(|| Smoothness {
        fps: f64::from(gaps) / drawing.as_secs_f64(),
        longest_gap,
    })
}

/// The meter itself, to put in the window's widgets where it is drawn after everything else.
pub fn frame_meter<'a, Message: 'a>() -> Element<'a, Message> {
    Element::new(Meter)
}

struct Meter;

impl<Message> Widget<Message, Theme, iced::Renderer> for Meter {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(1.0), Length::Fixed(1.0))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &iced::Renderer,
        _limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(Size::new(1.0, 1.0))
    }

    fn draw(
        &self,
        _tree: &Tree,
        _renderer: &mut iced::Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let now = Instant::now();
        let Ok(mut notes) = NOTES.lock() else {
            return;
        };
        let since = notes.drawn.back().map(|previous| now - *previous);
        let built = notes.built.take();
        notes.drawn.push_back(now);
        if notes.drawn.len() > 1000 {
            notes.drawn.pop_front();
        }
        if let Some(log) = &mut notes.log {
            let ms = |duration: Option<Duration>| {
                duration.map_or(String::from("-"), |d| format!("{:.2}", d.as_secs_f64() * 1e3))
            };
            let laid_out = built.map(|(_, ended)| now - ended);
            let _ = writeln!(log, "{} {} {}", ms(since), ms(built.map(|b| b.0)), ms(laid_out));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, milliseconds: &[u64]) -> Vec<Instant> {
        milliseconds.iter().map(|ms| start + Duration::from_millis(*ms)).collect()
    }

    #[test]
    fn steady_frames_give_their_rate_and_gap() {
        let start = Instant::now();
        let frames: Vec<u64> = (0..=60).map(|frame| frame * 1000 / 60).collect();
        let smoothness = measure(at(start, &frames)).expect("plenty of frames");
        assert!((smoothness.fps - 60.0).abs() < 1.0, "{}", smoothness.fps);
        assert_eq!(smoothness.longest_gap, Duration::from_millis(17));
    }

    #[test]
    fn a_pause_between_runs_is_not_a_late_frame() {
        let start = Instant::now();
        let smoothness = measure(at(start, &[0, 16, 33, 50, 600, 616, 633, 650])).unwrap();
        assert_eq!(smoothness.longest_gap, Duration::from_millis(17));
        assert!(smoothness.fps > 55.0);
    }

    #[test]
    fn a_few_lone_frames_say_nothing() {
        assert!(measure(at(Instant::now(), &[0, 16, 400, 900])).is_none());
    }
}
