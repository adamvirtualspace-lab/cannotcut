//! Records what the screen shows and what the sound card plays, both stamped by the system clock,
//! to measure how far pictures and sound are apart (Windows only). Run it while cannotcut plays
//! a clip that flashes white on every click, such as `C:\dev\clips\sound_cuts.otio`.
//!
//! Usage: `cargo run -p cannotcut-audio --example av_offset -- <out prefix> <seconds> [x y]`
//!
//! `x` and `y` place the patch of screen that is watched, as fractions of the screen (default
//! 0.35 0.2, inside the viewer of a maximized window). Writes `<prefix>-screen.csv` (when each new
//! picture of the screen was presented, and how bright the patch was), `<prefix>.wav` and
//! `<prefix>-blocks.csv` (the sound, and when each block of it is played). Times are seconds of
//! the system's performance counter.

#[cfg(not(windows))]
fn main() {
    eprintln!("av_offset only works on Windows");
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    windows_only::run()
}

#[cfg(windows)]
mod windows_only {
    use std::fmt::Write as _;
    use std::fs;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
        D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
        ID3D11Texture2D,
    };
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIFactory1,
        IDXGIOutput1, IDXGIResource,
    };
    use windows::Win32::System::Performance::QueryPerformanceFrequency;
    use windows::core::Interface;

    /// Side of the square patch of screen that is watched, in pixels.
    const PATCH: u32 = 32;

    type Blocks = Arc<Mutex<Vec<(f64, Vec<f32>)>>>;

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let mut arguments = std::env::args().skip(1);
        let usage = "usage: av_offset <out prefix> <seconds> [x y]";
        let prefix = arguments.next().ok_or(usage)?;
        let seconds: f64 = arguments.next().ok_or(usage)?.parse()?;
        let x: f64 = arguments.next().map_or(Ok(0.35), |x| x.parse())?;
        let y: f64 = arguments.next().map_or(Ok(0.2), |y| y.parse())?;

        // The sound: recording an output device records what it plays. Its timestamps are the
        // performance counter in 100 ns steps.
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
                    let played = info.timestamp().capture.as_nanos() as f64 / 1e9;
                    blocks.lock().unwrap().push((played, data.to_vec()));
                }
            },
            |error| eprintln!("recording: {error}"),
            None,
        )?;
        recording.play()?;

        let mut screen = String::from("time,brightness\n");
        let pictures = unsafe { watch_screen(seconds, x, y, &mut screen)? };
        drop(recording);
        fs::write(format!("{prefix}-screen.csv"), screen)?;

        let blocks = blocks.lock().unwrap();
        let mut table = String::from("time,first_sample\n");
        let mut samples = Vec::new();
        for (played, data) in blocks.iter() {
            writeln!(table, "{played:.6},{}", samples.len() / 2)?;
            for frame in data.chunks_exact(channels) {
                samples.push(frame[0]);
                samples.push(*frame.get(1).unwrap_or(&frame[0]));
            }
        }
        fs::write(format!("{prefix}-blocks.csv"), table)?;
        fs::write(format!("{prefix}.wav"), wav(&samples, rate))?;
        println!(
            "{pictures} screen pictures, {:.2} s of sound at {rate} Hz",
            samples.len() as f64 / 2.0 / f64::from(rate)
        );
        Ok(())
    }

    /// Watches the patch at (`x`, `y`) of the first screen for `seconds`, writing a line to
    /// `out` for every picture presented. Returns how many there were.
    unsafe fn watch_screen(
        seconds: f64,
        x: f64,
        y: f64,
        out: &mut String,
    ) -> Result<usize, Box<dyn std::error::Error>> {
        unsafe {
            let mut frequency = 0;
            QueryPerformanceFrequency(&mut frequency)?;
            let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
            let adapter = factory.EnumAdapters1(0)?;
            let output: IDXGIOutput1 = adapter.EnumOutputs(0)?.cast()?;
            let mut device: Option<ID3D11Device> = None;
            let mut context: Option<ID3D11DeviceContext> = None;
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            let (device, context) = (device.ok_or("no device")?, context.ok_or("no context")?);
            let duplication = output.DuplicateOutput(&device)?;
            let described = duplication.GetDesc();
            let (width, height) = (described.ModeDesc.Width, described.ModeDesc.Height);
            println!(
                "screen {width}x{height} at {}/{} Hz",
                described.ModeDesc.RefreshRate.Numerator, described.ModeDesc.RefreshRate.Denominator
            );
            let left = ((f64::from(width) * x) as u32).min(width - PATCH);
            let top = ((f64::from(height) * y) as u32).min(height - PATCH);

            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: PATCH,
                Height: PATCH,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut staging: Option<ID3D11Texture2D> = None;
            device.CreateTexture2D(&staging_desc, None, Some(&mut staging))?;
            let staging = staging.ok_or("no staging texture")?;

            let started = Instant::now();
            let mut pictures = 0;
            while started.elapsed() < Duration::from_secs_f64(seconds) {
                let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
                let mut resource: Option<IDXGIResource> = None;
                match duplication.AcquireNextFrame(100, &mut info, &mut resource) {
                    Ok(()) => {}
                    Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => continue,
                    Err(error) => return Err(error.into()),
                }
                // Zero when only the mouse pointer moved.
                if info.LastPresentTime != 0
                    && let Some(resource) = &resource
                {
                    let texture: ID3D11Texture2D = resource.cast()?;
                    let patch = D3D11_BOX {
                        left,
                        top,
                        front: 0,
                        right: left + PATCH,
                        bottom: top + PATCH,
                        back: 1,
                    };
                    context.CopySubresourceRegion(&staging, 0, 0, 0, 0, &texture, 0, Some(&patch));
                    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                    context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
                    let mut sum = 0u64;
                    for row in 0..PATCH as usize {
                        let line = std::slice::from_raw_parts(
                            (mapped.pData as *const u8).add(row * mapped.RowPitch as usize),
                            PATCH as usize * 4,
                        );
                        sum += line.iter().map(|&byte| u64::from(byte)).sum::<u64>();
                    }
                    context.Unmap(&staging, 0);
                    let brightness = sum as f64 / f64::from(PATCH * PATCH * 4);
                    let presented = info.LastPresentTime as f64 / frequency as f64;
                    writeln!(out, "{presented:.6},{brightness:.1}")?;
                    pictures += 1;
                }
                drop(resource);
                duplication.ReleaseFrame()?;
            }
            Ok(pictures)
        }
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
}
