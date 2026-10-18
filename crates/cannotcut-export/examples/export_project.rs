//! Exports a project to an MP4 file from the command line, printing progress.
//!
//! Usage: `cargo run -p cannotcut-export --example export_project -- <project.otio> <film.mp4>
//! [resolution] [quality]`, where resolution is `original` or the shorter side in pixels, and
//! quality is `lower`, `recommended` or `higher`.

use std::path::PathBuf;
use std::sync::mpsc;

use cannotcut_export::{Event, Export, Options, Quality, Resolution};
use cannotcut_timeline::Timeline;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let usage = "usage: export_project <project.otio> <film.mp4> [resolution] [quality]";
    let project = PathBuf::from(arguments.next().ok_or(usage)?);
    let film = PathBuf::from(arguments.next().ok_or(usage)?);
    let mut options = Options::default();
    if let Some(resolution) = arguments.next() {
        options.resolution = Resolution::from_key(&resolution).ok_or(usage)?;
    }
    if let Some(quality) = arguments.next() {
        options.quality = Quality::from_key(&quality).ok_or(usage)?;
    }
    let timeline = Timeline::load(&project)?;
    let (sender, events) = mpsc::channel();
    let folder = project.parent().map(PathBuf::from);
    let _export = Export::start(timeline, folder, film, options, move |event| {
        let _ = sender.send(event);
    });
    for event in events {
        match event {
            Event::Started {
                width,
                height,
                encoder,
            } => println!("{width}x{height} with {encoder}"),
            Event::Progress { done, total } => println!("{done} of {total} frames"),
            Event::Finished(Ok(summary)) => {
                println!(
                    "wrote {} frames to {} in {:.1} s",
                    summary.frames,
                    summary.path.display(),
                    summary.took.as_secs_f64()
                );
                return Ok(());
            }
            Event::Finished(Err(error)) => return Err(error.into()),
        }
    }
    Err("the export stopped without saying why".into())
}
