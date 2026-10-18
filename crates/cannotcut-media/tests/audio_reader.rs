//! The sound reader against `data/ramp.wav`: half a second of mono sound at 8 kHz in which every
//! sample has a value of its own, so where a read starts can be told from what it gives.

use std::path::PathBuf;

use cannotcut_media::{AudioReader, CHANNELS};

const RATE: u32 = 8000;
const SAMPLES: usize = 4000;

fn ramp() -> AudioReader {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/ramp.wav");
    AudioReader::open(&path, RATE)
        .expect("the ramp opens")
        .expect("the ramp has sound")
}

fn read(reader: &mut AudioReader, samples: usize) -> Vec<f32> {
    let mut out = vec![f32::NAN; samples * CHANNELS];
    reader.read(&mut out).expect("the ramp reads");
    out
}

#[test]
fn a_seek_lands_on_the_sample_asked_for() {
    let whole = read(&mut ramp(), SAMPLES);
    assert!(whole[CHANNELS..].iter().all(|sample| *sample > 0.0), "sound throughout");

    let mut reader = ramp();
    for start in [2500, 17, 3999, 0] {
        reader.seek(start as i64).unwrap();
        let part = read(&mut reader, 1);
        assert_eq!(part, whole[start * CHANNELS..(start + 1) * CHANNELS], "from {start}");
    }
    reader.seek(1234).unwrap();
    let part = read(&mut reader, 500);
    assert_eq!(part, whole[1234 * CHANNELS..1734 * CHANNELS]);
}

#[test]
fn past_the_end_is_silence() {
    let whole = read(&mut ramp(), SAMPLES);
    let mut reader = ramp();
    // Over the end: the last of the sound, then silence.
    reader.seek(3990).unwrap();
    let part = read(&mut reader, 100);
    assert_eq!(part[..10 * CHANNELS], whole[3990 * CHANNELS..]);
    assert!(part[10 * CHANNELS..].iter().all(|sample| *sample == 0.0));
    // All of it past the end, as for a clip trimmed beyond where its sound stops.
    reader.seek(6000).unwrap();
    assert!(read(&mut reader, 300).iter().all(|sample| *sample == 0.0));
    // And reading on from there stays silent.
    assert!(read(&mut reader, 300).iter().all(|sample| *sample == 0.0));
}

#[test]
fn before_the_start_is_silence() {
    let whole = read(&mut ramp(), SAMPLES);
    let mut reader = ramp();
    reader.seek(-50).unwrap();
    let part = read(&mut reader, 100);
    assert!(part[..50 * CHANNELS].iter().all(|sample| *sample == 0.0));
    assert_eq!(part[50 * CHANNELS..], whole[..50 * CHANNELS]);
}

#[test]
fn a_waveform_follows_the_sound() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/ramp.wav");
    let waveform = cannotcut_media::Waveform::read(&path, 100).unwrap().unwrap();
    assert_eq!(waveform.rate, 100);
    // Half a second of sound.
    assert!((49..=51).contains(&waveform.peaks.len()), "{}", waveform.peaks.len());
    // The ramp grows louder all the way, so the peaks do too.
    let (early, late) = (waveform.peak(0.0, 0.1), waveform.peak(0.4, 0.5));
    assert!(early < late * 0.3, "{early} against {late}");
    assert!(late > 0.3, "{late}");
    assert_eq!(waveform.peak(0.6, 2.0), 0.0);
}
