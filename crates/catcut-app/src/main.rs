//! catcut scrubbing spike: one clip, a viewer, a scrub bar, and live latency and memory readouts.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use catcut_media::{ClipInfo, Event, Options, Ratio, Scrubber};
use iced::futures::channel::mpsc;
use iced::keyboard::{self, key};
use iced::widget::{button, center, column, container, image, row, slider, space, text};
use iced::{Center, Color, ContentFit, Element, Fill, Subscription, Task, Theme, time};
use iced_runtime::image::{Allocation, allocate};

const MEMORY_REFRESH: Duration = Duration::from_secs(1);
const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "mkv", "webm", "m4v", "avi", "ts"];
const BYTES_PER_MB: usize = 1024 * 1024;

fn main() -> iced::Result {
    iced::application(App::boot, App::update, App::view)
        .title("catcut")
        .subscription(App::subscription)
        .theme(Theme::Dark)
        .window_size((1280.0, 820.0))
        .run()
}

#[derive(Clone)]
enum Message {
    OpenRequested,
    FileChosen(Option<PathBuf>),
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
        handle: image::Handle,
    },
    Failed(String),
}

#[derive(Clone, Copy)]
struct PictureInfo {
    request: u64,
    exact: bool,
    hardware: bool,
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
                },
                handle: image::Handle::from_rgba(picture.width, picture.height, picture.rgba),
            },
            Event::Failed(error) => Self::Failed(error),
        }
    }
}

#[derive(Default)]
struct App {
    clip: Option<Clip>,
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
    /// Holding the allocation keeps the picture on the GPU, so drawing it never shows a gap.
    allocation: Allocation,
    order: u64,
    exact: bool,
    hardware: bool,
}

#[derive(Default)]
struct Latency {
    first_picture: Option<Duration>,
    exact: Option<Duration>,
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let mut app = Self::default();
        let task = match std::env::args_os().nth(1) {
            Some(path) => app.open(path.into()),
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
                    pending: None,
                    arrivals: 0,
                    shown: None,
                    latency: Latency::default(),
                };
                clip.request(0);
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
            Message::Media(generation, event) => {
                if let Some(clip) = self.clip.as_mut().filter(|clip| clip.generation == generation)
                {
                    match event {
                        Media::Opened(info) => clip.info = Some(info),
                        Media::Picture { picture, handle } => {
                            // iced uploads images in the background; swapping to a picture before
                            // its upload finishes would flash the empty viewer.
                            clip.arrivals += 1;
                            let order = clip.arrivals;
                            return allocate(handle).map(move |allocation| Message::Uploaded {
                                generation,
                                order,
                                picture,
                                allocation: allocation.ok(),
                            });
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
                        Some(allocation) => clip.show(order, picture, allocation),
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

    fn step(&mut self, frames: i64) {
        if let Some(clip) = &mut self.clip {
            clip.move_playhead(clip.playhead.saturating_add_signed(frames));
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            keyboard::listen().map(Message::Key),
            time::every(MEMORY_REFRESH).map(|_| Message::MemoryTick),
        ])
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
            text(headline)
        ]
        .spacing(12)
        .align_y(Center);

        let picture: Element<'_, Message> = match clip.and_then(|clip| clip.shown.as_ref()) {
            Some(shown) => image(shown.allocation.handle().clone())
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
                    )),
                    space().width(Fill),
                    text(self.readout(clip)),
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
            parts.push(if shown.hardware { "GPU" } else { "CPU" }.to_string());
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

    fn request(&mut self, frame: u64) {
        self.playhead = frame;
        self.pending = Some(Pending {
            request: self.scrubber.request(frame),
            at: Instant::now(),
            seen: false,
        });
    }

    fn show(&mut self, order: u64, picture: PictureInfo, allocation: Allocation) {
        // Uploads finish independently, so an older picture can complete after a newer one.
        if self.shown.as_ref().is_some_and(|shown| shown.order > order) {
            return;
        }
        // Pictures for requests that were since replaced are still the freshest image we have, so
        // they are shown too; only the newest request is timed.
        if let Some(pending) = self
            .pending
            .as_mut()
            .filter(|pending| pending.request == picture.request)
        {
            let elapsed = pending.at.elapsed();
            if !pending.seen {
                pending.seen = true;
                self.latency.first_picture = Some(elapsed);
                self.latency.exact = None;
            }
            if picture.exact {
                self.latency.exact = Some(elapsed);
                self.pending = None;
            }
        }
        self.shown = Some(Shown {
            allocation,
            order,
            exact: picture.exact,
            hardware: picture.hardware,
        });
    }
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
