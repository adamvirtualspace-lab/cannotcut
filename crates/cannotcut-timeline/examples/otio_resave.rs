//! Loads a timeline, prints what cannotcut understood of it, and optionally saves it again. For
//! checking files against other OpenTimelineIO tools.
//!
//! Usage: `cargo run -p cannotcut-timeline --example otio_resave -- <in.otio> [<out.otio>]`

use std::path::PathBuf;

use cannotcut_timeline::{Item, Timeline};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1).map(PathBuf::from);
    let input = arguments
        .next()
        .ok_or("usage: otio_resave <in.otio> [<out.otio>]")?;
    let timeline = Timeline::load(&input)?;
    let rate = timeline.rate();
    println!(
        "timeline  \"{}\" at {}/{} fps, {} frames",
        timeline.name,
        rate.num,
        rate.den,
        timeline.duration()
    );
    for (index, track) in timeline.tracks().enumerate() {
        println!("track {index}   {:?} \"{}\"", track.kind, track.name);
        for (start, item) in track.placed() {
            match item {
                Item::Clip(clip) => println!(
                    "  {start:>6}  clip \"{}\", {} frames from media frame {} of {}",
                    clip.name,
                    clip.duration,
                    clip.media_frame(0, rate),
                    clip.media.target.as_deref().unwrap_or("missing media")
                ),
                Item::Gap(duration) => println!("  {start:>6}  gap, {duration} frames"),
            }
        }
    }
    if let Some(output) = arguments.next() {
        timeline.save(&output)?;
        println!("saved     {}", output.display());
    }
    Ok(())
}
