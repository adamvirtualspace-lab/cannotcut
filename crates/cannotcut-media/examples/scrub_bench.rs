//! Headless scrubbing benchmark: measures how quickly pictures arrive for the playhead moves an
//! editor makes (random jumps, a fast drag, single-frame steps).
//!
//! Usage: `cargo run -p cannotcut-media --example scrub_bench -- <clip> [--software] [--yuv]
//! [--fit=<width>x<height>] [--verbose]`
//!
//! `--yuv` asks for pictures the way the GPU viewer takes them, without the conversion to RGB.
//! `--fit` sets the viewer size the pictures are scaled down to.
//! `--sweep` replaces the timing runs with a frame-accuracy check of every frame in the clip.
//! `--verify` replaces them with a check of the pictures themselves: the decoder's word for which
//! frame a picture shows is not taken, the picture is compared with the one reached by decoding
//! straight through from the start.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use cannotcut_media::{Event, Options, Output, Picture, Pixels, Scrubber};

const RANDOM_JUMPS: usize = 40;
const STEPS_FORWARD: u64 = 60;
const STEPS_BACKWARD: u64 = 30;
const STEPS_AFTER_REST: u64 = 100;
const REST: Duration = Duration::from_millis(1000);
const DRAG_DURATION: Duration = Duration::from_secs(4);
/// Roughly one pointer event per display refresh.
const DRAG_INTERVAL: Duration = Duration::from_millis(16);
const PICTURE_TIMEOUT: Duration = Duration::from_secs(20);
/// `--verify` looks at this many frames from the start of the clip, and jumps this often.
const VERIFY_FRAMES: u64 = 3000;
const VERIFY_JUMPS: usize = 400;
/// Pictures are compared as grids of average brightness, this many cells across and down.
const SIGNATURE_GRID: (usize, usize) = (48, 27);
/// A picture reached by a jump is held against the frames this far around the one asked for.
const VERIFY_NEIGHBOURS: u64 = 3;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut path = None;
    let mut options = Options::default();
    let (mut verbose, mut sweep, mut verify) = (false, false, false);
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--software" => options.hardware = false,
            "--yuv" => options.output = Output::Yuv,
            "--verbose" => verbose = true,
            "--sweep" => sweep = true,
            "--verify" => verify = true,
            _ => match argument.strip_prefix("--fit=") {
                Some(size) => {
                    let (width, height) = size
                        .split_once('x')
                        .ok_or("--fit takes <width>x<height>")?;
                    options.max_preview = (width.parse()?, height.parse()?);
                }
                None => path = Some(PathBuf::from(argument)),
            },
        }
    }
    let usage = "scrub_bench <clip> [--software] [--yuv] [--fit=WxH] [--verbose] [--sweep | --verify]";
    let path = path.ok_or(format!("usage: {usage}"))?;

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
        "decoder   {}, pictures fit inside {}x{}, opened in {} ms",
        info.hardware.unwrap_or("software"),
        options.max_preview.0,
        options.max_preview.1,
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

    if verify {
        let wrong = verify_pictures(&mut scrubber, &pictures, info.frame_count)?;
        return if wrong == 0 {
            Ok(())
        } else {
            Err(format!("{wrong} pictures do not show the frame they were asked for").into())
        };
    }

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

    // Backward from a frame reached by a jump, after resting on it: nothing behind it has been
    // shown yet, so this is what the look-behind is for. Going on past what it prepared shows
    // the cost of refilling while stepping.
    let rest_frame = info.frame_count / 3;
    jump(&mut scrubber, rest_frame)?;
    thread::sleep(REST);
    let (mut rested, mut from_cache) = (Vec::new(), 0u64);
    for step in 1..=STEPS_AFTER_REST.min(rest_frame) {
        let result = jump(&mut scrubber, rest_frame - step)?;
        rested.push(result.picture.timing.latency);
        from_cache += u64::from(result.picture.timing.cached);
    }
    println!(
        "  backward after a {} ms rest ({})   {}",
        REST.as_millis(),
        rested.len(),
        summary(&mut rested)
    );
    println!("    {from_cache} of {} came from the cache", rested.len());

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

/// Jumps around the start of the clip and checks every picture against the one decoding straight
/// through gave for that frame. Returns how many showed another frame.
fn verify_pictures(
    scrubber: &mut Scrubber,
    pictures: &Receiver<Event>,
    frame_count: u64,
) -> Result<usize, String> {
    let frames = frame_count.min(VERIFY_FRAMES);
    let mut exact = |frame: u64| {
        let request = scrubber.request(frame);
        wait_for_exact(pictures, request).map(|jump| jump.picture)
    };
    // Asked for in order, every frame comes from decoding on from the one before.
    let mut straight = Vec::new();
    for frame in 0..frames {
        straight.push(signature(&exact(frame)?));
    }

    let mut random = Lcg(0x5eed);
    let (mut wrong, mut decoded) = (0, 0);
    for _ in 0..VERIFY_JUMPS {
        let frame = random.next() % frames;
        let picture = exact(frame)?;
        decoded += usize::from(!picture.timing.cached);
        let seen = signature(&picture);
        let unlike = |other: u64| distance(&seen, &straight[other as usize]);
        let first = frame.saturating_sub(VERIFY_NEIGHBOURS);
        let nearest = (first..(frame + VERIFY_NEIGHBOURS + 1).min(frames))
            .min_by_key(|other| unlike(*other))
            .unwrap_or(frame);
        // A jump decodes the same data as going straight through, so the two pictures of a
        // frame are equal but for the odd rounding. It is wrong when it is far from that, or
        // when a neighbouring frame fits better.
        if unlike(frame) as usize > seen.len() || unlike(nearest) < unlike(frame) {
            wrong += 1;
            if wrong <= 10 {
                let (decoded, seeked) = (picture.timing.frames_decoded, picture.timing.seeked);
                println!("  frame {frame}: the picture is another frame's, most like {nearest}");
                println!("    ({decoded} decoded, seek {seeked})");
            }
        }
    }
    println!(
        "
{VERIFY_JUMPS} jumps within the first {frames} frames, {decoded} of them decoded afresh"
    );
    println!("pictures that do not show their frame: {wrong}");
    Ok(wrong)
}

/// A picture boiled down to a grid of average brightness.
fn signature(picture: &Picture) -> Vec<u8> {
    let (width, height) = (picture.width as usize, picture.height as usize);
    // Brightness is the luma plane, or near enough the green of RGBA.
    let (samples, step, offset) = match &picture.pixels {
        Pixels::Nv12 { luma, .. } => (luma, 1, 0),
        Pixels::Rgba(rgba) => (rgba, 4, 1),
    };
    let (across, down) = SIGNATURE_GRID;
    let mut cells = Vec::with_capacity(across * down);
    for row in 0..down {
        for column in 0..across {
            let (left, right) = (column * width / across, (column + 1) * width / across);
            let (top, bottom) = (row * height / down, (row + 1) * height / down);
            let mut sum = 0;
            for y in top..bottom {
                let line = &samples[y * width * step..(y + 1) * width * step];
                sum += (left..right)
                    .map(|x| u64::from(line[x * step + offset]))
                    .sum::<u64>();
            }
            let count = ((right - left) * (bottom - top)).max(1) as u64;
            cells.push((sum / count) as u8);
        }
    }
    cells
}

/// How unlike two signatures are.
fn distance(a: &[u8], b: &[u8]) -> u32 {
    a.iter().zip(b).map(|(a, b)| u32::from(a.abs_diff(*b))).sum()
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
