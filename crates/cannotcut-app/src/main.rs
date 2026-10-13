//! cannotcut: a timeline of clips that can be rearranged, trimmed and cut, a viewer showing the
//! frame under the playhead, and live latency and memory readouts.

mod timeline_panel;
mod viewer;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cannotcut_media::{ClipInfo, Event, Options, Output, Pixels, Scrubber};
use cannotcut_timeline::{
    ClipId, Edge, Edit, History, Media as TimelineMedia, NewClip, Placement, Rate, Timeline,
    TrackKind,
};
use iced::futures::channel::mpsc;
use iced::keyboard::{self, key};
use iced::widget::{button, center, column, container, image, row, sensor, shader, stack, text};
use iced::{
    Center, Color, ContentFit, Element, Fill, Size, Subscription, Task, Theme, time, window,
};
use iced_runtime::image::{Allocation, allocate};
use timeline_panel::{Action, MAIN_TRACK, View, timecode_base, timeline_panel};
use viewer::{YuvPicture, renderer_probe};

/// Keeps the viewer off the shader path, to try the fallback on a machine with a working GPU.
const CPU_VIEWER_FLAG: &str = "--cpu-viewer";
/// Starts with the playhead on a frame, to look at a spot without touching the mouse.
const PLAYHEAD_FLAG: &str = "--at=";
const PROJECT_EXTENSION: &str = "otio";
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
    Undo,
    Redo,
    /// Cut a clip in two at the playhead.
    Split,
    /// Take the selected clip off the timeline.
    Delete,
    AddTrack,
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
    /// The source of the clip under the playhead; pictures from any other are out of date.
    active: Option<u64>,
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
}

/// An open media file: a decode thread that makes pictures of its frames.
struct Source {
    generation: u64,
    /// The file's location as the clips on the timeline give it.
    target: String,
    name: String,
    scrubber: Scrubber,
    /// Unknown until the decode thread has opened the file.
    info: Option<ClipInfo>,
    /// The form last asked of the decoder.
    form: Option<Form>,
}

impl Source {
    fn request(&mut self, frame: u64, form: Form) -> u64 {
        if self.form != Some(form) {
            self.form = Some(form);
            self.scrubber.set_output(form.output);
            self.scrubber.set_max_preview(form.max_size);
        }
        self.scrubber.request(frame)
    }
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
            active: None,
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
        };
        let mut playhead = 0;
        for argument in std::env::args_os().skip(1) {
            let path = PathBuf::from(&argument);
            let text = argument.to_string_lossy();
            if text == CPU_VIEWER_FLAG {
                app.viewer = Some(ViewerPath::Cpu);
            } else if let Some(frame) = text.strip_prefix(PLAYHEAD_FLAG) {
                playhead = frame.parse().unwrap_or(0);
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
        let task = app.add_next();
        (app, task)
    }

    /// Replaces what is being edited with a project read from `path`.
    fn adopt_project(&mut self, path: PathBuf, timeline: Timeline) -> Task<Message> {
        self.history = History::new(timeline);
        self.project_folder = path.parent().map(Path::to_path_buf);
        self.project_path = Some(path);
        self.selected = None;
        self.preview = None;
        self.playhead = 0;
        // The old project's files are closed; the new one's open as the playhead reaches them.
        self.sources.clear();
        self.adding.clear();
        self.opening = None;
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
        if self.opening.is_some() {
            return Task::none();
        }
        while let Some(path) = self.adding.front().cloned() {
            let target = TimelineMedia::target_for(&path);
            let (index, task) = match self.source(&target, &path) {
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

    /// Puts all of a media file on the main track, after what is there.
    fn place_at_end(&mut self, path: &Path, info: &ClipInfo) {
        let media_rate = Rate::new(
            info.frame_rate.num.max(1) as u32,
            info.frame_rate.den.max(1) as u32,
        );
        let media = TimelineMedia::from_path(path, media_rate, info.frame_count as i64);
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

    /// The open source for a media file, opening it if need be. Returns its place in
    /// `sources` and the task that brings in its events.
    fn source(&mut self, target: &str, path: &Path) -> Result<(usize, Task<Message>), String> {
        let mut task = Task::none();
        match self.sources.iter().position(|source| source.target == target) {
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
                });
                task = Task::run(events, move |event| Message::Media(generation, event.into()));
                // Closing a source ends its decode thread and frees its pictures.
                while self.sources.len() > MAX_OPEN_SOURCES {
                    let idle = self.sources.iter().position(|source| {
                        ![self.active, self.opening, Some(generation)]
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
    /// the timeline as that edit would leave it.
    fn show_playhead(&mut self) -> Task<Message> {
        let Some(form) = self.form else {
            // The first picture waits until the viewer knows what it wants.
            return Task::none();
        };
        let (timeline, at) = match &self.preview {
            Some(preview) => (
                &preview.timeline,
                preview.look_at.unwrap_or(self.playhead),
            ),
            None => (self.history.timeline(), self.playhead),
        };
        let Some((clip, offset)) = timeline.picture_at(at) else {
            // Nothing plays here: the viewer goes black.
            self.active = None;
            self.pending = None;
            self.shown = None;
            return Task::none();
        };
        let frame = clip.media_frame(offset, timeline.rate()).max(0) as u64;
        let (Some(target), Some(path)) = (clip.media.target.clone(), clip.media.path()) else {
            self.notice = Some(format!("the media of \"{}\" is missing", clip.name));
            self.active = None;
            self.shown = None;
            return Task::none();
        };
        let path = match &self.project_folder {
            Some(folder) if path.is_relative() => folder.join(path),
            _ => path,
        };
        match self.source(&target, &path) {
            Ok((index, task)) => {
                let source = &mut self.sources[index];
                self.active = Some(source.generation);
                self.pending = Some(Pending {
                    generation: source.generation,
                    request: source.request(frame, form),
                    at: Instant::now(),
                    seen: false,
                });
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

    fn update(&mut self, message: Message) -> Task<Message> {
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
            Message::Saved(Ok(path)) => {
                self.notice = Some(format!("Saved {}", path.display()));
                self.project_folder = path.parent().map(Path::to_path_buf);
                self.project_path = Some(path);
            }
            Message::ProjectLoaded(Err(error)) | Message::Saved(Err(error)) => {
                self.notice = Some(error);
            }
            Message::ProjectChosen(None) | Message::SaveChosen(None) => {}
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
                Action::Cancel => {
                    if self.preview.take().is_some() {
                        return self.show_playhead();
                    }
                }
            },
            Message::Stepped(frames) => {
                return self.move_playhead(self.playhead.saturating_add(frames));
            }
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
                if self.opening == Some(generation) {
                    self.opening = None;
                    if let Some(path) = self.adding.pop_front() {
                        self.place_at_end(&path, &info);
                    }
                    return self.add_next();
                }
            }
            Media::Picture { picture, surface } => {
                if self.active != Some(generation) {
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

    fn move_playhead(&mut self, frame: i64) -> Task<Message> {
        let frame = frame.clamp(0, self.last_frame());
        if frame == self.playhead {
            return Task::none();
        }
        self.playhead = frame;
        self.show_playhead()
    }

    /// The timeline was edited: what is under the playhead may be something else now.
    fn timeline_changed(&mut self) -> Task<Message> {
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
        Subscription::batch(subscriptions)
    }

    fn view(&self) -> Element<'_, Message> {
        let timeline = self.history.timeline();
        let header = row![
            button("Add clips").on_press(Message::AddRequested),
            button("Open").on_press(Message::OpenRequested),
            button("Save").on_press(Message::SaveRequested),
            one_line(self.headline()).width(Fill),
        ]
        .spacing(12)
        .align_y(Center);

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
            one_line(position).center_x(Fill),
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
            Message::Timeline,
        );
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
    rfd::AsyncFileDialog::new()
        .set_title("Add video clips")
        .add_filter("Video", VIDEO_EXTENSIONS)
        .pick_files()
        .await
        .unwrap_or_default()
        .iter()
        .map(|file| file.path().to_path_buf())
        .collect()
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
