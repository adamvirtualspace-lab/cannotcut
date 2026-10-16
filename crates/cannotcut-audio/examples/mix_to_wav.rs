//! Mixes the sound of a project into a WAV file, without a sound card, to check the mix: where
//! each clip's sound lands and what part of its media it is.
//!
//! Usage: `cargo run -p cannotcut-audio --example mix_to_wav -- <project.otio> <out.wav>
//! [from frame] [rate]`

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use cannotcut_audio::Mixer;
use cannotcut_media::CHANNELS;
use cannotcut_timeline::Timeline;

const DEFAULT_RATE: u32 = 48_000;
/// Samples mixed at a time, as the player does.
const CHUNK: usize = 1024;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let usage = "usage: mix_to_wav <project.otio> <out.wav> [from frame] [rate]";
    let project = PathBuf::from(arguments.next().ok_or(usage)?);
    let out = PathBuf::from(arguments.next().ok_or(usage)?);
    let from: i64 = arguments.next().map_or(Ok(0), |from| from.parse())?;
    let rate: u32 = arguments.next().map_or(Ok(DEFAULT_RATE), |rate| rate.parse())?;

    let timeline = Timeline::load(&project)?;
    let folder = project.parent().map(PathBuf::from);
    let mut mixer = Mixer::new(timeline, from, rate, folder);
    let started = Instant::now();
    let mut samples = Vec::new();
    let mut chunk = vec![0.0; CHUNK * CHANNELS];
    while !mixer.finished() {
        let count = mixer.mix(&mut chunk);
        samples.extend_from_slice(&chunk[..count * CHANNELS]);
        if let Some(problem) = mixer.problem.take() {
            eprintln!("{problem}");
        }
    }
    let seconds = (samples.len() / CHANNELS) as f64 / f64::from(rate);
    println!(
        "mixed {seconds:.2} s from frame {from} in {:.0} ms",
        started.elapsed().as_secs_f64() * 1000.0
    );
    fs::write(&out, wav(&samples, rate))?;
    Ok(())
}

/// A WAV file of 32-bit float samples.
fn wav(samples: &[f32], rate: u32) -> Vec<u8> {
    const FLOAT: u16 = 3;
    let bytes = (samples.len() * 4) as u32;
    let channels = CHANNELS as u16;
    let mut file = Vec::with_capacity(44 + bytes as usize);
    file.extend_from_slice(b"RIFF");
    file.extend_from_slice(&(36 + bytes).to_le_bytes());
    file.extend_from_slice(b"WAVEfmt ");
    file.extend_from_slice(&16u32.to_le_bytes());
    file.extend_from_slice(&FLOAT.to_le_bytes());
    file.extend_from_slice(&channels.to_le_bytes());
    file.extend_from_slice(&rate.to_le_bytes());
    file.extend_from_slice(&(rate * u32::from(channels) * 4).to_le_bytes());
    file.extend_from_slice(&(channels * 4).to_le_bytes());
    file.extend_from_slice(&32u16.to_le_bytes());
    file.extend_from_slice(b"data");
    file.extend_from_slice(&bytes.to_le_bytes());
    for sample in samples {
        file.extend_from_slice(&sample.to_le_bytes());
    }
    file
}
