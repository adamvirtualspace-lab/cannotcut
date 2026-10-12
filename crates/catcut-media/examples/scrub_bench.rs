//! Headless scrubbing benchmark: measures how quickly pictures arrive for the playhead moves an
//! editor makes (random jumps, a fast drag, single-frame steps).
//!
//! Usage: `cargo run -p catcut-media --example scrub_bench -- <clip> [--software] [--verbose]`
//!
//! `--sweep` replaces the timing runs with a frame-accuracy check of every frame in the clip.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use catcut_media::{Event, Options, Picture, Scrubber};

const RANDOM_JUMPS: usize = 40;
const STEPS_FORWARD: u64 = 60;
const STEPS_BACKWARD: u64 = 30;
const DRAG_DURATION: Duration = Duration::from_secs(4);
/// Roughly one pointer event per display refresh.
const DRAG_INTERVAL: Duration = Duration::from_millis(16);
const PICTURE_TIMEOUT: Duration = Duration::from_secs(20);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut path = None;
    let mut options = Options::default();
    let (mut verbose, mut sweep) = (false, false);
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--software" => options.hardware = false,
            "--verbose" => verbose = true,
            "--sweep" => sweep = true,
            _ => path = Some(PathBuf::from(argument)),
        }
    }
    let path = path.ok_or("usage: scrub_bench <clip> [--software] [--verbose]")?;

    let (sender, pictures) = mpsc::channel();
    let started = Instant::now();
    let mut scrubber = Scrubber::open(&path, options, move |event| {
        let _ = sender.send(event);
    })?;
    let info = match pictures.recv_timeout(PICTURE_TIMEOUT)? {
        Event::Opened(info) => info,
        Event::Failed(error) => return Err(error.into()),
        Event::Picture(_) => return Err("picture arrived before the clip opened".into()),
    };
    println!("clip      {}", info.path.display());
    println!(
        "video     {} {}x{} at {}/{} fps, {} frames, {} keyframes in index",
        info.codec,
        info.width,
        info.height,
        info.frame_rate.num,
        info.frame_rate.den,
        info.frame_count,
        info.keyframe_count
    );
    println!(
        "decoder   {}, preview {}x{}, opened in {} ms",
        info.hardware.unwrap_or("software"),
        info.preview_width,
        info.preview_height,
        started.elapsed().as_millis()
    );

    let mut used_hardware = false;
    let mut wrong_frames = 0;
    let mut jump = |scrubber: &mut Scrubber, frame: u64| -> Result<Jump, String> {
        let request = scrubber.request(frame);
        let result = wait_for_exact(&pictures, request)?;
        used_hardware |= result.picture.timing.hardware;
        if result.picture.frame != frame {
            wrong_frames += 1;
            println!("  wrong frame: asked for {frame}, got {}", result.picture.frame);
        }
        if verbose {
            println!(
                "  frame {frame:>6}: first {:>4} ms, exact {:>4} ms, {:>3} decoded, seek {}",
                result.first_picture.as_millis(),
                result.picture.timing.latency.as_millis(),
                result.picture.timing.frames_decoded,
                result.picture.timing.seeked
            );
        }
        Ok(result)
    };

    if sweep {
        // Walking backwards makes every request seek, so each frame of the clip is reached the
        // hard way once.
        let started = Instant::now();
        for frame in (0..info.frame_count).rev() {
            jump(&mut scrubber, frame)?;
        }
        println!(
            "\nbackward sweep over all {} frames in {:.1} s",
            info.frame_count,
            started.elapsed().as_secs_f64()
        );
        println!("exact pictures on the wrong frame: {wrong_frames}");
        return Ok(());
    }

    // Random jumps: the cost of clicking somewhere else on the timeline.
    let mut random = Lcg(0x5eed);
    let (mut first, mut exact, mut decoded) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..RANDOM_JUMPS {
        let result = jump(&mut scrubber, random.next() % info.frame_count)?;
        first.push(result.first_picture);
        exact.push(result.picture.timing.latency);
        decoded.push(result.picture.timing.frames_decoded);
    }
    println!("\nrandom jumps ({RANDOM_JUMPS})");
    println!("  first picture   {}", summary(&mut first));
    println!("  exact frame     {}", summary(&mut exact));
    decoded.sort_unstable();
    println!(
        "  frames decoded  median {}, max {}",
        decoded[decoded.len() / 2],
        decoded[decoded.len() - 1]
    );

    // Single steps from the middle of the clip: arrow keys.
    let middle = info.frame_count / 2;
    jump(&mut scrubber, middle)?;
    let mut forward = Vec::new();
    for step in 1..=STEPS_FORWARD {
        forward.push(jump(&mut scrubber, middle + step)?.picture.timing.latency);
    }
    let mut backward = Vec::new();
    for step in 1..=STEPS_BACKWARD {
        let frame = middle + STEPS_FORWARD - step;
        backward.push(jump(&mut scrubber, frame)?.picture.timing.latency);
    }
    println!("\nsingle-frame steps");
    println!("  forward  ({STEPS_FORWARD})   {}", summary(&mut forward));
    println!("  backward ({STEPS_BACKWARD})   {}", summary(&mut backward));

    // A fast drag across the whole clip without waiting for pictures.
    let drag_started = Instant::now();
    let (mut requests, mut shown, mut drag_latency) = (0u32, 0u32, Vec::new());
    let (mut last_request, mut last_frame) = (0, 0);
    // The exact picture for the final move can arrive while the drag is still being drained.
    let mut settled = None;
    while drag_started.elapsed() < DRAG_DURATION {
        let progress = drag_started.elapsed().as_secs_f64() / DRAG_DURATION.as_secs_f64();
        last_frame = (progress * (info.frame_count - 1) as f64) as u64;
        last_request = scrubber.request(last_frame);
        settled = None;
        requests += 1;
        thread::sleep(DRAG_INTERVAL);
        for event in pictures.try_iter() {
            if let Event::Picture(picture) = event {
                shown += 1;
                drag_latency.push(picture.timing.latency);
                if picture.exact && picture.request == last_request {
                    settled = Some(picture);
                }
            }
        }
    }
    let settled = match settled {
        Some(picture) => picture,
        None => wait_for_exact(&pictures, last_request)?.picture,
    };
    if settled.frame != last_frame {
        wrong_frames += 1;
        println!("  wrong frame: asked for {last_frame}, got {}", settled.frame);
    }
    println!("\nfast drag across the clip ({} s)", DRAG_DURATION.as_secs());
    println!(
        "  {requests} playhead moves, {shown} pictures shown ({:.0} per second)",
        f64::from(shown) / DRAG_DURATION.as_secs_f64()
    );
    println!("  picture latency {}", summary(&mut drag_latency));
    println!(
        "  exact frame after release  {} ms",
        settled.timing.latency.as_millis()
    );

    println!("\nexact pictures on the wrong frame: {wrong_frames}");
    println!("pictures decoded on the GPU: {used_hardware}");
    if let Some(memory) = memory_stats::memory_stats() {
        println!("process memory: {} MB", memory.physical_mem / (1024 * 1024));
    }
    Ok(())
}

struct Jump {
    /// Latency of the first picture for the request, exact or not.
    first_picture: Duration,
    picture: Picture,
}

fn wait_for_exact(pictures: &Receiver<Event>, request: u64) -> Result<Jump, String> {
    let mut first_picture = None;
    loop {
        match pictures.recv_timeout(PICTURE_TIMEOUT) {
            Ok(Event::Picture(picture)) if picture.request == request => {
                let first_picture = *first_picture.get_or_insert(picture.timing.latency);
                if picture.exact {
                    return Ok(Jump {
                        first_picture,
                        picture,
                    });
                }
            }
            Ok(Event::Picture(_) | Event::Opened(_)) => {}
            Ok(Event::Failed(error)) => return Err(error),
            Err(error) => return Err(format!("no picture for request {request}: {error}")),
        }
    }
}

fn summary(samples: &mut [Duration]) -> String {
    if samples.is_empty() {
        return "no samples".to_string();
    }
    samples.sort_unstable();
    let at = |fraction: f64| samples[((samples.len() - 1) as f64 * fraction).round() as usize];
    format!(
        "median {:>4} ms   p95 {:>4} ms   max {:>4} ms",
        at(0.5).as_millis(),
        at(0.95).as_millis(),
        at(1.0).as_millis()
    )
}

/// Small fixed-seed generator so every run jumps to the same frames.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
}
