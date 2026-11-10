//! Export for cannotcut: a timeline made into an MP4 film, picture by picture with its sound,
//! on a thread of its own, at the size and quality asked for.

mod choices;
mod title;

pub use choices::{Options, Quality, Resolution};
pub use title::{TitlePicture, draw_title, font_file};

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use cannotcut_audio::{Mixer, frame_to_sample};
use cannotcut_media::{CHANNELS, ClipReader, FilmFormat, FilmPicture, FilmWriter, picture_size};
use cannotcut_timeline::{ClipId, Shot, Timeline, Title};

const SAMPLE_RATE: u32 = 48_000;
/// Sound is mixed this many samples at a time.
const CHUNK: usize = 1024;
/// How often progress is reported.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
/// Clips whose readers are kept open, for timelines that go back and forth between them; and
/// as many more as there are layers at a frame.
const OPEN_READERS: usize = 3;
/// Titles kept drawn, for timelines that go back and forth between them.
const DRAWN_TITLES: usize = 8;

/// What an export reports while it runs.
#[derive(Clone, Debug)]
pub enum Event {
    /// The film file is open: its size, and the video encoder that makes it.
    Started {
        width: u32,
        height: u32,
        encoder: &'static str,
    },
    /// Frames made so far, of how many.
    Progress { done: i64, total: i64 },
    /// The film is written, or why not.
    Finished(Result<Summary, String>),
}

#[derive(Clone, Debug)]
pub struct Summary {
    pub path: PathBuf,
    pub frames: i64,
    pub took: Duration,
}

/// An export under way. Dropping it does not stop it; [`cancel`](Self::cancel) does.
pub struct Export {
    cancelled: Arc<AtomicBool>,
}

impl Export {
    /// Starts making `timeline` into a film at `path` and returns at once. `folder` is where
    /// the project file is, for clips whose paths are relative to it. `sink` is called on the
    /// export's thread.
    pub fn start(
        timeline: Timeline,
        folder: Option<PathBuf>,
        path: PathBuf,
        options: Options,
        mut sink: impl FnMut(Event) + Send + 'static,
    ) -> Self {
        let cancelled = Arc::new(AtomicBool::new(false));
        let started = thread::Builder::new().name("cannotcut-export".to_string()).spawn({
            let cancelled = cancelled.clone();
            move || {
                let place = (folder.as_deref(), path.as_path());
                let result = run(&timeline, place, options, &cancelled, &mut sink);
                sink(Event::Finished(result));
            }
        });
        if let Err(error) = started {
            // The sink went with the thread that was not made; nothing can be reported.
            eprintln!("export: {error}");
        }
        Self { cancelled }
    }

    /// Stops the export; the part made so far is thrown away.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

fn run(
    timeline: &Timeline,
    (folder, path): (Option<&Path>, &Path),
    options: Options,
    cancelled: &AtomicBool,
    sink: &mut dyn FnMut(Event),
) -> Result<Summary, String> {
    let began = Instant::now();
    let total = timeline.duration();
    if total == 0 {
        return Err("there is nothing on the timeline to export".to_string());
    }
    let locate = |shot: &Shot| -> Result<PathBuf, String> {
        shot.file(folder)
            .ok_or_else(|| format!("the media of \"{}\" is missing", shot.clip.name))
    };

    // The film takes its shape from the first clip on the main track, as the project's
    // pictures were first seen; inside a nested project if that is what it plays.
    let first = timeline
        .first_shot()
        .ok_or_else(|| "there are no clips with media on the timeline".to_string())?;
    let first_path = locate(&first)?;
    let source =
        picture_size(&first_path).map_err(|error| format!("{}: {error}", first.clip.name))?;
    let rate = timeline.rate();
    let (width, height) = options.resolution.film_size(source);
    let film = FilmFormat {
        width,
        height,
        rate: (rate.num, rate.den),
        sample_rate: SAMPLE_RATE,
        bit_rate: options.quality.bit_rate((width, height), rate),
    };

    // Written under another name, and only given its own once it is whole.
    let mut partial = path.as_os_str().to_owned();
    partial.push(".exporting");
    let partial = PathBuf::from(partial);
    let made = make(timeline, folder, &partial, film, cancelled, sink, &locate);
    match made {
        Ok(()) => {
            fs::rename(&partial, path).map_err(|error| format!("finish the film: {error}"))?;
            Ok(Summary {
                path: path.to_path_buf(),
                frames: total,
                took: began.elapsed(),
            })
        }
        Err(error) => {
            let _ = fs::remove_file(&partial);
            Err(error)
        }
    }
}

fn make(
    timeline: &Timeline,
    folder: Option<&Path>,
    path: &Path,
    film: FilmFormat,
    cancelled: &AtomicBool,
    sink: &mut dyn FnMut(Event),
    locate: &dyn Fn(&Shot) -> Result<PathBuf, String>,
) -> Result<(), String> {
    let mut writer = FilmWriter::create(path, film).map_err(|error| error.to_string())?;
    sink(Event::Started {
        width: film.width,
        height: film.height,
        encoder: writer.encoder,
    });
    let total = timeline.duration();
    let rate = timeline.rate();
    let mut mixer = Mixer::new(timeline.clone(), 0, SAMPLE_RATE, folder.map(Path::to_path_buf));
    let mut sound = vec![0.0; CHUNK * CHANNELS];
    // By the clips a picture is seen through, the one used last at the end: each clip reads
    // its media from front to back.
    let mut readers: Vec<(Vec<ClipId>, ClipReader)> = Vec::new();
    // Where a layer is drawn before it is laid over the film's picture.
    let mut spare = writer.spare_picture();
    let film_size = (film.width, film.height);
    // Text clips' words, drawn once each; the one used last at the end.
    let mut titles: Vec<(Title, FilmPicture)> = Vec::new();
    let mut reported = Instant::now();

    for index in 0..total {
        if cancelled.load(Ordering::Relaxed) {
            return Err("cancelled".to_string());
        }
        let shots = timeline.shots_at(index);
        // A single picture as it comes is drawn straight into the film's, as before layers.
        let plain = shots.len() == 1
            && shots[0].clip.media.title.is_none()
            && shots[0].framing.is_identity()
            && shots[0].reveal.is_full();
        let open = OPEN_READERS.max(shots.len());
        if !plain {
            // Where nothing plays the film is black, and layers go over black.
            writer.picture().clear();
        }
        for shot in &shots {
            let clip = shot.clip;
            if let Some(title) = &clip.media.title {
                match titles.iter().position(|(drawn, _)| drawn == title) {
                    Some(place) => {
                        let used = titles.remove(place);
                        titles.push(used);
                    }
                    None => {
                        let drawn = draw_title(title, film_size);
                        let mut picture = writer.spare_picture();
                        picture.fill(&drawn.luma, &drawn.chroma, drawn.alpha);
                        titles.push((title.clone(), picture));
                        if titles.len() > DRAWN_TITLES {
                            titles.remove(0);
                        }
                    }
                }
                let (_, picture) = titles.last().expect("just drawn or put back");
                let area = [0, 0, film.width & !1, film.height & !1];
                let corners = shot.framing.corners(film_size, (area[2], area[3])).0;
                let opacity = shot.framing.opacity() * shot.reveal.fade;
                let wipe = shot.reveal.wipe.map(|wipe| (wipe.direction, wipe.threshold));
                writer.picture().lay_over(picture, area, corners, opacity, wipe);
                continue;
            }
            let reader = match readers.iter().position(|(key, _)| *key == shot.key) {
                Some(place) => {
                    let used = readers.remove(place);
                    readers.push(used);
                    &mut readers.last_mut().expect("just put back").1
                }
                None => {
                    let reader = ClipReader::open(&locate(shot)?, true)
                        .map_err(|error| format!("{}: {error}", clip.name))?;
                    readers.push((shot.key.clone(), reader));
                    while readers.len() > open {
                        readers.remove(0);
                    }
                    &mut readers.last_mut().expect("just opened").1
                }
            };
            let frame = shot.media_frame.max(0) as u64;
            let drawn_into = if plain {
                writer.picture()
            } else {
                &mut spare
            };
            // Past the end of its media a clip shows its last picture, which stays drawn.
            reader
                .draw(frame, drawn_into)
                .map_err(|error| format!("{}: {error}", clip.name))?;
            if !plain && let Some(area) = reader.drawn_area() {
                let corners = shot.framing.corners(film_size, (area[2], area[3])).0;
                let opacity = shot.framing.opacity() * shot.reveal.fade;
                let wipe = shot.reveal.wipe.map(|wipe| (wipe.direction, wipe.threshold));
                writer.picture().lay_over(&spare, area, corners, opacity, wipe);
            }
        }
        writer.write_picture(index).map_err(|error| error.to_string())?;

        // The sound up to the end of this frame.
        let until = frame_to_sample(index + 1, rate, SAMPLE_RATE);
        while mixer.position() < until && !mixer.finished() {
            let wanted = ((until - mixer.position()) as usize).min(CHUNK);
            let count = mixer.mix(&mut sound[..wanted * CHANNELS]);
            writer
                .write_sound(&sound[..count * CHANNELS])
                .map_err(|error| error.to_string())?;
            if let Some(problem) = mixer.problem.take() {
                return Err(problem);
            }
        }

        if reported.elapsed() >= PROGRESS_EVERY {
            reported = Instant::now();
            sink(Event::Progress {
                done: index + 1,
                total,
            });
        }
    }
    sink(Event::Progress { done: total, total });
    writer.finish().map_err(|error| error.to_string())
}
