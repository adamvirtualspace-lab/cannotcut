//! Headless memory probe: plays through several clips one after another, the way playback opens a
//! decoder for each clip on the timeline, and reports the process's memory after each step.
//!
//! Usage: `cargo run -p cannotcut-media --example memory_probe -- <clip>... [--software]
//! [--frames=<n>] [--keep=<n>] [--cache-mb=<n>] [--rest] [--playing]`
//!
//! `--keep` is how many scrubbers stay open at once (the app keeps a few for going back across a
//! cut); the oldest is dropped when another opens. `--rest` rests the clips played before the
//! latest, as the app does with those off screen. `--playing` asks for the frames as playback
//! does, which caches only the last few.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use cannotcut_media::{Event, Options, Output, Scrubber};

const PICTURE_TIMEOUT: Duration = Duration::from_secs(20);
/// Time for a dropped scrubber's thread to notice and free what it holds.
const DROP_SETTLE: Duration = Duration::from_millis(300);
/// Time for a resting scrubber to let go, which it waits a while to do.
const REST_SETTLE: Duration = Duration::from_millis(6500);
const MB: usize = 1024 * 1024;

fn memory() -> usize {
    memory_stats::memory_stats().map_or(0, |usage| usage.physical_mem / MB)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut paths = Vec::new();
    let mut options = Options {
        output: Output::Yuv,
        max_preview: (1280, 720),
        ..Options::default()
    };
    let (mut frames, mut keep, mut rest, mut playing) = (120_u64, 4_usize, false, false);
    for argument in std::env::args().skip(1) {
        if argument == "--playing" {
            playing = true;
        } else if argument == "--rest" {
            rest = true;
        } else if argument == "--software" {
            options.hardware = false;
        } else if let Some(value) = argument.strip_prefix("--frames=") {
            frames = value.parse()?;
        } else if let Some(value) = argument.strip_prefix("--keep=") {
            keep = value.parse()?;
        } else if let Some(value) = argument.strip_prefix("--cache-mb=") {
            options.cache_bytes = value.parse::<usize>()? * MB;
        } else {
            paths.push(PathBuf::from(argument));
        }
    }
    if paths.is_empty() {
        return Err(
            "memory_probe <clip>... [--software] [--frames=N] [--keep=N] [--cache-mb=N] [--rest] \n             [--playing]"
                .into(),
        );
    }

    println!("start: {} MB", memory());
    let mut open: VecDeque<(Scrubber, Receiver<Event>)> = VecDeque::new();
    for path in &paths {
        let before = memory();
        let (sender, events) = mpsc::channel();
        let mut scrubber = Scrubber::open(path, options, move |event| {
            let _ = sender.send(event);
        })?;
        scrubber.set_playing(playing);
        match events.recv_timeout(PICTURE_TIMEOUT)? {
            Event::Opened(info) => print!(
                "{}: {}x{} {} {:?}",
                path.file_name().unwrap_or_default().to_string_lossy(),
                info.width,
                info.height,
                info.codec,
                info.hardware
            ),
            Event::Failed(error) => return Err(error.into()),
            Event::Picture(_) => unreachable!("a picture before the clip opened"),
        }
        let opened = memory();
        open.push_back((scrubber, events));
        let (scrubber, events) = open.back_mut().expect("just pushed");
        let started = Instant::now();
        for frame in 0..frames {
            let id = scrubber.request(frame);
            loop {
                match events.recv_timeout(PICTURE_TIMEOUT)? {
                    Event::Picture(picture) if picture.request == id && picture.exact => break,
                    Event::Failed(error) => return Err(error.into()),
                    _ => {}
                }
            }
        }
        let played = memory();
        print!(
            "  open +{} MB, play +{} MB ({:.0} fps)",
            opened as i64 - before as i64,
            played as i64 - opened as i64,
            frames as f64 / started.elapsed().as_secs_f64()
        );
        while open.len() > keep {
            drop(open.pop_front());
            thread::sleep(DROP_SETTLE);
        }
        if rest && open.len() > 1 {
            for (earlier, _) in open.iter().take(open.len() - 1) {
                earlier.rest();
            }
            thread::sleep(REST_SETTLE);
        }
        println!("  => {} MB with {} open", memory(), open.len());
    }
    open.clear();
    thread::sleep(DROP_SETTLE * 3);
    println!("all closed: {} MB", memory());
    Ok(())
}
