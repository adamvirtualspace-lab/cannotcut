//! Loads a timeline, prints what cannotcut understood of it, and optionally saves it again. For
//! checking files against other OpenTimelineIO tools. Projects used as clips are printed inside
//! the clip that uses them.
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
    print(&timeline, "");
    if let Some(output) = arguments.next() {
        timeline.save(&output)?;
        println!("saved     {}", output.display());
    }
    Ok(())
}

fn print(timeline: &Timeline, indent: &str) {
    let rate = timeline.rate();
    println!(
        "{indent}timeline  \"{}\" at {}/{} fps, {} frames",
        timeline.name,
        rate.num,
        rate.den,
        timeline.duration()
    );
    for (index, track) in timeline.tracks().enumerate() {
        println!("{indent}track {index}   {:?} \"{}\"", track.kind, track.name);
        for (start, item) in track.placed() {
            match item {
                Item::Clip(clip) => {
                    let media = &clip.media;
                    let what = match (media.is_project(), &media.project) {
                        (true, None) => "project that could not be loaded",
                        (true, Some(_)) => "project",
                        (false, _) => "media",
                    };
                    println!(
                        "{indent}  {start:>6}  clip \"{}\", {} frames from {what} frame {} of {}",
                        clip.name,
                        clip.duration,
                        clip.media_frame(0, rate),
                        media.target.as_deref().unwrap_or("missing media")
                    );
                    if let Some(nested) = &media.project {
                        print(&nested.timeline, &format!("{indent}          | "));
                    }
                }
                Item::Gap(duration) => println!("{indent}  {start:>6}  gap, {duration} frames"),
            }
        }
    }
}
