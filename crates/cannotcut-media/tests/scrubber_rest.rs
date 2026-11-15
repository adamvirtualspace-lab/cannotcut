//! A scrubber told to rest lets go of its decoder; asked again, it must pick up where asked as if
//! nothing had happened.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use cannotcut_media::{Event, FilmFormat, FilmWriter, Options, Output, Scrubber};

const FRAMES: i64 = 60;
const TIMEOUT: Duration = Duration::from_secs(20);
/// Longer than a scrubber waits before it rests.
const REST_WAIT: Duration = Duration::from_millis(6500);

fn write_clip() -> PathBuf {
    let path = std::env::temp_dir().join(format!("cannotcut-rest-{}.mp4", std::process::id()));
    let film = FilmFormat {
        width: 320,
        height: 240,
        rate: (30, 1),
        sample_rate: 48_000,
        bit_rate: 500_000,
    };
    let mut writer = FilmWriter::create(&path, film).expect("the test clip starts");
    for index in 0..FRAMES {
        writer.write_picture(index).expect("a picture is written");
    }
    writer.finish().expect("the test clip is finished");
    path
}

fn exact_picture(events: &Receiver<Event>, request: u64) -> u64 {
    loop {
        match events.recv_timeout(TIMEOUT).expect("a picture arrives") {
            Event::Picture(picture) if picture.request == request && picture.exact => {
                return picture.frame;
            }
            Event::Failed(error) => panic!("decoding failed: {error}"),
            _ => {}
        }
    }
}

#[test]
fn a_rested_scrubber_finds_the_frames_asked_for() {
    let path = write_clip();
    let options = Options {
        output: Output::Yuv,
        // Nothing worth speaking of is cached, so the pictures after the rest are decoded.
        cache_bytes: 0,
        resting_cache_bytes: 0,
        ..Options::default()
    };
    let (sender, events) = mpsc::channel();
    let mut scrubber = Scrubber::open(&path, options, move |event| {
        let _ = sender.send(event);
    })
    .expect("the scrubber starts");
    match events.recv_timeout(TIMEOUT).expect("the clip opens") {
        Event::Opened(info) => assert!(info.frame_count >= FRAMES as u64),
        Event::Failed(error) => panic!("the clip did not open: {error}"),
        Event::Picture(_) => panic!("a picture before the clip opened"),
    }

    let request = scrubber.request(30);
    assert_eq!(exact_picture(&events, request), 30);
    for frame in [45, 10] {
        scrubber.rest();
        thread::sleep(REST_WAIT);
        let request = scrubber.request(frame);
        assert_eq!(exact_picture(&events, request), frame);
    }
    // Asked again before the rest begins: it carries on.
    scrubber.rest();
    let request = scrubber.request(12);
    assert_eq!(exact_picture(&events, request), 12);

    drop(scrubber);
    let _ = std::fs::remove_file(&path);
}
