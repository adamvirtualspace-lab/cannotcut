//! Asks for frames one after another and writes each exact picture's luma to a PGM file. For
//! clips that carry their frame number in the picture: what a picture really shows can then be
//! checked against what the decoder says it shows.
//!
//! Usage: `cargo run -p cannotcut-media --example frame_dump -- <clip> <folder> <step>...`
//!
//! A step is a frame number, or `rest` to pause as a user would before the next one, which lets
//! the decoder fill its cache behind the playhead. `software` anywhere among the steps decodes
//! without the GPU.

use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use cannotcut_media::{Event, Options, Output, Pixels, Scrubber};

/// Long enough for the frames behind the playhead to be decoded.
const REST: Duration = Duration::from_millis(1200);
const PICTURE_TIMEOUT: Duration = Duration::from_secs(20);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let usage = "usage: frame_dump <clip> <folder> <frame or \"rest\">...";
    let clip = PathBuf::from(arguments.next().ok_or(usage)?);
    let folder = PathBuf::from(arguments.next().ok_or(usage)?);
    let mut steps: Vec<String> = arguments.collect();
    let hardware = !steps.iter().any(|step| step == "software");
    steps.retain(|step| step != "software");

    let options = Options {
        hardware,
        output: Output::Yuv,
        ..Options::default()
    };
    let (sender, events) = mpsc::channel();
    let mut scrubber = Scrubber::open(&clip, options, move |event| {
        let _ = sender.send(event);
    })?;
    for (step, frame) in steps.iter().enumerate() {
        if frame == "rest" {
            thread::sleep(REST);
            continue;
        }
        let frame: u64 = frame.parse()?;
        let request = scrubber.request(frame);
        let picture = loop {
            match events.recv_timeout(PICTURE_TIMEOUT)? {
                Event::Picture(picture) if picture.request == request && picture.exact => {
                    break picture;
                }
                Event::Failed(error) => return Err(error.into()),
                _ => {}
            }
        };
        let Pixels::Nv12 { luma, .. } = &picture.pixels else {
            return Err("expected YUV pictures".into());
        };
        let path = folder.join(format!("{step:02}-asked-{frame}-says-{}.pgm", picture.frame));
        let mut file = format!("P5\n{} {}\n255\n", picture.width, picture.height).into_bytes();
        file.extend_from_slice(luma);
        fs::write(&path, file)?;
        println!(
            "asked for {frame}, decoder says {}: {} decoded, seek {}, cached {}  -> {}",
            picture.frame,
            picture.timing.frames_decoded,
            picture.timing.seeked,
            picture.timing.cached,
            path.display()
        );
    }
    Ok(())
}
