//! Reads a file's waveform and says how long that took and what came out.
//!
//! Usage: `cargo run -p cannotcut-media --example waveform_time -- <clip>`

use std::path::PathBuf;
use std::time::Instant;

use cannotcut_media::Waveform;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let clip = PathBuf::from(std::env::args().nth(1).ok_or("usage: waveform_time <clip>")?);
    let started = Instant::now();
    match Waveform::read(&clip, 100)? {
        Some(waveform) => {
            let seconds = waveform.peaks.len() as f64 / f64::from(waveform.rate);
            let loudest = waveform.peaks.iter().copied().fold(0.0, f32::max);
            println!(
                "{seconds:.1} s of sound read in {:.2} s; loudest peak {loudest:.2}",
                started.elapsed().as_secs_f64()
            );
        }
        None => println!("no sound"),
    }
    Ok(())
}
