//! Plays a project through the sound card while recording what the card is sent (loopback,
//! Windows only), and logs where the player says the sound is. For checking that the sound is
//! unbroken and that the playback clock agrees with it.
//!
//! Usage: `cargo run -p cannotcut-audio --example play_check -- <project.otio> <out prefix>
//! [from frame] [seconds]`
//!
//! Writes `<prefix>.wav` (the recording, stereo float), `<prefix>-blocks.csv` (when each block of
//! the recording was captured) and `<prefix>-clock.csv` (the player's position over time). Times
//! are seconds since play was asked for.

use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cannotcut_audio::{Player, Status};
use cannotcut_timeline::Timeline;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

const POLL: Duration = Duration::from_millis(1);

type Blocks = Arc<Mutex<Vec<(Instant, Vec<f32>)>>>;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let usage = "usage: play_check <project.otio> <out prefix> [from frame] [seconds]";
    let project = PathBuf::from(arguments.next().ok_or(usage)?);
    let prefix = arguments.next().ok_or(usage)?;
    let from: i64 = arguments.next().map_or(Ok(0), |from| from.parse())?;
    let seconds: f64 = arguments.next().map_or(Ok(10.0), |seconds| seconds.parse())?;
    let timeline = Timeline::load(&project)?;

    let opening = Instant::now();
    let mut player = Player::start();
    while matches!(player.status(), Status::Starting) {
        thread::sleep(POLL);
    }
    println!("{:?} after {:?}", player.status(), opening.elapsed());

    // Recording an output device records what it plays.
    let device = cpal::default_host()
        .default_output_device()
        .ok_or("no sound output")?;
    let config: cpal::StreamConfig = device.default_output_config()?.into();
    let channels = usize::from(config.channels);
    let rate = config.sample_rate;
    let blocks: Blocks = Arc::default();
    let recording = device.build_input_stream::<f32, _, _>(
        config,
        {
            let blocks = blocks.clone();
            move |data: &[f32], info: &cpal::InputCallbackInfo| {
                // Both are read from the same system clock as `Instant`. In a loopback
                // recording the capture time is when the device plays the sound, which can be
                // after the callback.
                let timestamp = info.timestamp();
                let now = Instant::now();
                let captured = match timestamp.capture.checked_duration_since(timestamp.callback) {
                    Some(ahead) => now + ahead,
                    None => now - timestamp.callback.duration_since(timestamp.capture),
                };
                blocks.lock().unwrap().push((captured, data.to_vec()));
            }
        },
        |error| eprintln!("recording: {error}"),
        None,
    )?;
    recording.play()?;
    thread::sleep(Duration::from_millis(300));

    let started = Instant::now();
    player.play(timeline, from, project.parent().map(PathBuf::from));
    let mut clock = String::from("time,position\n");
    let mut first_sound = None;
    while started.elapsed().as_secs_f64() < seconds {
        let now = Instant::now();
        let since = now.duration_since(started).as_secs_f64();
        match player.position(now) {
            Some(position) => {
                first_sound.get_or_insert(since);
                writeln!(clock, "{since:.6},{position:.6}")?;
            }
            None => writeln!(clock, "{since:.6},")?,
        }
        thread::sleep(POLL);
    }
    player.stop();
    thread::sleep(Duration::from_millis(200));
    drop(recording);
    if let Some(problem) = player.problem() {
        println!("problem: {problem}");
    }
    println!("first sound after {:.1} ms", first_sound.unwrap_or(f64::NAN) * 1000.0);

    let blocks = blocks.lock().unwrap();
    let mut table = String::from("time,first_sample\n");
    let mut samples = Vec::new();
    for (captured, data) in blocks.iter() {
        let time = if *captured >= started {
            captured.duration_since(started).as_secs_f64()
        } else {
            -started.duration_since(*captured).as_secs_f64()
        };
        writeln!(table, "{time:.6},{}", samples.len() / 2)?;
        for frame in data.chunks_exact(channels) {
            samples.push(frame[0]);
            samples.push(*frame.get(1).unwrap_or(&frame[0]));
        }
    }
    fs::write(format!("{prefix}-blocks.csv"), table)?;
    fs::write(format!("{prefix}-clock.csv"), clock)?;
    fs::write(format!("{prefix}.wav"), wav(&samples, rate))?;
    println!(
        "recorded {:.2} s in {} blocks at {rate} Hz, {channels} channels",
        samples.len() as f64 / 2.0 / f64::from(rate),
        blocks.len()
    );
    Ok(())
}

/// A WAV file of 32-bit float stereo samples.
fn wav(samples: &[f32], rate: u32) -> Vec<u8> {
    const FLOAT: u16 = 3;
    let bytes = (samples.len() * 4) as u32;
    let mut file = Vec::with_capacity(44 + bytes as usize);
    file.extend_from_slice(b"RIFF");
    file.extend_from_slice(&(36 + bytes).to_le_bytes());
    file.extend_from_slice(b"WAVEfmt ");
    file.extend_from_slice(&16u32.to_le_bytes());
    file.extend_from_slice(&FLOAT.to_le_bytes());
    file.extend_from_slice(&2u16.to_le_bytes());
    file.extend_from_slice(&rate.to_le_bytes());
    file.extend_from_slice(&(rate * 8).to_le_bytes());
    file.extend_from_slice(&8u16.to_le_bytes());
    file.extend_from_slice(&32u16.to_le_bytes());
    file.extend_from_slice(b"data");
    file.extend_from_slice(&bytes.to_le_bytes());
    for sample in samples {
        file.extend_from_slice(&sample.to_le_bytes());
    }
    file
}
