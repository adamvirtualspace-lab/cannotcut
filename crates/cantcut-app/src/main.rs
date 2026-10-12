//! cantcut scrubbing spike: one clip, a viewer, a scrub bar, and live latency and memory readouts.

mod viewer;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use cantcut_media::{ClipInfo, Event, Options, Output, Pixels, Ratio, Scrubber};
use iced::futures::channel::mpsc;
use iced::keyboard::{self, key};
use iced::widget::{
    button, center, column, container, image, row, sensor, shader, slider, stack, text,
};
use iced::{
    Center, Color, ContentFit, Element, Fill, Size, Subscription, Task, Theme, time, window,
};
use iced_runtime::image::{Allocation, allocate};
use viewer::{YuvPicture, renderer_probe};

/// Keeps the viewer off the shader path, to try the fallback on a machine with a working GPU.
const CPU_VIEWER_FLAG: &str = "--cpu-viewer";
const MEMORY_REFRESH: Duration = Duration::from_secs(1);
/// How long the viewer's size has to hold still before pictures are made for it. While a window
/// edge is being dragged the picture on screen is stretched instead, which costs nothing.
const RESIZE_SETTLE: Duration = Duration::from_millis(150);
const RESIZE_POLL: Duration = Duration::from_millis(50);
const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "mkv", "webm", "m4v", "avi", "ts"];
const BYTES_PER_MB: usize = 1024 * 1024;

fn main() -> iced::Result {
    iced::application(App::boot, App::update, App::view)
        .title("cantcut")
        .subscription(App::subscription)
        .theme(Theme::Dark)
        .window_size((1280.0, 820.0))
        .run()
}

#[derive(Clone)]
enum Message {
    OpenRequested,
    FileChosen(Option<PathBuf>),
    /// Whether the window is drawn on the GPU, which decides the viewer's path.
    RendererProbed(bool),
    /// The viewer's area was laid out at this size, in logical pixels.
    ViewerResized(Size),
    /// Physical pixels per logical pixel in the window.
    Rescaled(f32),
    ResizeTick,
    /// An event from the decode thread of the clip with this generation.
    Media(u64, Media),
    /// A picture finished uploading to the GPU (or failed to) and can be shown.
    Uploaded {
        generation: u64,
        order: u64,
        picture: PictureInfo,
        allocation: Option<Allocation>,
    },
    Scrubbed(u32),
    Stepped(i64),
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

#[derive(Default)]
struct App {
    clip: Option<Clip>,
    /// Unknown until the window's renderer has been probed; chosen once.
    viewer: Option<ViewerPath>,
    /// The viewer's area in logical pixels, as last laid out.
    viewer_area: Option<Size>,
    scale_factor: Option<f32>,
    /// When the viewer last changed while the pictures have not followed yet.
    resized_at: Option<Instant>,
    /// What the decoder is asked for; unknown until the viewer's path and size both are.
    form: Option<Form>,
    /// Counts opened clips so events from a clip that was since replaced can be told apart.
    generation: u64,
    notice: Option<String>,
    memory_mb: Option<usize>,
}

struct Clip {
    generation: u64,
    name: String,
    scrubber: Scrubber,
    /// Unknown until the decode thread has opened the file.
    info: Option<ClipInfo>,
    playhead: u64,
    /// The form last asked of the decoder.
    form: Option<Form>,
    pending: Option<Pending>,
    /// Pictures received from the decoder so far; gives each one its place in line.
    arrivals: u64,
    shown: Option<Shown>,
    latency: Latency,
}

/// The newest request, kept to time how long its pictures take to reach the UI.
struct Pending {
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
        let mut app = Self::default();
        let mut path = None;
        for argument in std::env::args_os().skip(1) {
            if argument == CPU_VIEWER_FLAG {
                app.viewer = Some(ViewerPath::Cpu);
            } else {
                path = Some(PathBuf::from(argument));
            }
        }
        let task = match path {
            Some(path) => app.open(path),
            None => Task::none(),
        };
        (app, task)
    }

    fn open(&mut self, path: PathBuf) -> Task<Message> {
        self.generation += 1;
        let generation = self.generation;
        let (sender, events) = mpsc::unbounded();
        let opened = Scrubber::open(&path, Options::default(), move |event| {
            let _ = sender.unbounded_send(event);
        });
        match opened {
            Ok(scrubber) => {
                let mut clip = Clip {
                    generation,
                    name: path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    scrubber,
                    info: None,
                    playhead: 0,
                    form: None,
                    pending: None,
                    arrivals: 0,
                    shown: None,
                    latency: Latency::default(),
                };
                // The file opens meanwhile, but the first picture waits until the viewer knows
                // what it wants.
                if let Some(form) = self.form {
                    clip.set_form(form);
                }
                self.clip = Some(clip);
                self.notice = None;
                Task::run(events, move |event| Message::Media(generation, event.into()))
            }
            Err(error) => {
                self.notice = Some(error.to_string());
                Task::none()
            }
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::OpenRequested => return Task::perform(pick_clip(), Message::FileChosen),
            Message::FileChosen(Some(path)) => return self.open(path),
            Message::FileChosen(None) => {}
            Message::RendererProbed(on_gpu) => {
                if self.viewer.is_none() {
                    self.viewer = Some(if on_gpu {
                        ViewerPath::Shader
                    } else {
                        ViewerPath::Cpu
                    });
                    self.viewer_changed();
                }
            }
            Message::ViewerResized(area) => {
                self.viewer_area = Some(area);
                if self.scale_factor.is_none() {
                    return window::latest()
                        .and_then(window::scale_factor)
                        .map(Message::Rescaled);
                }
                self.viewer_changed();
            }
            Message::Rescaled(scale_factor) => {
                self.scale_factor = Some(scale_factor);
                self.viewer_changed();
            }
            Message::ResizeTick => {
                if self
                    .resized_at
                    .is_some_and(|at| at.elapsed() >= RESIZE_SETTLE)
                {
                    self.resized_at = None;
                    self.adopt_form();
                }
            }
            Message::Media(generation, event) => {
                if let Some(clip) = self.clip.as_mut().filter(|clip| clip.generation == generation)
                {
                    match event {
                        Media::Opened(info) => clip.info = Some(info),
                        Media::Picture { picture, surface } => {
                            clip.arrivals += 1;
                            let order = clip.arrivals;
                            match surface {
                                // The shader uploads the planes in the frame that first draws
                                // them.
                                Surface::Yuv(planes) => {
                                    clip.show(order, picture, Displayed::Yuv(planes));
                                }
                                // iced uploads images in the background; swapping to a picture
                                // before its upload finishes would flash the empty viewer.
                                Surface::Image(handle) => {
                                    return allocate(handle).map(move |allocation| {
                                        Message::Uploaded {
                                            generation,
                                            order,
                                            picture,
                                            allocation: allocation.ok(),
                                        }
                                    });
                                }
                            }
                        }
                        Media::Failed(error) => self.notice = Some(error),
                    }
                }
            }
            Message::Uploaded {
                generation,
                order,
                picture,
                allocation,
            } => {
                if let Some(clip) = self.clip.as_mut().filter(|clip| clip.generation == generation)
                {
                    match allocation {
                        Some(allocation) => {
                            clip.show(order, picture, Displayed::Image(allocation));
                        }
                        None => {
                            self.notice = Some("could not upload a picture to the GPU".to_string());
                        }
                    }
                }
            }
            Message::Scrubbed(frame) => {
                if let Some(clip) = &mut self.clip {
                    clip.move_playhead(u64::from(frame));
                }
            }
            Message::Stepped(frames) => self.step(frames),
            Message::Key(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(named),
                ..
            }) => match named {
                key::Named::ArrowLeft => self.step(-1),
                key::Named::ArrowRight => self.step(1),
                _ => {}
            },
            Message::Key(_) => {}
            Message::MemoryTick => {
                self.memory_mb =
                    memory_stats::memory_stats().map(|usage| usage.physical_mem / BYTES_PER_MB);
            }
        }
        Task::none()
    }

    /// Something the form of the pictures depends on has changed.
    fn viewer_changed(&mut self) {
        if self.form.is_none() {
            // Nothing is on screen yet, so there is no drag to wait out.
            self.adopt_form();
        } else {
            self.resized_at = Some(Instant::now());
        }
    }

    /// Starts asking the decoder for pictures that suit the viewer as it is now.
    fn adopt_form(&mut self) {
        let (Some(path), Some(area), Some(scale)) =
            (self.viewer, self.viewer_area, self.scale_factor)
        else {
            return;
        };
        let form = Form {
            output: path.output(),
            max_size: (
                (area.width * scale).ceil() as u32,
                (area.height * scale).ceil() as u32,
            ),
        };
        self.form = Some(form);
        if let Some(clip) = &mut self.clip {
            clip.set_form(form);
        }
    }

    fn step(&mut self, frames: i64) {
        if let Some(clip) = &mut self.clip {
            clip.move_playhead(clip.playhead.saturating_add_signed(frames));
        }
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
        let clip = self.clip.as_ref();
        let info = clip.and_then(|clip| clip.info.as_ref());

        let headline = match (&self.notice, clip, info) {
            (Some(notice), _, _) => notice.clone(),
            (None, Some(clip), Some(info)) => format!(
                "{}   {} {}x{} at {} fps, {}",
                clip.name,
                info.codec,
                info.width,
                info.height,
                frames_per_second(info.frame_rate),
                match info.hardware {
                    Some(name) => format!("{name} decode"),
                    None => "software decode".to_string(),
                }
            ),
            (None, Some(clip), None) => format!("Opening {}", clip.name),
            (None, None, _) => String::new(),
        };
        let header = row![
            button("Open clip").on_press(Message::OpenRequested),
            one_line(headline).width(Fill),
        ]
        .spacing(12)
        .align_y(Center);

        let shown = clip.and_then(|clip| clip.shown.as_ref());
        let picture: Element<'_, Message> = match shown.map(|shown| &shown.picture) {
            Some(Displayed::Yuv(planes)) => shader(planes).width(Fill).height(Fill).into(),
            Some(Displayed::Image(allocation)) => image(allocation.handle().clone())
                .content_fit(ContentFit::Contain)
                .width(Fill)
                .height(Fill)
                .into(),
            None => center(text("Open a clip to start scrubbing")).into(),
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

        let mut layout = column![header, viewer].spacing(10).padding(12);
        if let (Some(clip), Some(info)) = (clip, info) {
            let last_frame = info.frame_count.saturating_sub(1);
            // The slider widget needs a type that converts to f64, hence u32 frame numbers here.
            layout = layout.push(slider(
                0..=u32::try_from(last_frame).unwrap_or(u32::MAX),
                u32::try_from(clip.playhead).unwrap_or(u32::MAX),
                Message::Scrubbed,
            ));
            layout = layout.push(
                row![
                    button("Previous frame").on_press(Message::Stepped(-1)),
                    button("Next frame").on_press(Message::Stepped(1)),
                    text(format!(
                        "{}   frame {} of {}",
                        timecode(clip.playhead, info.frame_rate),
                        clip.playhead,
                        last_frame
                    ))
                    .wrapping(text::Wrapping::None),
                    one_line(self.readout(clip)).align_right(Fill),
                ]
                .spacing(12)
                .align_y(Center),
            );
        }
        layout.into()
    }

    fn readout(&self, clip: &Clip) -> String {
        let milliseconds = |latency: Option<Duration>| match latency {
            Some(latency) => format!("{} ms", latency.as_millis()),
            None => "...".to_string(),
        };
        let mut parts = vec![
            format!("first picture {}", milliseconds(clip.latency.first_picture)),
            format!("exact frame {}", milliseconds(clip.latency.exact)),
        ];
        if let Some(shown) = &clip.shown {
            if !shown.exact {
                parts.push("showing keyframe".to_string());
            }
            parts.push(if shown.hardware { "GPU decode" } else { "CPU decode" }.to_string());
        }
        let path = match self.viewer {
            Some(ViewerPath::Shader) => "shader viewer",
            Some(ViewerPath::Cpu) => "CPU viewer",
            None => "viewer",
        };
        match &clip.shown {
            Some(Shown {
                size: (width, height),
                ..
            }) => parts.push(format!("{path} {width}x{height}")),
            None => parts.push(path.to_string()),
        }
        if let Some(memory_mb) = self.memory_mb {
            parts.push(format!("memory {memory_mb} MB"));
        }
        parts.join("   ")
    }
}

impl Clip {
    fn move_playhead(&mut self, frame: u64) {
        let last_frame = self
            .info
            .as_ref()
            .map_or(u64::MAX, |info| info.frame_count.saturating_sub(1));
        let frame = frame.min(last_frame);
        if frame != self.playhead {
            self.request(frame);
        }
    }

    /// Asks the decoder for pictures in `form` from now on, starting with the current frame.
    fn set_form(&mut self, form: Form) {
        if self.form != Some(form) {
            self.form = Some(form);
            self.scrubber.set_output(form.output);
            self.scrubber.set_max_preview(form.max_size);
            self.request(self.playhead);
        }
    }

    fn request(&mut self, frame: u64) {
        self.playhead = frame;
        self.pending = Some(Pending {
            request: self.scrubber.request(frame),
            at: Instant::now(),
            seen: false,
        });
    }

    fn show(&mut self, order: u64, info: PictureInfo, picture: Displayed) {
        // Uploads finish independently, so an older picture can complete after a newer one.
        if self.shown.as_ref().is_some_and(|shown| shown.order > order) {
            return;
        }
        // Pictures for requests that were since replaced are still the freshest image we have, so
        // they are shown too; only the newest request is timed.
        if let Some(pending) = self
            .pending
            .as_mut()
            .filter(|pending| pending.request == info.request)
        {
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
}

/// Text that is cut off instead of wrapped when it runs out of room. The rows around the viewer
/// must keep their height whatever they say: a row that grows squeezes the viewer, and the
/// picture with it.
fn one_line<'a>(content: String) -> container::Container<'a, Message> {
    container(text(content).wrapping(text::Wrapping::None)).clip(true)
}

async fn pick_clip() -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .set_title("Open a video clip")
        .add_filter("Video", VIDEO_EXTENSIONS)
        .pick_file()
        .await
        .map(|file| file.path().to_path_buf())
}

/// Frame rate rounded to the nearest whole number, as used for timecode.
fn frames_per_second(rate: Ratio) -> u64 {
    let (num, den) = (rate.num.max(1) as u64, rate.den.max(1) as u64);
    ((num + den / 2) / den).max(1)
}

/// Non-drop-frame timecode.
fn timecode(frame: u64, rate: Ratio) -> String {
    let fps = frames_per_second(rate);
    let seconds = frame / fps;
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60,
        frame % fps
    )
}
