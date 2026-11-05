//! cannotcut: a timeline of clips that can be rearranged, trimmed, cut and played with their
//! sound, a viewer showing the frame under the playhead, and live latency and memory readouts.

mod settings;
mod timeline_panel;
mod viewer;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cannotcut_audio::{Player, Status as SoundStatus};
use cannotcut_export::{Event as ExportEvent, Export, Quality, Resolution};
use cannotcut_media::{
    ClipInfo, Event, Options, Output, Pixels, Scrubber, Waveform, picture_size,
};
use cannotcut_timeline::{
    ClipId, Edge, Edit, History, Media as TimelineMedia, Nested, NewClip, Placement, Projects,
    Rate, Shot, Timeline, TrackKind,
};
use iced::futures::channel::mpsc;
use iced::keyboard::{self, key};
use iced::futures::channel::oneshot;
use iced::widget::{
    Column, button, center, column, container, image, pick_list, progress_bar, row, sensor,
    shader, slider, stack, text,
};
use iced::{
    Center, Color, ContentFit, Element, Fill, Size, Subscription, Task, Theme, time, window,
};
use iced_runtime::image::{Allocation, allocate};
use settings::Settings;
use timeline_panel::{
    Action, LANE_GAP, LANE_HEIGHT, MAIN_TRACK, PANEL_HEIGHT, RULER_HEIGHT, View, Waveforms,
    timecode_base, timeline_panel,
};
use viewer::{YuvPicture, renderer_probe};

/// Keeps the viewer off the shader path, to try the fallback on a machine with a working GPU.
const CPU_VIEWER_FLAG: &str = "--cpu-viewer";
/// Plays without the sound card, following the system clock.
const NO_SOUND_FLAG: &str = "--no-sound";
/// Milliseconds the sound is heard later than the sound card reports; pictures wait as long.
/// Overrides the delay kept in the settings for the sound output.
const SOUND_DELAY_FLAG: &str = "--sound-delay=";
/// Exports the project to this film as soon as it is open, without asking where.
const EXPORT_FLAG: &str = "--export=";
/// Starts with the playhead on a frame, to look at a spot without touching the mouse.
const PLAYHEAD_FLAG: &str = "--at=";
/// Adds a file to the end of the main track as "Add clips" does; for a project, that puts it in
/// as a clip instead of opening it.
const ADD_FLAG: &str = "--add=";
const PROJECT_EXTENSION: &str = "otio";
const FILM_EXTENSION: &str = "mp4";
const EXPORT_BAR_WIDTH: f32 = 160.0;
const DIALOG_WIDTH: f32 = 380.0;
const DIALOG_LABEL_WIDTH: f32 = 90.0;
const DIALOG_TITLE_SIZE: f32 = 20.0;
const BYTES_PER_MEGABYTE: f64 = 1_000_000.0;
const EXPORT_BAR_HEIGHT: f32 = 8.0;
/// The small print of the status row.
const STATUS_TEXT_SIZE: f32 = 13.0;
const MEMORY_REFRESH: Duration = Duration::from_secs(1);
/// How long the viewer's size has to hold still before pictures are made for it. While a window
/// edge is being dragged the picture on screen is stretched instead, which costs nothing.
const RESIZE_SETTLE: Duration = Duration::from_millis(150);
const RESIZE_POLL: Duration = Duration::from_millis(50);
const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "mkv", "webm", "m4v", "avi", "ts"];
const BYTES_PER_MB: usize = 1024 * 1024;
/// Media files kept open at once. Each holds a decoder and its cached pictures, so going back
/// and forth across a cut stays quick without every file of the project staying in memory.
const MAX_OPEN_SOURCES: usize = 3;
const SOURCE_CACHE_BYTES: usize = 96 * BYTES_PER_MB;
/// How long before playback reaches a cut the decoder for the clip after it is got ready.
const WARM_UP_SECONDS: f64 = 2.0;
/// One decoder carries on across a cut when the frames either side of it are this close in the
/// same file, as after a clip was split.
const CARRY_ON_FRAMES: u64 = 2;
/// While playing, frames are asked for this long before they are due, so that each is on hand
/// when its moment comes and goes on screen then, neither early nor late.
const LEAD_SECONDS: f64 = 0.06;
/// How many requests made while playing are remembered, to tell which frame a picture is of.
const REMEMBERED_REQUESTS: usize = 16;
/// How strongly each display tick's time pulls the steady tick times towards it.
const TICK_PULL: f64 = 0.05;
/// Tick intervals the display's pace is first worked out from. A single one can be far out: a
/// tick can come early, or the app can miss one.
const PACE_SAMPLES: usize = 8;
/// A frame start this close to a display tick, as a share of a frame (about a millisecond at
/// 30 fps), could fall either side of it from one tick to the next.
const NEAR_TICK: f64 = 0.03;
/// How far such a frame start is moved away from the tick, as a share of a frame.
const TICK_NUDGE: f64 = 0.1;
/// How long playing waits for the sound card to start before following the system clock.
const SOUND_START_LIMIT: Duration = Duration::from_secs(1);
/// Wide enough for either word, so the buttons after it stay put.
const PLAY_LABEL_WIDTH: f32 = 44.0;
/// Peaks a second in a waveform: enough to see words and beats.
const WAVEFORM_RATE: u32 = 100;
/// The loudest a clip can be turned up to: twice as strong as recorded.
const MAX_VOLUME: f32 = 2.0;
const VOLUME_SLIDER_WIDTH: f32 = 140.0;
/// Wide enough for "200%", so the slider beside it stays put.
const VOLUME_LABEL_WIDTH: f32 = 44.0;
/// The column of track buttons left of the timeline panel.
const HEADER_WIDTH: f32 = 72.0;
const HEADER_TEXT_SIZE: f32 = 12.0;
/// The rate of a project until its first clip sets it.
const EMPTY_PROJECT_RATE: Rate = Rate::new(30, 1);

fn main() -> iced::Result {
    iced::application(App::boot, App::update, App::view)
        .title("cannotcut")
        .subscription(App::subscription)
        .theme(Theme::Dark)
        // Maximized, as an editor wants every pixel; the size is what "restore" goes back to,
        // and fits a laptop screen.
        .window(window::Settings {
            size: Size::new(1280.0, 720.0),
            maximized: true,
            min_size: Some(Size::new(640.0, 480.0)),
            ..window::Settings::default()
        })
        .run()
}

#[derive(Clone)]
enum Message {
    AddRequested,
    FilesChosen(Vec<PathBuf>),
    OpenRequested,
    ProjectChosen(Option<PathBuf>),
    ProjectLoaded(Result<(PathBuf, Timeline), String>),
    SaveRequested,
    SaveChosen(Option<PathBuf>),
    /// Show the export choices.
    ExportOpened,
    ExportClosed,
    ExportResolution(Resolution),
    ExportQuality(Quality),
    /// The choices are made: ask where the film goes.
    ExportRequested,
    ExportChosen(Option<PathBuf>),
    /// A media file's picture size has been read, or could not be.
    PictureSized(String, Option<(u32, u32)>),
    /// News from the export under way.
    Exported(ExportEvent),
    ExportCancelled,
    Saved(Result<PathBuf, String>),
    /// Whether the window is drawn on the GPU, which decides the viewer's path.
    RendererProbed(bool),
    /// The viewer's area was laid out at this size, in logical pixels.
    ViewerResized(Size),
    /// Physical pixels per logical pixel in the window.
    Rescaled(f32),
    ResizeTick,
    /// An event from the decode thread of the source with this generation.
    Media(u64, Media),
    /// A picture finished uploading to the GPU (or failed to) and can be shown.
    Uploaded {
        generation: u64,
        order: u64,
        picture: PictureInfo,
        allocation: Option<Allocation>,
    },
    /// Something the user did in the timeline panel.
    Timeline(Action),
    Stepped(i64),
    PlayToggled,
    /// A new display frame while playing: the playhead goes where the clock says.
    PlayTick,
    Undo,
    Redo,
    /// Cut a clip in two at the playhead.
    Split,
    /// Take the selected clip off the timeline.
    Delete,
    AddTrack,
    /// Silence a track, or let it be heard again.
    ToggleMute(usize),
    /// The volume slider of the selected clip is being dragged.
    VolumeChanged(f32),
    /// The volume slider was let go: the volume it shows is set.
    VolumeReleased,
    /// A media file's waveform has been read, or could not be.
    WaveformRead(String, Option<Arc<Waveform>>),
    /// A project chosen to go on the timeline as a clip has been loaded, or could not be.
    NestedLoaded(PathBuf, Result<Arc<Nested>, String>),
    /// Open the project a clip plays, to edit it in place of the one it sits in.
    StepInside(ClipId),
    /// Go back to the project at this place on the path, outermost first.
    StepOut(usize),
    /// Back in a project after stepping out of one or more: the projects left that had changes
    /// are saved (named here), and those it uses are loaded afresh. Tagged with the step it
    /// belongs to.
    SteppedOut(u64, Result<(Arc<Projects>, Vec<String>), String>),
    Key(keyboard::Event),
    MemoryTick,
}

/// A decode event in the form the UI keeps: iced clones messages, so the pixels live behind an
/// image handle, which is cheap to clone, instead of in a plain buffer.
#[derive(Clone)]
enum Media {
    Opened(ClipInfo),
    Picture {
        picture: PictureInfo,
        surface: Surface,
    },
    Failed(String),
}

/// A picture's pixels, in the form its viewer path draws.
#[derive(Clone)]
enum Surface {
    /// Still has to be uploaded to the GPU before it can be shown.
    Image(image::Handle),
    Yuv(YuvPicture),
}

/// How pictures get from the decoder to the screen.
#[derive(Clone, Copy)]
enum ViewerPath {
    /// The decoder's YUV planes are uploaded as they are and a shader converts them.
    Shader,
    /// Pictures are converted to RGB on the decode thread; iced's software renderer can show
    /// those.
    Cpu,
}

impl ViewerPath {
    fn output(self) -> Output {
        match self {
            Self::Shader => Output::Yuv,
            Self::Cpu => Output::Rgba,
        }
    }
}

/// What the viewer wants pictures to look like.
#[derive(Clone, Copy, PartialEq)]
struct Form {
    output: Output,
    /// The viewer's area in physical pixels; pictures are scaled down to fit inside.
    max_size: (u32, u32),
}

#[derive(Clone, Copy)]
struct PictureInfo {
    request: u64,
    exact: bool,
    hardware: bool,
    size: (u32, u32),
}

impl From<Event> for Media {
    fn from(event: Event) -> Self {
        match event {
            Event::Opened(info) => Self::Opened(info),
            Event::Picture(picture) => Self::Picture {
                picture: PictureInfo {
                    request: picture.request,
                    exact: picture.exact,
                    hardware: picture.timing.hardware,
                    size: (picture.width, picture.height),
                },
                surface: match picture.pixels {
                    Pixels::Rgba(rgba) => Surface::Image(image::Handle::from_rgba(
                        picture.width,
                        picture.height,
                        rgba,
                    )),
                    Pixels::Nv12 {
                        luma,
                        chroma,
                        colors,
                    } => Surface::Yuv(YuvPicture::new(
                        picture.width,
                        picture.height,
                        luma,
                        chroma,
                        colors,
                    )),
                },
            },
            Event::Failed(error) => Self::Failed(error),
        }
    }
}

/// An edit being dragged in the timeline, shown before it is made.
struct Preview {
    timeline: Timeline,
    /// The frame the viewer shows in place of the playhead's: the edge being trimmed.
    look_at: Option<i64>,
}

/// What the playhead follows while playing.
#[derive(Clone, Copy)]
enum Clock {
    /// The sound card: the playhead is wherever the sound being heard is, so pictures keep
    /// pace with the sound.
    Sound { asked_at: Instant },
    /// The system clock, from where playing started; when there is no sound card to follow.
    Wall { started: Instant, from: i64 },
}

/// Playback under way: the playhead follows the clock.
struct Playback {
    clock: Clock,
    /// The next frame on which the picture comes from another clip, once worked out, and
    /// whether the decoder for that clip has been got ready.
    cut: Option<i64>,
    warmed: bool,
    /// The timeline frame asked for last; it runs a little ahead of the clock.
    asked: i64,
    /// Requests made while playing: source, request and the timeline frame it was for.
    requests: VecDeque<(u64, u64, i64)>,
    /// Pictures that have arrived and wait for their frame's turn, earliest first.
    ready: VecDeque<(i64, Shown)>,
    /// When the display shows its frames, at a steady pace.
    ticks: Ticks,
    /// How far, in frames, the frame shown runs ahead of the clock: nudges that keep frame
    /// starts off the display's ticks.
    shift: f64,
}

impl Playback {
    /// The frame to show when the clock stands at `exact` frames. The sound card and the
    /// display keep slightly different time, so now and then a frame start drifts onto a
    /// display tick, where it would fall either side of it by turns. It is nudged off the tick
    /// instead, the way it already fell, so the frame shown does not change for it.
    fn frame_at(&mut self, exact: f64) -> i64 {
        let shifted = exact + self.shift;
        let near = shifted - shifted.round();
        if near.abs() < NEAR_TICK {
            self.shift += TICK_NUDGE.copysign(near);
            // Half a frame of nudges, which takes minutes: one frame shows twice or not at
            // all, and the nudging starts over.
            if self.shift.abs() > 0.5 {
                self.shift -= self.shift.signum();
            }
        }
        (exact + self.shift).floor() as i64
    }
}

/// The times of the display's frames, one refresh apart. Ticks reach the app a little early or
/// late, by a millisecond or two; a frame that starts close to a tick would then show a tick
/// early or late by turns. Which frame to show is decided at these steady times instead.
#[derive(Default)]
struct Ticks {
    last: Option<Instant>,
    /// Seconds from one display frame to the next.
    period: Option<f64>,
    /// The first intervals between ticks, until the pace is known.
    intervals: Vec<f64>,
}

impl Ticks {
    /// The steady time of the display frame a tick arriving `now` stands for.
    fn steady(&mut self, now: Instant) -> Instant {
        let since = |earlier: Instant| match now.checked_duration_since(earlier) {
            Some(after) => after.as_secs_f64(),
            None => -earlier.duration_since(now).as_secs_f64(),
        };
        let (Some(last), Some(period)) = (self.last, self.period) else {
            // The first ticks: the pace is not known yet.
            if let Some(last) = self.last {
                self.intervals.push(since(last));
                if self.intervals.len() == PACE_SAMPLES {
                    self.intervals.sort_by(f64::total_cmp);
                    let median = self.intervals[PACE_SAMPLES / 2];
                    self.period = Some(median).filter(|period| *period > 0.0);
                    self.intervals.clear();
                }
            }
            self.last = Some(now);
            return now;
        };
        // Display frames the app missed are counted in.
        let frames = (since(last) / period).round().max(1.0);
        let expected = last + Duration::from_secs_f64(frames * period);
        let error = since(expected);
        if error.abs() > period / 2.0 {
            // Out of step: the display changed, or the app stalled. Start over from here.
            *self = Self {
                last: Some(now),
                ..Self::default()
            };
            return now;
        }
        let steady = if error >= 0.0 {
            expected + Duration::from_secs_f64(error * TICK_PULL)
        } else {
            expected - Duration::from_secs_f64(-error * TICK_PULL)
        };
        self.period = Some(period + error * TICK_PULL / frames);
        self.last = Some(steady);
        steady
    }
}

/// How smoothly the last stretch of playback went.
#[derive(Clone, Copy, Default)]
struct PlayStats {
    pictures: u32,
    /// When the last picture went on screen; None across stretches where nothing plays.
    last: Option<Instant>,
    /// How many times one picture followed another, and how long that took in all.
    steps: u32,
    stepping: Duration,
    /// The longest wait between two pictures in a row.
    longest_gap: Duration,
}

struct App {
    history: History,
    /// The project's file, once it has been saved or was opened from one.
    project_path: Option<PathBuf>,
    /// Where the project file is; relative media paths start from here.
    project_folder: Option<PathBuf>,
    selected: Option<ClipId>,
    preview: Option<Preview>,
    /// The timeline frame the viewer shows.
    playhead: i64,
    /// The media files that are open, the one used longest ago first.
    sources: Vec<Source>,
    /// Counts opened sources, so events from one that was since closed can be told apart.
    generation: u64,
    /// Files waiting to go on the timeline, in the order they were chosen.
    adding: VecDeque<PathBuf>,
    /// The source being opened for the first file in `adding`.
    opening: Option<u64>,
    /// A project for the first file in `adding` is being loaded.
    opening_project: bool,
    /// The projects stepped out of to edit one they use, outermost first.
    parents: Vec<Parent>,
    /// Inside a nested project: how it was when stepped into or last saved, which tells whether
    /// it has changes to save on the way out.
    unchanged: Option<Timeline>,
    /// Counts steps in and out, so that what was loaded for a project no longer shown is left
    /// alone.
    nest_step: u64,
    /// The source of the clip under the playhead; pictures from any other are out of date.
    active: Option<u64>,
    /// The source being got ready for the clip after the next cut.
    warming: Option<u64>,
    /// Plays the timeline's sound, and keeps time while playing.
    sound: Player,
    /// Plays without sound, as asked on the command line.
    silent: bool,
    /// The sound delay asked for on the command line, in milliseconds, instead of the one in
    /// the settings.
    sound_delay: Option<u64>,
    settings: Settings,
    playback: Option<Playback>,
    play_stats: Option<PlayStats>,
    pending: Option<Pending>,
    /// Pictures accepted from decoders so far; gives each one its place in line.
    arrivals: u64,
    shown: Option<Shown>,
    latency: Latency,
    /// Unknown until the window's renderer has been probed; chosen once.
    viewer: Option<ViewerPath>,
    /// The viewer's area in logical pixels, as last laid out.
    viewer_area: Option<Size>,
    scale_factor: Option<f32>,
    /// When the viewer last changed while the pictures have not followed yet.
    resized_at: Option<Instant>,
    /// What decoders are asked for; unknown until the viewer's path and size both are.
    form: Option<Form>,
    /// The part of the timeline its panel shows; None shows all of it.
    timeline_view: Option<View>,
    notice: Option<String>,
    memory_mb: Option<usize>,
    /// The waveforms of the media files the project shown uses, by their names in it, as the
    /// panel finds them.
    waveforms: Waveforms,
    /// Every waveform read so far, by file ([`file_key`]): projects stepped into and out of
    /// share them.
    waveform_files: HashMap<String, Arc<Waveform>>,
    /// Files whose waveform has been asked for, whether it has come yet or not.
    waveforms_asked: HashSet<String>,
    /// Files waiting for their waveform to be read, one at a time.
    waveform_queue: VecDeque<(String, PathBuf)>,
    reading_waveform: bool,
    /// The volume the slider stands at while it is dragged.
    volume_drag: Option<f32>,
    exporting: Option<Exporting>,
    /// The export choices are on show.
    export_dialog: bool,
    /// The picture sizes of media files, by their names in the project, as far as known.
    picture_sizes: HashMap<String, (u32, u32)>,
    /// Files whose picture size is being read.
    sizing: HashSet<String>,
}

/// An export under way.
struct Exporting {
    export: Export,
    started: Instant,
    /// Frames made so far, of how many.
    done: i64,
    total: i64,
    encoder: Option<&'static str>,
}

/// An open media file: a decode thread that makes pictures of its frames.
struct Source {
    generation: u64,
    /// The file's key: its full path ([`file_key`]).
    target: String,
    name: String,
    scrubber: Scrubber,
    /// Unknown until the decode thread has opened the file.
    info: Option<ClipInfo>,
    /// The form last asked of the decoder.
    form: Option<Form>,
    /// The frame last asked for: where its decoder is, or is heading.
    last_frame: Option<u64>,
}

impl Source {
    fn request(&mut self, frame: u64, form: Form) -> u64 {
        if self.form != Some(form) {
            self.form = Some(form);
            self.scrubber.set_output(form.output);
            self.scrubber.set_max_preview(form.max_size);
        }
        self.last_frame = Some(frame);
        self.scrubber.request(frame)
    }
}

/// A project stepped out of to edit one of the projects it uses, as it was left.
struct Parent {
    history: History,
    path: Option<PathBuf>,
    folder: Option<PathBuf>,
    unchanged: Option<Timeline>,
    playhead: i64,
    selected: Option<ClipId>,
    view: Option<View>,
}

/// The newest request, kept to time how long its pictures take to reach the UI.
struct Pending {
    generation: u64,
    request: u64,
    at: Instant,
    seen: bool,
}

struct Shown {
    picture: Displayed,
    order: u64,
    exact: bool,
    hardware: bool,
    size: (u32, u32),
}

enum Displayed {
    /// Holding the allocation keeps the picture on the GPU, so drawing it never shows a gap.
    Image(Allocation),
    Yuv(YuvPicture),
}

#[derive(Default)]
struct Latency {
    first_picture: Option<Duration>,
    exact: Option<Duration>,
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let mut app = Self {
            history: History::new(Timeline::new("Untitled", EMPTY_PROJECT_RATE)),
            project_path: None,
            project_folder: None,
            selected: None,
            preview: None,
            playhead: 0,
            sources: Vec::new(),
            generation: 0,
            adding: VecDeque::new(),
            opening: None,
            opening_project: false,
            parents: Vec::new(),
            unchanged: None,
            nest_step: 0,
            active: None,
            warming: None,
            sound: Player::start(),
            silent: false,
            sound_delay: None,
            settings: Settings::load(),
            playback: None,
            play_stats: None,
            pending: None,
            arrivals: 0,
            shown: None,
            latency: Latency::default(),
            viewer: None,
            viewer_area: None,
            scale_factor: None,
            resized_at: None,
            form: None,
            timeline_view: None,
            notice: None,
            memory_mb: None,
            waveforms: Waveforms::new(),
            waveform_files: HashMap::new(),
            waveforms_asked: HashSet::new(),
            waveform_queue: VecDeque::new(),
            reading_waveform: false,
            volume_drag: None,
            exporting: None,
            export_dialog: false,
            picture_sizes: HashMap::new(),
            sizing: HashSet::new(),
        };
        let mut playhead = 0;
        let mut export = None;
        for argument in std::env::args_os().skip(1) {
            let path = PathBuf::from(&argument);
            let text = argument.to_string_lossy();
            if text == CPU_VIEWER_FLAG {
                app.viewer = Some(ViewerPath::Cpu);
            } else if text == NO_SOUND_FLAG {
                app.silent = true;
            } else if let Some(milliseconds) = text.strip_prefix(SOUND_DELAY_FLAG) {
                app.sound_delay = milliseconds.parse().ok();
            } else if let Some(film) = text.strip_prefix(EXPORT_FLAG) {
                export = Some(PathBuf::from(film));
            } else if let Some(frame) = text.strip_prefix(PLAYHEAD_FLAG) {
                playhead = frame.parse().unwrap_or(0);
            } else if let Some(added) = text.strip_prefix(ADD_FLAG) {
                app.adding.push_back(PathBuf::from(added));
            } else if path.extension().is_some_and(|extension| extension == PROJECT_EXTENSION) {
                match Timeline::load(&path) {
                    Ok(timeline) => drop(app.adopt_project(path, timeline)),
                    Err(error) => app.notice = Some(error.to_string()),
                }
            } else {
                app.adding.push_back(path);
            }
        }
        app.playhead = playhead;
        let mut task = app.add_next();
        if let Some(film) = export {
            task = Task::batch([task, app.export(film)]);
        }
        (app, task)
    }

    /// Replaces what is being edited with a project read from `path`.
    fn adopt_project(&mut self, path: PathBuf, timeline: Timeline) -> Task<Message> {
        self.history = History::new(timeline);
        // Whatever projects were stepped out of are left behind with the old project.
        self.parents.clear();
        self.unchanged = None;
        self.nest_step += 1;
        self.project_folder = path.parent().map(Path::to_path_buf);
        self.project_path = Some(path);
        self.selected = None;
        self.preview = None;
        self.stop_playing();
        self.warming = None;
        self.playhead = 0;
        // The old project's files are closed; the new one's open as the playhead reaches them.
        self.sources.clear();
        self.adding.clear();
        self.opening = None;
        self.opening_project = false;
        self.active = None;
        self.pending = None;
        self.shown = None;
        self.notice = None;
        self.show_playhead()
    }

    fn save(&self, path: PathBuf) -> Task<Message> {
        // Tracks are shared, so the copy handed to the background is cheap.
        let timeline = self.history.timeline().clone();
        let saving = async move {
            match timeline.save(&path) {
                Ok(()) => Ok(path),
                Err(error) => Err(error.to_string()),
            }
        };
        Task::perform(saving, Message::Saved)
    }

    /// Starts on the next file waiting to go on the timeline. Its length has to be known first,
    /// which takes opening it; files go on one at a time so they keep the order they were
    /// chosen in.
    fn add_next(&mut self) -> Task<Message> {
        if self.opening.is_some() || self.opening_project {
            return Task::none();
        }
        while let Some(path) = self.adding.front().cloned() {
            if is_project(&path) {
                // Read off the UI thread with the projects it uses in turn.
                self.opening_project = true;
                let loading = path.clone();
                let load = async move { Nested::load(&loading).map_err(|error| error.to_string()) };
                let shown = self.show_playhead();
                let loaded = Task::perform(load, move |loaded| {
                    Message::NestedLoaded(path.clone(), loaded)
                });
                return Task::batch([loaded, shown]);
            }
            let key = file_key(&path);
            let (index, task) = match self.source(&key, &path, 0, false) {
                Ok(found) => found,
                Err(error) => {
                    self.notice = Some(error);
                    self.adding.pop_front();
                    continue;
                }
            };
            match self.sources[index].info.clone() {
                // Already open: a file can be on the timeline more than once.
                Some(info) => {
                    self.adding.pop_front();
                    self.place_at_end(&path, &info);
                }
                None => {
                    self.opening = Some(self.sources[index].generation);
                    // What is on the timeline already does not wait for the rest.
                    let shown = self.show_playhead();
                    return Task::batch([task, shown]);
                }
            }
        }
        self.show_playhead()
    }

    /// Opens the project a clip plays in place of the one it sits in, which waits on the path
    /// in the header. The playhead goes to the frame of that project it stood on.
    fn step_inside(&mut self, id: ClipId) -> Task<Message> {
        let timeline = self.history.timeline();
        let Some((_, clip, start)) = timeline.clip(id) else {
            return Task::none();
        };
        let Some(nested) = clip.media.project.clone() else {
            if clip.media.is_project() {
                self.notice = Some(format!("the project of \"{}\" could not be read", clip.name));
            }
            return Task::none();
        };
        let offset = (self.playhead - start).clamp(0, clip.duration - 1);
        let frame = clip.media_frame(offset, timeline.rate());
        self.stop_playing();
        self.preview = None;
        let parent = Parent {
            history: std::mem::replace(&mut self.history, History::new(nested.timeline.clone())),
            path: self.project_path.replace(nested.path.clone()),
            folder: self.project_folder.replace(nested.folder.clone()),
            unchanged: self.unchanged.replace(nested.timeline.clone()),
            playhead: self.playhead,
            selected: self.selected.take(),
            view: self.timeline_view.take(),
        };
        self.parents.push(parent);
        self.nest_step += 1;
        self.notice = None;
        self.playhead = frame;
        // Decoders stay open: they are known by file, so the pictures inside carry on.
        self.timeline_changed()
    }

    /// Goes back to the project at `level` of the path. Each project stepped out of that has
    /// changes is saved, so the projects using it show them, and the one arrived at loads the
    /// projects it uses afresh; both off the UI thread.
    fn step_out(&mut self, level: usize) -> Task<Message> {
        if level >= self.parents.len() {
            return Task::none();
        }
        self.stop_playing();
        self.preview = None;
        // Innermost first, so that a project is saved before the one around it is read.
        let mut saves = Vec::new();
        while self.parents.len() > level {
            let parent = self.parents.pop().expect("there are parents left");
            let left = std::mem::replace(&mut self.history, parent.history);
            if let Some(path) = self.project_path.take()
                && self.unchanged.as_ref() != Some(left.timeline())
            {
                saves.push((left.timeline().clone(), path));
            }
            self.project_path = parent.path;
            self.project_folder = parent.folder;
            self.unchanged = parent.unchanged;
            self.playhead = parent.playhead;
            self.selected = parent.selected;
            self.timeline_view = parent.view;
        }
        self.nest_step += 1;
        let step = self.nest_step;
        let timeline = self.history.timeline().clone();
        let folder = self.project_folder.clone().unwrap_or_default();
        let work = async move {
            let mut saved = Vec::new();
            for (timeline, path) in saves {
                timeline.save(&path).map_err(|error| error.to_string())?;
                saved.push(file_name(&path));
            }
            let projects = Projects::load(&timeline, &folder).map_err(|error| error.to_string())?;
            Ok((Arc::new(projects), saved))
        };
        let shown = self.timeline_changed();
        Task::batch([
            Task::perform(work, move |result| Message::SteppedOut(step, result)),
            shown,
        ])
    }

    /// Puts all of a media file on the main track, after what is there.
    fn place_at_end(&mut self, path: &Path, info: &ClipInfo) {
        let media_rate = Rate::new(
            info.frame_rate.num.max(1) as u32,
            info.frame_rate.den.max(1) as u32,
        );
        let media = TimelineMedia::from_path(path, media_rate, info.frame_count as i64);
        self.place_media_at_end(path, media);
    }

    /// Puts all of another project on the main track, after what is there, unless that project
    /// holds the one being edited: a project cannot go inside itself.
    fn place_project_at_end(&mut self, path: &Path, nested: Arc<Nested>) {
        if let Some(own) = &self.project_path
            && nested.holds(own)
        {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            self.notice = Some(format!("{name} uses this project, so it cannot go inside it"));
            return;
        }
        self.place_media_at_end(path, TimelineMedia::from_project(path, nested));
    }

    fn place_media_at_end(&mut self, path: &Path, media: TimelineMedia) {
        let media_rate = media.rate;
        // The first clip decides the project's frame rate.
        let empty = |timeline: &Timeline| timeline.tracks().all(|track| track.items().is_empty());
        if empty(self.history.timeline()) && self.history.timeline().rate() != media_rate {
            let name = self.history.timeline().name.clone();
            self.history = History::new(Timeline::new(name, media_rate));
        }
        let timeline = self.history.timeline();
        let name = path
            .file_stem()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(clip) = NewClip::whole(name, Arc::new(media), timeline.rate()) else {
            self.notice = Some(format!("{} has no frames", path.display()));
            return;
        };
        let edit = Edit::Place {
            track: 0,
            at: timeline.track(0).map_or(0, |track| track.duration()),
            clip,
            mode: Placement::Insert,
        };
        match self.history.apply(&edit) {
            Ok(_) => self.notice = None,
            Err(error) => self.notice = Some(error.to_string()),
        }
    }

    /// An open source for a media file to show `frame` from, opening one if need be. `target`
    /// is the file's key ([`file_key`]). A file can be open more than once; the source whose
    /// decoder is nearest that frame is taken. With `spare` the source showing the picture now
    /// is left alone. Returns the source's place in `sources` and the task that brings in its
    /// events.
    fn source(
        &mut self,
        target: &str,
        path: &Path,
        frame: u64,
        spare: bool,
    ) -> Result<(usize, Task<Message>), String> {
        let mut task = Task::none();
        let nearest = (0..self.sources.len())
            .filter(|index| {
                let source = &self.sources[*index];
                source.target == target && !(spare && Some(source.generation) == self.active)
            })
            .min_by_key(|index| {
                let last = self.sources[*index].last_frame;
                last.map_or(u64::MAX, |last| last.abs_diff(frame))
            });
        match nearest {
            Some(index) => {
                let source = self.sources.remove(index);
                self.sources.push(source);
            }
            None => {
                self.generation += 1;
                let generation = self.generation;
                let (sender, events) = mpsc::unbounded();
                let options = Options {
                    cache_bytes: SOURCE_CACHE_BYTES,
                    ..Options::default()
                };
                let scrubber = Scrubber::open(path, options, move |event| {
                    let _ = sender.unbounded_send(event);
                })
                .map_err(|error| error.to_string())?;
                self.sources.push(Source {
                    generation,
                    target: target.to_string(),
                    name: path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    scrubber,
                    info: None,
                    form: None,
                    last_frame: None,
                });
                task = Task::run(events, move |event| Message::Media(generation, event.into()));
                // Closing a source ends its decode thread and frees its pictures.
                while self.sources.len() > MAX_OPEN_SOURCES {
                    let idle = self.sources.iter().position(|source| {
                        ![self.active, self.opening, self.warming, Some(generation)]
                            .contains(&Some(source.generation))
                    });
                    match idle {
                        Some(index) => drop(self.sources.remove(index)),
                        None => break,
                    }
                }
            }
        }
        Ok((self.sources.len() - 1, task))
    }

    /// Asks for the picture of the frame under the playhead; while an edit is being dragged, of
    /// the frame that edit wants looked at.
    fn show_playhead(&mut self) -> Task<Message> {
        let look_at = self.preview.as_ref().and_then(|preview| preview.look_at);
        self.show_frame(look_at.unwrap_or(self.playhead))
    }

    /// Asks for the picture of timeline frame `at`; while an edit is being dragged, of the
    /// timeline as that edit would leave it.
    fn show_frame(&mut self, at: i64) -> Task<Message> {
        let Some(form) = self.form else {
            // The first picture waits until the viewer knows what it wants.
            return Task::none();
        };
        let timeline = match &self.preview {
            Some(preview) => &preview.timeline,
            None => self.history.timeline(),
        };
        let Some(shot) = timeline.shot_at(at) else {
            // Nothing plays here: the viewer goes black. While playing that waits for the
            // clock to get there.
            self.active = None;
            self.pending = None;
            if self.playback.is_none() {
                self.shown = None;
            }
            return Task::none();
        };
        let frame = shot.media_frame.max(0) as u64;
        let Some((target, path)) = self.location(&shot) else {
            self.notice = Some(format!("the media of \"{}\" is missing", shot.clip.name));
            self.active = None;
            self.shown = None;
            return Task::none();
        };
        match self.source(&target, &path, frame, false) {
            Ok((index, task)) => {
                let source = &mut self.sources[index];
                self.active = Some(source.generation);
                if self.warming == self.active {
                    self.warming = None;
                }
                let pending = Pending {
                    generation: source.generation,
                    request: source.request(frame, form),
                    at: Instant::now(),
                    seen: false,
                };
                if let Some(playback) = &mut self.playback {
                    let asked = (pending.generation, pending.request, at);
                    playback.requests.push_back(asked);
                    if playback.requests.len() > REMEMBERED_REQUESTS {
                        playback.requests.pop_front();
                    }
                }
                self.pending = Some(pending);
                task
            }
            Err(error) => {
                self.notice = Some(error);
                self.active = None;
                self.shown = None;
                Task::none()
            }
        }
    }

    /// Where the media file of a picture is: its key ([`file_key`]) and its path on disk. None
    /// when it is missing.
    fn location(&self, shot: &Shot) -> Option<(String, PathBuf)> {
        let path = shot.file(self.project_folder.as_deref())?;
        Some((file_key(&path), path))
    }

    /// While playing, gets the decoder for the clip after the next cut ready a little ahead of
    /// time: opened, and on that clip's first frame. Without this the picture would stand still
    /// at every cut for as long as opening and seeking take.
    fn warm_up(&mut self) -> Task<Message> {
        let (Some(form), Some(playback)) = (self.form, &mut self.playback) else {
            return Task::none();
        };
        let timeline = self.history.timeline();
        let rate = timeline.rate();
        if playback.cut.is_none_or(|cut| cut <= self.playhead) {
            playback.cut = timeline.next_picture_change(self.playhead);
            playback.warmed = false;
        }
        let Some(cut) = playback.cut else {
            return Task::none();
        };
        let ahead = (WARM_UP_SECONDS * rate.as_f64()) as i64;
        if playback.warmed || cut - self.playhead > ahead {
            return Task::none();
        }
        playback.warmed = true;
        let Some(shot) = timeline.shot_at(cut) else {
            // Nothing plays after the cut.
            return Task::none();
        };
        let frame = shot.media_frame.max(0) as u64;
        let Some((target, path)) = self.location(&shot) else {
            return Task::none();
        };
        // Where the next clip goes on in the file from where this one stops, the decoder at
        // work simply carries on.
        let carries_on = timeline.shot_at(cut - 1).is_some_and(|before| {
            let stops_at = before.media_frame.max(0) as u64;
            self.location(&before).is_some_and(|(key, _)| key == target)
                && (stops_at..=stops_at + CARRY_ON_FRAMES).contains(&frame)
        });
        if carries_on {
            return Task::none();
        }
        match self.source(&target, &path, frame, true) {
            Ok((index, task)) => {
                let source = &mut self.sources[index];
                self.warming = Some(source.generation);
                // Its picture is not shown; asking for it is what puts the decoder in place.
                source.request(frame, form);
                task
            }
            Err(_) => Task::none(),
        }
    }

    /// A display frame while playing: the clock says which timeline frame is due, the picture
    /// for it goes on screen if it has arrived, and the frame a little further on is asked for.
    /// A decoder that cannot keep up leaves frames out; it does not slow the film down.
    fn play_tick(&mut self) -> Task<Message> {
        let rate = self.history.timeline().rate().as_f64();
        let last_frame = self.last_frame();
        let Some(playback) = &mut self.playback else {
            return Task::none();
        };
        // Not the time the display frame was stamped with: the tick arrives one or two display
        // frames after that, and which of the two changes from run to run, which would move
        // the pictures against the sound by as much. What is drawn now goes up next.
        let now = playback.ticks.steady(Instant::now());
        let frame = match playback.clock {
            Clock::Sound { asked_at } => match self.sound.position(now) {
                Some(seconds) => playback.frame_at(seconds * rate),
                // The sound card does not answer: the system clock takes over from here.
                None if now.saturating_duration_since(asked_at) > SOUND_START_LIMIT => {
                    self.sound.stop();
                    playback.clock = Clock::Wall {
                        started: now,
                        from: self.playhead,
                    };
                    self.playhead
                }
                // The pictures wait for the sound to start.
                None => self.playhead,
            },
            Clock::Wall { started, from } => {
                let elapsed = now.saturating_duration_since(started).as_secs_f64();
                playback.frame_at(from as f64 + elapsed * rate)
            }
        };
        // The sound card's first guesses at its delay can be a few milliseconds out; the
        // playhead never steps back for that.
        let frame = frame.clamp(self.playhead, last_frame);
        self.playhead = frame;

        // The newest picture that is due; older ones it overtook are dropped.
        let mut due = None;
        while playback.ready.front().is_some_and(|(at, _)| *at <= frame) {
            due = playback.ready.pop_front();
        }
        let ahead = (frame + (LEAD_SECONDS * rate).ceil() as i64).min(last_frame);
        let ask = (ahead > playback.asked).then(|| playback.asked = ahead);
        if let Some((_, shown)) = due {
            self.shown = Some(shown);
            if let Some(stats) = &mut self.play_stats {
                if let Some(last) = stats.last {
                    let gap = now.saturating_duration_since(last);
                    stats.steps += 1;
                    stats.stepping += gap;
                    // The picture on screen when play was pressed does not count: what follows
                    // it is the start-up, not a stutter.
                    if stats.pictures > 1 {
                        stats.longest_gap = stats.longest_gap.max(gap);
                    }
                }
                stats.pictures += 1;
                stats.last = Some(now);
            }
        }
        if self.history.timeline().shot_at(frame).is_none() {
            // Nothing plays here. The wait for the next picture is no stutter.
            self.shown = None;
            if let Some(stats) = &mut self.play_stats {
                stats.last = None;
            }
        }
        if frame >= last_frame {
            self.stop_playing();
            return self.show_playhead();
        }
        let asked = if ask.is_some() {
            self.show_frame(ahead)
        } else {
            Task::none()
        };
        Task::batch([asked, self.warm_up()])
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        let task = self.handle(message);
        // Whatever happened may have put a new file on the timeline.
        Task::batch([task, self.read_waveforms()])
    }

    /// Asks for the waveform of every media file on the timeline that has none yet.
    fn read_waveforms(&mut self) -> Task<Message> {
        let mut wanted = Vec::new();
        let mut shown = Waveforms::new();
        let folder = self.project_folder.as_deref();
        for track in self.history.timeline().tracks() {
            for (_, clip) in track.clips() {
                let (Some(target), Some(path)) = (&clip.media.target, clip.media.file(folder))
                else {
                    continue;
                };
                // Read by file, found by the panel by the media's name in the project.
                let key = file_key(&path);
                match self.waveform_files.get(&key) {
                    Some(waveform) => drop(shown.insert(target.clone(), waveform.clone())),
                    None if !self.waveforms_asked.contains(&key) => wanted.push((key, path)),
                    None => {}
                }
            }
        }
        self.waveforms = shown;
        for (key, path) in wanted {
            if self.waveforms_asked.insert(key.clone()) {
                self.waveform_queue.push_back((key, path));
            }
        }
        self.next_waveform()
    }

    /// Reads the next waveform waiting, on a thread of its own: it takes reading the file's
    /// whole sound. One at a time, so the files being played from keep the disk to themselves.
    fn next_waveform(&mut self) -> Task<Message> {
        if self.reading_waveform {
            return Task::none();
        }
        let Some((target, path)) = self.waveform_queue.pop_front() else {
            return Task::none();
        };
        let (sender, read) = oneshot::channel();
        let started = std::thread::Builder::new()
            .name("cannotcut-waveform".to_string())
            .spawn(move || {
                let _ = sender.send(Waveform::read(&path, WAVEFORM_RATE));
            });
        if started.is_err() {
            return Task::none();
        }
        self.reading_waveform = true;
        Task::perform(
            async move { read.await.ok().and_then(Result::ok).flatten().map(Arc::new) },
            move |waveform| Message::WaveformRead(target.clone(), waveform),
        )
    }

    fn handle(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::AddRequested => return Task::perform(pick_clips(), Message::FilesChosen),
            Message::FilesChosen(paths) => {
                self.adding.extend(paths);
                return self.add_next();
            }
            Message::RendererProbed(on_gpu) => {
                if self.viewer.is_none() {
                    self.viewer = Some(if on_gpu {
                        ViewerPath::Shader
                    } else {
                        ViewerPath::Cpu
                    });
                    return self.viewer_changed();
                }
            }
            Message::ViewerResized(area) => {
                self.viewer_area = Some(area);
                if self.scale_factor.is_none() {
                    return window::latest()
                        .and_then(window::scale_factor)
                        .map(Message::Rescaled);
                }
                return self.viewer_changed();
            }
            Message::Rescaled(scale_factor) => {
                self.scale_factor = Some(scale_factor);
                return self.viewer_changed();
            }
            Message::ResizeTick => {
                if self
                    .resized_at
                    .is_some_and(|at| at.elapsed() >= RESIZE_SETTLE)
                {
                    self.resized_at = None;
                    return self.adopt_form();
                }
            }
            Message::Media(generation, event) => return self.media_event(generation, event),
            Message::Uploaded {
                generation,
                order,
                picture,
                allocation,
            } => match allocation {
                Some(allocation) => {
                    self.show(generation, order, picture, Displayed::Image(allocation));
                }
                None => self.notice = Some("could not upload a picture to the GPU".to_string()),
            },
            Message::OpenRequested => {
                return Task::perform(pick_project(), Message::ProjectChosen);
            }
            Message::ProjectChosen(Some(path)) => {
                return Task::perform(load_project(path), Message::ProjectLoaded);
            }
            Message::ProjectLoaded(Ok((path, timeline))) => {
                return self.adopt_project(path, timeline);
            }
            Message::SaveRequested => {
                return match self.project_path.clone() {
                    Some(path) => self.save(path),
                    None => {
                        let name = self.history.timeline().name.clone();
                        Task::perform(pick_save_path(name), Message::SaveChosen)
                    }
                };
            }
            Message::SaveChosen(Some(path)) => return self.save(path),
            Message::ExportOpened => {
                if self.exporting.is_none() {
                    self.export_dialog = true;
                    return self.read_film_size();
                }
            }
            Message::ExportClosed => self.export_dialog = false,
            Message::ExportResolution(resolution) => self.settings.export.resolution = resolution,
            Message::ExportQuality(quality) => self.settings.export.quality = quality,
            Message::PictureSized(target, size) => {
                self.sizing.remove(&target);
                if let Some(size) = size {
                    self.picture_sizes.insert(target, size);
                }
            }
            Message::ExportRequested => {
                let name = self.history.timeline().name.clone();
                return Task::perform(pick_film_path(name), Message::ExportChosen);
            }
            Message::ExportChosen(Some(path)) => {
                self.export_dialog = false;
                self.settings.save_export();
                return self.export(path);
            }
            Message::Exported(event) => self.export_event(event),
            Message::ExportCancelled => {
                if let Some(exporting) = &self.exporting {
                    exporting.export.cancel();
                }
            }
            Message::Saved(Ok(path)) => {
                self.notice = Some(format!("Saved {}", path.display()));
                // Inside a nested project, what was saved is nothing to save on the way out.
                if !self.parents.is_empty() && self.project_path.as_ref() == Some(&path) {
                    self.unchanged = Some(self.history.timeline().clone());
                }
                self.project_folder = path.parent().map(Path::to_path_buf);
                self.project_path = Some(path);
            }
            Message::ProjectLoaded(Err(error)) | Message::Saved(Err(error)) => {
                self.notice = Some(error);
            }
            Message::ProjectChosen(None)
            | Message::SaveChosen(None)
            | Message::ExportChosen(None) => {}
            Message::Timeline(action) => match action {
                Action::Scrub(frame) => return self.move_playhead(frame),
                Action::View(view) => self.timeline_view = view,
                Action::Select(clip) => self.selected = clip,
                Action::Preview(edit) => {
                    let mut timeline = self.history.timeline().clone();
                    if timeline.apply(&edit).is_ok() {
                        // While an edge is trimmed the viewer shows the frame it now stands on.
                        let look_at = match edit {
                            Edit::Trim { clip, edge, .. } => {
                                timeline.clip(clip).map(|(_, clip, start)| match edge {
                                    Edge::Start => start,
                                    Edge::End => start + clip.duration - 1,
                                })
                            }
                            _ => None,
                        };
                        self.preview = Some(Preview { timeline, look_at });
                        return self.show_playhead();
                    }
                }
                Action::Commit(edit) => return self.edit(&edit),
                Action::Open(clip) => return self.step_inside(clip),
                Action::Cancel => {
                    if self.preview.take().is_some() {
                        return self.show_playhead();
                    }
                }
            },
            Message::Stepped(frames) => {
                return self.move_playhead(self.playhead.saturating_add(frames));
            }
            Message::PlayToggled => {
                let was_playing = self.playback.is_some();
                self.stop_playing();
                if !was_playing && self.history.timeline().duration() > 0 {
                    // From the end, playing starts over.
                    if self.playhead >= self.last_frame() {
                        self.playhead = 0;
                    }
                    self.preview = None;
                    let clock = match self.sound.status() {
                        SoundStatus::Ready { .. } if !self.silent => {
                            let delay = self.output_delay().unwrap_or(0);
                            self.sound.set_delay(Duration::from_millis(delay));
                            // Tracks are shared, so the copy for the sound thread is cheap.
                            let timeline = self.history.timeline().clone();
                            let folder = self.project_folder.clone();
                            self.sound.play(timeline, self.playhead, folder);
                            Clock::Sound {
                                asked_at: Instant::now(),
                            }
                        }
                        _ => Clock::Wall {
                            started: Instant::now(),
                            from: self.playhead,
                        },
                    };
                    self.playback = Some(Playback {
                        clock,
                        cut: None,
                        warmed: false,
                        asked: self.playhead,
                        requests: VecDeque::new(),
                        ready: VecDeque::new(),
                        ticks: Ticks::default(),
                        shift: 0.0,
                    });
                    self.play_stats = Some(PlayStats::default());
                    return self.warm_up();
                }
                // Paused: the frame the playhead stopped on, exactly.
                return self.show_playhead();
            }
            Message::PlayTick => return self.play_tick(),
            Message::Undo => {
                if self.history.undo() {
                    return self.timeline_changed();
                }
            }
            Message::Redo => {
                if self.history.redo() {
                    return self.timeline_changed();
                }
            }
            Message::Split => {
                if let Some(edit) = self.split_edit() {
                    return self.edit(&edit);
                }
            }
            Message::Delete => {
                if let Some(edit) = self.delete_edit() {
                    return self.edit(&edit);
                }
            }
            Message::AddTrack => {
                let tracks = self.history.timeline().tracks().len();
                return self.edit(&Edit::AddTrack {
                    index: tracks,
                    kind: TrackKind::Video,
                    name: format!("Video {}", tracks + 1),
                });
            }
            Message::ToggleMute(track) => {
                let muted = self.history.timeline().track(track).map(|track| !track.muted);
                if let Some(muted) = muted {
                    return self.sound_edit(&Edit::SetMuted { track, muted });
                }
            }
            Message::VolumeChanged(volume) => {
                // Shown on the clip while dragging; heard once let go.
                if let Some(clip) = self.selected {
                    let mut timeline = self.history.timeline().clone();
                    if timeline.apply(&Edit::SetVolume { clip, volume }).is_ok() {
                        self.volume_drag = Some(volume);
                        self.preview = Some(Preview {
                            timeline,
                            look_at: None,
                        });
                    }
                }
            }
            Message::VolumeReleased => {
                self.preview = None;
                if let (Some(clip), Some(volume)) = (self.selected, self.volume_drag.take()) {
                    return self.sound_edit(&Edit::SetVolume { clip, volume });
                }
            }
            Message::StepInside(clip) => return self.step_inside(clip),
            Message::StepOut(level) => return self.step_out(level),
            Message::SteppedOut(step, result) => match result {
                Ok((projects, saved)) => {
                    if !saved.is_empty() {
                        self.notice = Some(format!("Saved {}", saved.join(", ")));
                    }
                    if step == self.nest_step {
                        let folder = self.project_folder.clone().unwrap_or_default();
                        self.history.adopt_projects(&projects, &folder);
                        if let Some(unchanged) = &mut self.unchanged {
                            unchanged.adopt_projects(&projects, &folder);
                        }
                        return self.timeline_changed();
                    }
                }
                Err(error) => self.notice = Some(error),
            },
            Message::NestedLoaded(path, loaded) => {
                self.opening_project = false;
                if self.adding.front() == Some(&path) {
                    self.adding.pop_front();
                }
                match loaded {
                    Ok(nested) => self.place_project_at_end(&path, nested),
                    Err(error) => self.notice = Some(error),
                }
                return self.add_next();
            }
            Message::WaveformRead(target, waveform) => {
                self.reading_waveform = false;
                if let Some(waveform) = waveform {
                    // Shown once `read_waveforms` has gone over the timeline again.
                    self.waveform_files.insert(target, waveform);
                }
                return self.next_waveform();
            }
            Message::Key(keyboard::Event::KeyPressed {
                key: pressed,
                modifiers,
                ..
            }) => {
                // With Shift held a letter arrives as its capital.
                let letter = |letter: &str| {
                    matches!(pressed.as_ref(), keyboard::Key::Character(character)
                        if modifiers.command() && character.eq_ignore_ascii_case(letter))
                };
                let named = match pressed.as_ref() {
                    keyboard::Key::Named(named) => Some(named),
                    _ => None,
                };
                let meant = match named {
                    Some(key::Named::ArrowLeft) => Some(Message::Stepped(-1)),
                    Some(key::Named::ArrowRight) => Some(Message::Stepped(1)),
                    Some(key::Named::Home) => Some(Message::Timeline(Action::Scrub(0))),
                    Some(key::Named::End) => {
                        Some(Message::Timeline(Action::Scrub(self.last_frame())))
                    }
                    Some(key::Named::Delete | key::Named::Backspace) => Some(Message::Delete),
                    Some(key::Named::Space) => Some(Message::PlayToggled),
                    Some(key::Named::Escape) if self.export_dialog => Some(Message::ExportClosed),
                    _ if letter("z") && !modifiers.shift() => Some(Message::Undo),
                    _ if letter("z") || letter("y") => Some(Message::Redo),
                    _ if letter("b") => Some(Message::Split),
                    _ if letter("s") => Some(Message::SaveRequested),
                    _ if letter("o") => Some(Message::OpenRequested),
                    _ => None,
                };
                if let Some(message) = meant {
                    return self.update(message);
                }
            }
            Message::Key(_) => {}
            Message::MemoryTick => {
                self.memory_mb =
                    memory_stats::memory_stats().map(|usage| usage.physical_mem / BYTES_PER_MB);
            }
        }
        Task::none()
    }

    fn media_event(&mut self, generation: u64, event: Media) -> Task<Message> {
        let Some(source) = self
            .sources
            .iter_mut()
            .find(|source| source.generation == generation)
        else {
            // The source was closed since.
            return Task::none();
        };
        match event {
            Media::Opened(info) => {
                source.info = Some(info.clone());
                let target = source.target.clone();
                self.picture_sizes.insert(target, (info.width, info.height));
                if self.opening == Some(generation) {
                    self.opening = None;
                    if let Some(path) = self.adding.pop_front() {
                        self.place_at_end(&path, &info);
                    }
                    return self.add_next();
                }
            }
            Media::Picture { picture, surface } => {
                // While playing, pictures are told apart by the request they answer, as the
                // clip before a cut and the one after it are both at work around it.
                if self.active != Some(generation) && self.playback.is_none() {
                    return Task::none();
                }
                self.arrivals += 1;
                let order = self.arrivals;
                match surface {
                    // The shader uploads the planes in the frame that first draws them.
                    Surface::Yuv(planes) => {
                        self.show(generation, order, picture, Displayed::Yuv(planes));
                    }
                    // iced uploads images in the background; swapping to a picture before its
                    // upload finishes would flash the empty viewer.
                    Surface::Image(handle) => {
                        return allocate(handle).map(move |allocation| Message::Uploaded {
                            generation,
                            order,
                            picture,
                            allocation: allocation.ok(),
                        });
                    }
                }
            }
            Media::Failed(error) => {
                self.notice = Some(format!("{}: {error}", source.name));
                // A file that does not open is left off the timeline; the rest still go on.
                if self.opening == Some(generation) {
                    self.opening = None;
                    self.adding.pop_front();
                    self.sources.retain(|source| source.generation != generation);
                    return self.add_next();
                }
            }
        }
        Task::none()
    }

    fn show(&mut self, generation: u64, order: u64, info: PictureInfo, picture: Displayed) {
        if let Some(playback) = &mut self.playback {
            // While playing, a picture waits for the clock to reach its frame. Keyframes on
            // the way to a frame, and pictures nobody asked for while playing, are left out.
            let asked = playback
                .requests
                .iter()
                .find(|(source, request, _)| (*source, *request) == (generation, info.request));
            if let Some((_, _, frame)) = asked.filter(|_| info.exact) {
                let shown = Shown {
                    picture,
                    order,
                    exact: true,
                    hardware: info.hardware,
                    size: info.size,
                };
                let place = playback.ready.partition_point(|(at, _)| at <= frame);
                playback.ready.insert(place, (*frame, shown));
                if let Some(pending) = self.pending.take_if(|pending| {
                    pending.generation == generation && pending.request == info.request
                }) {
                    self.latency.first_picture = Some(pending.at.elapsed());
                    self.latency.exact = self.latency.first_picture;
                }
            }
            return;
        }
        // The playhead may have left the clip while the picture was on its way, and uploads
        // finish independently, so an older picture can complete after a newer one.
        if self.active != Some(generation)
            || self.shown.as_ref().is_some_and(|shown| shown.order > order)
        {
            return;
        }
        // Pictures for requests that were since replaced are still the freshest image we have, so
        // they are shown too; only the newest request is timed.
        if let Some(pending) = self.pending.as_mut().filter(|pending| {
            pending.generation == generation && pending.request == info.request
        }) {
            let elapsed = pending.at.elapsed();
            if !pending.seen {
                pending.seen = true;
                self.latency.first_picture = Some(elapsed);
                self.latency.exact = None;
            }
            if info.exact {
                self.latency.exact = Some(elapsed);
                self.pending = None;
            }
        }
        self.shown = Some(Shown {
            picture,
            order,
            exact: info.exact,
            hardware: info.hardware,
            size: info.size,
        });
    }

    fn last_frame(&self) -> i64 {
        (self.history.timeline().duration() - 1).max(0)
    }

    /// Makes an edit the user asked for; it can be undone.
    fn edit(&mut self, edit: &Edit) -> Task<Message> {
        match self.history.apply(edit) {
            Ok(_) => self.notice = None,
            Err(error) => self.notice = Some(error.to_string()),
        }
        self.timeline_changed()
    }

    /// The cut at the playhead: through the selected clip when the playhead is on it, otherwise
    /// through the topmost clip there. None when the playhead is on no clip, or on a cut.
    fn split_edit(&self) -> Option<Edit> {
        let timeline = self.history.timeline();
        let cuts = |track: usize| {
            let (_, offset) = timeline.track(track)?.clip_at(self.playhead)?;
            (offset > 0).then_some(track)
        };
        let selected = self
            .selected
            .and_then(|id| timeline.clip(id))
            .filter(|(_, clip, start)| (start + 1..start + clip.duration).contains(&self.playhead))
            .map(|(track, ..)| track);
        let track = selected.or_else(|| (0..timeline.tracks().len()).rev().find_map(cuts))?;
        Some(Edit::Split {
            track,
            at: self.playhead,
        })
    }

    /// Taking the selected clip away. On the main track the clips after it close up.
    fn delete_edit(&self) -> Option<Edit> {
        let (track, clip, _) = self.history.timeline().clip(self.selected?)?;
        Some(Edit::Remove {
            clip: clip.id,
            ripple: track == MAIN_TRACK,
        })
    }

    /// How much later than the sound card reports the sound is heard, in milliseconds, when
    /// that is known for the output in use.
    fn output_delay(&self) -> Option<u64> {
        match self.sound.status() {
            SoundStatus::Ready { device, .. } => self
                .sound_delay
                .or_else(|| self.settings.sound_delays.get(&device).copied()),
            _ => None,
        }
    }

    /// Ends playback, sound and all.
    fn stop_playing(&mut self) {
        self.playback = None;
        self.sound.stop();
    }

    /// Puts the playhead where the user wants it, which ends playback.
    fn move_playhead(&mut self, frame: i64) -> Task<Message> {
        self.stop_playing();
        let frame = frame.clamp(0, self.last_frame());
        if frame == self.playhead {
            return Task::none();
        }
        self.playhead = frame;
        self.show_playhead()
    }

    /// Starts making the timeline into a film at `path`, in the background.
    fn export(&mut self, path: PathBuf) -> Task<Message> {
        if self.exporting.is_some() {
            return Task::none();
        }
        let (sender, events) = mpsc::unbounded();
        // Tracks are shared, so the copy the export works from is cheap; editing goes on
        // without changing the film being made.
        let export = Export::start(
            self.history.timeline().clone(),
            self.project_folder.clone(),
            path,
            self.settings.export,
            move |event| {
                let _ = sender.unbounded_send(event);
            },
        );
        self.exporting = Some(Exporting {
            export,
            started: Instant::now(),
            done: 0,
            total: self.history.timeline().duration(),
            encoder: None,
        });
        Task::run(events, Message::Exported)
    }

    /// The media file a film takes its shape from: that of the first clip on the main track,
    /// or else the first anywhere, looking into nested projects. The export works it out the
    /// same way.
    fn film_source(&self) -> Option<(String, PathBuf)> {
        self.location(&self.history.timeline().first_shot()?)
    }

    /// Reads the picture size of the media a film takes its shape from, in the background,
    /// unless it is known already.
    fn read_film_size(&mut self) -> Task<Message> {
        let Some((target, path)) = self.film_source() else {
            return Task::none();
        };
        if self.picture_sizes.contains_key(&target) || !self.sizing.insert(target.clone()) {
            return Task::none();
        }
        let (sender, read) = oneshot::channel();
        let started = std::thread::Builder::new()
            .name("cannotcut-probe".to_string())
            .spawn(move || {
                let _ = sender.send(picture_size(&path).ok());
            });
        if started.is_err() {
            return Task::none();
        }
        Task::perform(async move { read.await.ok().flatten() }, move |size| {
            Message::PictureSized(target.clone(), size)
        })
    }

    fn export_event(&mut self, event: ExportEvent) {
        let Some(exporting) = &mut self.exporting else {
            return;
        };
        match event {
            ExportEvent::Started { encoder, .. } => exporting.encoder = Some(encoder),
            ExportEvent::Progress { done, total } => {
                exporting.done = done;
                exporting.total = total;
            }
            ExportEvent::Finished(result) => {
                self.notice = Some(match result {
                    Ok(summary) => format!(
                        "Exported {} in {:.0} s",
                        summary
                            .path
                            .file_name()
                            .map_or(summary.path.display().to_string(), |name| name
                                .to_string_lossy()
                                .into_owned()),
                        summary.took.as_secs_f64()
                    ),
                    Err(error) if error == "cancelled" => "Export cancelled".to_string(),
                    Err(error) => format!("Export failed: {error}"),
                });
                self.exporting = None;
            }
        }
    }

    /// Makes an edit that changes only what is heard. Playback carries on: the sound is
    /// picked up again from the playhead, as the edit leaves it.
    fn sound_edit(&mut self, edit: &Edit) -> Task<Message> {
        match self.history.apply(edit) {
            Ok(_) => self.notice = None,
            Err(error) => {
                self.notice = Some(error.to_string());
                return Task::none();
            }
        }
        if let Some(playback) = &mut self.playback
            && let Clock::Sound { .. } = playback.clock
        {
            let timeline = self.history.timeline().clone();
            let folder = self.project_folder.clone();
            if !self.sound.replace(timeline.clone(), folder.clone()) {
                // No sound heard yet to carry on from.
                self.sound.play(timeline, self.playhead, folder);
                playback.clock = Clock::Sound {
                    asked_at: Instant::now(),
                };
            }
        }
        Task::none()
    }

    /// The timeline was edited: what is under the playhead may be something else now.
    fn timeline_changed(&mut self) -> Task<Message> {
        self.stop_playing();
        let timeline = self.history.timeline();
        self.preview = None;
        self.selected = self.selected.filter(|id| timeline.clip(*id).is_some());
        self.playhead = self.playhead.clamp(0, self.last_frame());
        self.show_playhead()
    }

    /// Something the form of the pictures depends on has changed.
    fn viewer_changed(&mut self) -> Task<Message> {
        if self.form.is_none() {
            // Nothing is on screen yet, so there is no drag to wait out.
            self.adopt_form()
        } else {
            self.resized_at = Some(Instant::now());
            Task::none()
        }
    }

    /// Starts asking the decoders for pictures that suit the viewer as it is now.
    fn adopt_form(&mut self) -> Task<Message> {
        let (Some(path), Some(area), Some(scale)) =
            (self.viewer, self.viewer_area, self.scale_factor)
        else {
            return Task::none();
        };
        let form = Form {
            output: path.output(),
            max_size: (
                (area.width * scale).ceil() as u32,
                (area.height * scale).ceil() as u32,
            ),
        };
        if self.form == Some(form) {
            return Task::none();
        }
        self.form = Some(form);
        self.show_playhead()
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = vec![
            keyboard::listen().map(Message::Key),
            time::every(MEMORY_REFRESH).map(|_| Message::MemoryTick),
            iced::event::listen_with(|event, _status, _window| match event {
                iced::Event::Window(window::Event::Rescaled(scale_factor)) => {
                    Some(Message::Rescaled(scale_factor))
                }
                _ => None,
            }),
        ];
        if self.resized_at.is_some() {
            subscriptions.push(time::every(RESIZE_POLL).map(|_| Message::ResizeTick));
        }
        if self.playback.is_some() {
            // One tick for every frame the display shows.
            subscriptions.push(window::frames().map(|_| Message::PlayTick));
        }
        Subscription::batch(subscriptions)
    }

    fn view(&self) -> Element<'_, Message> {
        let timeline = self.history.timeline();
        let mut header = row![
            button("Add clips").on_press(Message::AddRequested),
            button("Open").on_press(Message::OpenRequested),
            button("Save").on_press(Message::SaveRequested),
            self.export_control(),
            // Up here, not with the editing buttons: on the GPD's screen at 225% those already
            // fill the row while a clip is selected.
            self.step_inside_control(),
        ]
        .spacing(12)
        .align_y(Center);
        if !self.parents.is_empty() {
            header = header.push(self.project_path_bar());
        }
        let header = header.push(one_line(self.headline()).width(Fill));

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
        let viewer = container(picture)
            .width(Fill)
            .height(Fill)
            .style(|_| container::background(Color::BLACK));
        let mut viewer: Element<'_, Message> =
            sensor(viewer).on_resize(Message::ViewerResized).into();
        if self.viewer.is_none() {
            viewer = stack![viewer, renderer_probe(Message::RendererProbed)].into();
        }
        if self.export_dialog {
            // Over the viewer, so that nothing around it moves.
            viewer = stack![viewer, center(self.export_dialog())].into();
        }

        // A button that cannot do anything right now shows it by being greyed out.
        let position = format!(
            "{}   frame {} of {}",
            timecode(self.playhead, timeline.rate()),
            self.playhead,
            self.last_frame()
        );
        let toolbar = row![
            button("Undo").on_press_maybe(self.history.undo_label().map(|_| Message::Undo)),
            button("Redo").on_press_maybe(self.history.redo_label().map(|_| Message::Redo)),
            button("Split").on_press_maybe(self.split_edit().map(|_| Message::Split)),
            button("Delete").on_press_maybe(self.delete_edit().map(|_| Message::Delete)),
            button("Add track").on_press(Message::AddTrack),
            self.volume_control(),
            one_line(position).center_x(Fill),
            button(
                text(if self.playback.is_some() { "Pause" } else { "Play" })
                    .width(PLAY_LABEL_WIDTH)
                    .align_x(Center)
            )
            .on_press_maybe((timeline.duration() > 0).then_some(Message::PlayToggled)),
            button("Previous frame").on_press(Message::Stepped(-1)),
            button("Next frame").on_press(Message::Stepped(1)),
        ]
        .spacing(12)
        .align_y(Center);

        let shown = self
            .preview
            .as_ref()
            .map_or(timeline, |preview| &preview.timeline);
        let panel = timeline_panel(
            timeline,
            shown,
            self.playhead,
            self.timeline_view,
            self.selected,
            &self.waveforms,
            Message::Timeline,
        );
        let panel = row![self.track_headers(), panel].spacing(8);
        let status = container(
            text(self.readout())
                .size(STATUS_TEXT_SIZE)
                .wrapping(text::Wrapping::None),
        )
        .clip(true)
        .align_right(Fill);

        column![header, viewer, toolbar, panel, status]
            .spacing(10)
            .padding(12)
            .into()
    }

    /// Where the project shown sits: the projects stepped out of, each a button back, and its
    /// own name.
    fn project_path_bar(&self) -> Element<'_, Message> {
        let mut bar = row![].spacing(4).align_y(Center);
        for (level, parent) in self.parents.iter().enumerate() {
            let name = project_name(parent.path.as_deref(), parent.history.timeline());
            bar = bar
                .push(
                    button(text(name))
                        .style(button::text)
                        .on_press(Message::StepOut(level)),
                )
                .push(text(">"));
        }
        let own = project_name(self.project_path.as_deref(), self.history.timeline());
        bar.push(text(own).wrapping(text::Wrapping::None)).into()
    }

    /// A way into the selected clip's project, for when double-clicking is not at hand.
    fn step_inside_control(&self) -> Element<'_, Message> {
        let nested = self.selected.filter(|clip| {
            let found = self.history.timeline().clip(*clip);
            found.is_some_and(|(_, clip, _)| clip.media.project.is_some())
        });
        match nested {
            Some(clip) => button("Step inside").on_press(Message::StepInside(clip)).into(),
            None => row![].into(),
        }
    }

    /// The export choices: how large and how good, what that makes, and the way on.
    fn export_dialog(&self) -> Element<'_, Message> {
        let options = self.settings.export;
        let timeline = self.history.timeline();
        let rate = timeline.rate();
        let source = self
            .film_source()
            .and_then(|(target, _)| self.picture_sizes.get(&target))
            .copied();
        let about = match source {
            Some(source) => {
                let (width, height) = options.resolution.film_size(source);
                let bytes = options.estimated_bytes(source, rate, timeline.duration());
                let fps = if rate.den == 1 {
                    rate.num.to_string()
                } else {
                    format!("{:.2}", rate.as_f64())
                };
                format!(
                    "{width}x{height} at {fps} fps, up to about {:.0} MB",
                    (bytes as f64 / BYTES_PER_MEGABYTE).ceil()
                )
            }
            None => "Working out the size".to_string(),
        };
        let choice = |label: &'static str, list: Element<'static, Message>| {
            row![text(label).width(DIALOG_LABEL_WIDTH), list]
                .spacing(12)
                .align_y(Center)
        };
        let card = column![
            text("Export").size(DIALOG_TITLE_SIZE),
            choice(
                "Resolution",
                pick_list(
                    &Resolution::CHOICES[..],
                    Some(options.resolution),
                    Message::ExportResolution
                )
                .into()
            ),
            choice(
                "Quality",
                pick_list(&Quality::CHOICES[..], Some(options.quality), Message::ExportQuality)
                    .into()
            ),
            text(about).size(STATUS_TEXT_SIZE),
            row![
                button("Cancel")
                    .on_press(Message::ExportClosed)
                    .style(button::secondary),
                button("Export").on_press(Message::ExportRequested),
            ]
            .spacing(12),
        ]
        .spacing(16);
        container(card)
            .padding(20)
            .width(DIALOG_WIDTH)
            .style(container::rounded_box)
            .into()
    }

    /// Export, or how the export under way is getting on and a way to stop it.
    fn export_control(&self) -> Element<'_, Message> {
        let Some(exporting) = &self.exporting else {
            let has_clips = self.history.timeline().duration() > 0;
            return button("Export")
                .on_press_maybe(has_clips.then_some(Message::ExportOpened))
                .into();
        };
        let share = exporting.done as f32 / exporting.total.max(1) as f32;
        let elapsed = exporting.started.elapsed().as_secs_f64();
        let left = match exporting.done {
            0 => String::new(),
            done => {
                let rest = (exporting.total - done) as f64 * elapsed / done as f64;
                format!(", {rest:.0} s left")
            }
        };
        let encoder = exporting
            .encoder
            .map_or(String::new(), |encoder| format!(" with {encoder}"));
        row![
            button("Cancel export").on_press(Message::ExportCancelled),
            progress_bar(0.0..=1.0, share)
                .length(EXPORT_BAR_WIDTH)
                .girth(EXPORT_BAR_HEIGHT),
            one_line(format!("Exporting {:.0}%{left}{encoder}", share * 100.0)),
        ]
        .spacing(8)
        .align_y(Center)
        .into()
    }

    /// A button beside each lane of the timeline panel to mute its track, lined up with it.
    fn track_headers(&self) -> Element<'_, Message> {
        let tracks = self.history.timeline().tracks();
        // The top of the stack is the top lane.
        let buttons = tracks.enumerate().rev().map(|(index, track)| {
            let label = if track.muted { "Muted" } else { "Mute" };
            let style = if track.muted {
                button::danger
            } else {
                button::secondary
            };
            let toggle = button(text(label).size(HEADER_TEXT_SIZE))
                .on_press(Message::ToggleMute(index))
                .style(style)
                .padding([4, 10]);
            container(toggle).height(LANE_HEIGHT).center_y(LANE_HEIGHT).into()
        });
        let column = Column::with_children(buttons)
            .spacing(LANE_GAP)
            .padding(iced::Padding {
                top: RULER_HEIGHT + LANE_GAP,
                ..iced::Padding::ZERO
            });
        container(column)
            .width(HEADER_WIDTH)
            .height(PANEL_HEIGHT)
            .clip(true)
            .into()
    }

    /// The selected clip's volume, to drag; nothing when no clip is selected.
    fn volume_control(&self) -> Element<'_, Message> {
        let Some((_, clip, _)) = self.selected.and_then(|id| self.history.timeline().clip(id))
        else {
            return row![].into();
        };
        let volume = self.volume_drag.unwrap_or(clip.volume).clamp(0.0, MAX_VOLUME);
        row![
            text("Volume"),
            slider(0.0..=MAX_VOLUME, volume, Message::VolumeChanged)
                .step(0.01_f32)
                .on_release(Message::VolumeReleased)
                .width(VOLUME_SLIDER_WIDTH),
            text(format!("{:.0}%", volume * 100.0)).width(VOLUME_LABEL_WIDTH),
        ]
        .spacing(8)
        .align_y(Center)
        .into()
    }

    /// The line next to the add button: what went wrong, or what the playhead is on.
    fn headline(&self) -> String {
        if let Some(notice) = &self.notice {
            return notice.clone();
        }
        let source = self
            .sources
            .iter()
            .find(|source| Some(source.generation) == self.active.or(self.opening));
        match source {
            Some(Source {
                name,
                info: Some(info),
                ..
            }) => format!(
                "{name}   {} {}x{} at {} fps, {}",
                info.codec,
                info.width,
                info.height,
                (f64::from(info.frame_rate.num) / f64::from(info.frame_rate.den.max(1))).round(),
                match info.hardware {
                    Some(decoder) => format!("{decoder} decode"),
                    None => "software decode".to_string(),
                }
            ),
            Some(source) => format!("Opening {}", source.name),
            None => String::new(),
        }
    }

    fn readout(&self) -> String {
        let milliseconds = |latency: Option<Duration>| match latency {
            Some(latency) => format!("{} ms", latency.as_millis()),
            None => "...".to_string(),
        };
        let mut parts = vec![
            format!("first {}", milliseconds(self.latency.first_picture)),
            format!("exact {}", milliseconds(self.latency.exact)),
        ];
        if let Some(shown) = &self.shown {
            if !shown.exact {
                parts.push("keyframe".to_string());
            }
            parts.push(if shown.hardware { "GPU decode" } else { "CPU decode" }.to_string());
        }
        let path = match self.viewer {
            Some(ViewerPath::Shader) => "shader viewer",
            Some(ViewerPath::Cpu) => "CPU viewer",
            None => "viewer",
        };
        match &self.shown {
            Some(Shown {
                size: (width, height),
                ..
            }) => parts.push(format!("{path} {width}x{height}")),
            None => parts.push(path.to_string()),
        }
        if let Some(stats) = self.play_stats.filter(|stats| !stats.stepping.is_zero()) {
            parts.push(format!(
                "played {:.0} fps, longest gap {} ms",
                f64::from(stats.steps) / stats.stepping.as_secs_f64(),
                stats.longest_gap.as_millis()
            ));
        }
        parts.push(match self.sound.status() {
            SoundStatus::Ready { rate, .. } => {
                let delay = match self.output_delay() {
                    Some(delay) => format!(", {delay} ms late"),
                    None => String::new(),
                };
                match self.sound.problem() {
                    Some(problem) => format!("sound {} kHz{delay}: {problem}", rate / 1000),
                    None => format!("sound {} kHz{delay}", rate / 1000),
                }
            }
            SoundStatus::Starting => "sound starting".to_string(),
            SoundStatus::Unavailable(why) => format!("no sound: {why}"),
        });
        parts.push(format!("{} open", self.sources.len()));
        if let Some(memory_mb) = self.memory_mb {
            parts.push(format!("{memory_mb} MB"));
        }
        parts.join("   ")
    }
}

/// Text that is cut off instead of wrapped when it runs out of room. The rows around the viewer
/// must keep their height whatever they say: a row that grows squeezes the viewer, and the
/// picture with it.
fn one_line<'a>(content: String) -> container::Container<'a, Message> {
    container(text(content).wrapping(text::Wrapping::None)).clip(true)
}

async fn pick_project() -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .set_title("Open a project")
        .add_filter("Project", &[PROJECT_EXTENSION])
        .pick_file()
        .await
        .map(|file| file.path().to_path_buf())
}

async fn pick_film_path(name: String) -> Option<PathBuf> {
    let chosen = rfd::AsyncFileDialog::new()
        .set_title("Export a film")
        .add_filter("Film", &[FILM_EXTENSION])
        .set_file_name(format!("{name}.{FILM_EXTENSION}"))
        .save_file()
        .await?;
    Some(chosen.path().with_extension(FILM_EXTENSION))
}

async fn pick_save_path(name: String) -> Option<PathBuf> {
    let chosen = rfd::AsyncFileDialog::new()
        .set_title("Save the project")
        .add_filter("Project", &[PROJECT_EXTENSION])
        .set_file_name(format!("{name}.{PROJECT_EXTENSION}"))
        .save_file()
        .await?;
    Some(chosen.path().with_extension(PROJECT_EXTENSION))
}

async fn load_project(path: PathBuf) -> Result<(PathBuf, Timeline), String> {
    match Timeline::load(&path) {
        Ok(timeline) => Ok((path, timeline)),
        Err(error) => Err(error.to_string()),
    }
}

async fn pick_clips() -> Vec<PathBuf> {
    let both: Vec<&str> = VIDEO_EXTENSIONS
        .iter()
        .copied()
        .chain([PROJECT_EXTENSION])
        .collect();
    rfd::AsyncFileDialog::new()
        .set_title("Add video clips or projects")
        .add_filter("Video or project", &both)
        .add_filter("Video", VIDEO_EXTENSIONS)
        .add_filter("Project", &[PROJECT_EXTENSION])
        .pick_files()
        .await
        .unwrap_or_default()
        .iter()
        .map(|file| file.path().to_path_buf())
        .collect()
}

/// What a project is called on the path in the header: its file's name, or the name in it
/// while it has no file.
fn project_name(path: Option<&Path>, timeline: &Timeline) -> String {
    match path.and_then(Path::file_stem) {
        Some(stem) => stem.to_string_lossy().into_owned(),
        None if timeline.name.is_empty() => "Untitled".to_string(),
        None => timeline.name.clone(),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into())
}

fn is_project(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case(PROJECT_EXTENSION))
}

/// What open media files, their picture sizes and decoders are known by: the full path. Two
/// nested projects can each have an `a.mp4` of their own, so names in a project do not do.
fn file_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Non-drop-frame timecode.
fn timecode(frame: i64, rate: Rate) -> String {
    let base = timecode_base(rate);
    let seconds = frame / base;
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60,
        frame % base
    )
}
