//! Media access for catcut: opening clips and decoding pictures for interactive scrubbing.
//!
//! Scrubbing works straight from the source file, without proxies. A [`Scrubber`] owns a decode
//! thread that always chases the most recent request: after a seek it first shows the keyframe it
//! landed on, then keeps decoding towards the requested frame unless a newer request arrives.

mod session;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use session::Session;

#[derive(Debug)]
pub struct Error(String);

impl Error {
    fn new(context: &str, cause: impl fmt::Display) -> Self {
        Self(format!("{context}: {cause}"))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ratio {
    pub num: i32,
    pub den: i32,
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Decode on the GPU when the codec and driver allow it.
    pub hardware: bool,
    /// Pictures are scaled down to fit inside this size; they are never scaled up.
    pub max_preview: (u32, u32),
}

impl Default for Options {
    fn default() -> Self {
        Self {
            hardware: true,
            max_preview: (1280, 720),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ClipInfo {
    pub path: PathBuf,
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub frame_rate: Ratio,
    pub frame_count: u64,
    /// Zero when the container carries no seek index.
    pub keyframe_count: usize,
    /// Name of the hardware decoder that was set up, if any.
    pub hardware: Option<&'static str>,
    pub preview_width: u32,
    pub preview_height: u32,
}

pub struct Picture {
    pub request: u64,
    pub requested_frame: u64,
    /// The frame this picture actually shows.
    pub frame: u64,
    /// False for the keyframe shown right after a seek, while the requested frame is still being
    /// decoded.
    pub exact: bool,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub timing: Timing,
}

#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// Time from the request being made to this picture being handed to the sink.
    pub latency: Duration,
    pub convert: Duration,
    pub seeked: bool,
    pub frames_decoded: u32,
    pub hardware: bool,
}

pub enum Event {
    Opened(ClipInfo),
    Picture(Picture),
    Failed(String),
}

struct Request {
    id: u64,
    frame: u64,
    at: Instant,
}

#[derive(Default)]
struct Slot {
    pending: Option<Request>,
    closed: bool,
}

/// Holds only the newest request; older ones are overwritten before the decoder ever sees them.
#[derive(Default)]
struct Mailbox {
    slot: Mutex<Slot>,
    arrived: Condvar,
}

impl Mailbox {
    fn post(&self, request: Request) {
        self.slot.lock().expect("mailbox lock poisoned").pending = Some(request);
        self.arrived.notify_one();
    }

    fn close(&self) {
        self.slot.lock().expect("mailbox lock poisoned").closed = true;
        self.arrived.notify_one();
    }

    fn take(&self) -> Option<Request> {
        self.slot.lock().expect("mailbox lock poisoned").pending.take()
    }

    /// Blocks until a request arrives; `None` once the scrubber is dropped.
    fn wait(&self) -> Option<Request> {
        let mut slot = self.slot.lock().expect("mailbox lock poisoned");
        loop {
            if slot.closed {
                return None;
            }
            if let Some(request) = slot.pending.take() {
                return Some(request);
            }
            slot = self.arrived.wait(slot).expect("mailbox lock poisoned");
        }
    }

    fn is_closed(&self) -> bool {
        self.slot.lock().expect("mailbox lock poisoned").closed
    }
}

pub struct Scrubber {
    mailbox: Arc<Mailbox>,
    next_request: u64,
}

impl Scrubber {
    /// Starts a decode thread for `path` and returns without waiting for the file to open; the
    /// result arrives as [`Event::Opened`] or [`Event::Failed`]. `sink` is called on the decode
    /// thread, so it should only hand the event off.
    pub fn open(
        path: &Path,
        options: Options,
        mut sink: impl FnMut(Event) + Send + 'static,
    ) -> Result<Self, Error> {
        let mailbox = Arc::new(Mailbox::default());
        thread::Builder::new()
            .name("catcut-scrub".to_string())
            .spawn({
                let mailbox = mailbox.clone();
                let path = path.to_path_buf();
                // FFmpeg contexts are created and used on this thread only.
                move || match Session::open(&path, options) {
                    Ok(session) => {
                        sink(Event::Opened(session.info().clone()));
                        Worker {
                            session,
                            mailbox,
                            sink: Box::new(sink),
                        }
                        .run();
                    }
                    Err(error) => sink(Event::Failed(error.to_string())),
                }
            })
            .map_err(|error| Error::new("start decode thread", error))?;
        Ok(Self {
            mailbox,
            next_request: 0,
        })
    }

    /// Asks for `frame`, replacing any request the decoder has not started on yet.
    pub fn request(&mut self, frame: u64) -> u64 {
        self.next_request += 1;
        self.mailbox.post(Request {
            id: self.next_request,
            frame,
            at: Instant::now(),
        });
        self.next_request
    }
}

impl Drop for Scrubber {
    fn drop(&mut self) {
        // The decode thread notices between frames and exits by itself; waiting for it here would
        // stall whoever drops the scrubber, usually the UI.
        self.mailbox.close();
    }
}

enum Served {
    Done,
    Superseded(Request),
}

struct Worker {
    session: Session,
    mailbox: Arc<Mailbox>,
    sink: Box<dyn FnMut(Event) + Send>,
}

impl Worker {
    fn run(mut self) {
        while let Some(mut request) = self.mailbox.wait() {
            loop {
                match self.serve(&request) {
                    Ok(Served::Done) => break,
                    Ok(Served::Superseded(next)) => request = next,
                    Err(error) => {
                        (self.sink)(Event::Failed(error.to_string()));
                        break;
                    }
                }
            }
        }
    }

    fn serve(&mut self, request: &Request) -> Result<Served, Error> {
        let target = self.session.frame_to_pts(request.frame);
        let mut seeked = false;
        let mut decoded = 0;
        if !self.session.shows(target) {
            if self.session.needs_seek(target) {
                if !self.session.seek(target)? {
                    return Ok(Served::Done);
                }
                seeked = true;
                decoded = 1;
            }
            // Every seek owes the viewer one picture, so fast dragging still shows keyframes.
            let mut preview_owed = seeked;
            // A seek leaves its keyframe decoded; otherwise the next frame is still to come.
            let mut frame_ready = seeked;
            loop {
                if !frame_ready {
                    if !self.session.advance()? {
                        // Ran off the end of the stream; there is nothing to show for this
                        // request.
                        return Ok(Served::Done);
                    }
                    decoded += 1;
                }
                frame_ready = false;
                if self.session.reached(target) {
                    break;
                }
                if preview_owed {
                    self.deliver(request, false, seeked, decoded)?;
                    preview_owed = false;
                }
                if let Some(next) = self.mailbox.take() {
                    return Ok(Served::Superseded(next));
                }
                if self.mailbox.is_closed() {
                    return Ok(Served::Done);
                }
            }
        }
        self.deliver(request, true, seeked, decoded)?;
        Ok(Served::Done)
    }

    fn deliver(
        &mut self,
        request: &Request,
        exact: bool,
        seeked: bool,
        frames_decoded: u32,
    ) -> Result<(), Error> {
        let started = Instant::now();
        let converted = self.session.picture()?;
        (self.sink)(Event::Picture(Picture {
            request: request.id,
            requested_frame: request.frame,
            frame: converted.frame,
            exact,
            width: converted.width,
            height: converted.height,
            rgba: converted.rgba,
            timing: Timing {
                latency: request.at.elapsed(),
                convert: started.elapsed(),
                seeked,
                frames_decoded,
                hardware: converted.hardware,
            },
        }));
        Ok(())
    }
}
