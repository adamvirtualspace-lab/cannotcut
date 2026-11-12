<h1 align="center">cannotcut</h1>

<p align="center">
  <b>A fast, simple video editor for Windows.</b><br>
  Cut clips, stack layers, add titles and transitions, and export to MP4.
</p>

<p align="center">
  <a href="https://github.com/adamvirtualspace-lab/cannotcut/actions/workflows/windows.yml"><img alt="Windows build" src="https://github.com/adamvirtualspace-lab/cannotcut/actions/workflows/windows.yml/badge.svg"></a>
  <a href="#licence"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue"></a>
</p>

<p align="center">
  <img src="docs/images/editor.png" alt="cannotcut editing a project with a title and a picture-in-picture over gameplay footage" width="100%"><br>
  <sub>Gameplay footage from <a href="https://www.youtube.com/watch?v=_KMj_SG7-Vc">AdamRoadTrips</a>.</sub>
</p>

## Get it

1. Open the [latest build](https://github.com/adamvirtualspace-lab/cannotcut/actions/workflows/windows.yml?query=branch%3Amain) (pick the newest green run).
2. Download **cannotcut-windows-…** under *Artifacts* and unzip it.
3. Run `cannotcut.exe`. Keep the `.dll` files next to it.

## What it can do

**Edit**
- Put clips on a timeline with as many tracks as you like
- Move, trim, split and delete clips
- Undo and redo (up to 500 steps)

**Make it look good**
- Layer videos and move, resize, rotate or fade each one
- Animate those changes with keyframes
- Add text titles and transitions (fade or wipe)
- Speed clips up or slow them down (0.1× to 10×)

**Finish**
- Export to MP4, up to 4K
- Save projects as `.otio` files, which other editors can open too

**Make it yours**
- Move panels around, hide them, or drag them into their own windows
- Pick your accent colour, font, and UI size in *Menu → Appearance*
- Works on small screens too:

<p align="center">
  <img src="docs/images/compact.png" alt="cannotcut in a small window, halfway through a wipe transition" width="60%">
</p>

**Coming soon:** colour correction and filters.

## Keyboard shortcuts

| Keys | What it does |
|---|---|
| <kbd>Space</kbd> | Play / pause |
| <kbd>←</kbd> <kbd>→</kbd> | Step one frame |
| <kbd>Ctrl</kbd>+<kbd>B</kbd> | Split the clip at the playhead |
| <kbd>Delete</kbd> | Delete selected clips |
| <kbd>Ctrl</kbd>+<kbd>Z</kbd> / <kbd>Ctrl</kbd>+<kbd>Y</kbd> | Undo / redo |
| <kbd>Ctrl</kbd>+<kbd>S</kbd> | Save |
| <kbd>Ctrl</kbd>+<kbd>[</kbd> / <kbd>]</kbd> | Hide or show the left / right panel |
| <kbd>Ctrl</kbd>+mouse wheel | Zoom the timeline |

## Build it yourself

You need Windows 10 or 11, and these four things:

1. [Rust](https://rustup.rs)
2. [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) (pick *Desktop development with C++*)
3. [LLVM](https://github.com/llvm/llvm-project/releases) (the `LLVM-…-win64.exe` installer)
4. [FFmpeg](https://github.com/BtbN/FFmpeg-Builds/releases/tag/latest): get `ffmpeg-n8.1-latest-win64-lgpl-shared-8.1.zip` and unzip it to `C:\dev\ffmpeg`

Tell Rust where FFmpeg and LLVM are (run once in PowerShell):

```powershell
[Environment]::SetEnvironmentVariable('FFMPEG_DIR', 'C:\dev\ffmpeg', 'User')
[Environment]::SetEnvironmentVariable('LIBCLANG_PATH', 'C:\Program Files\LLVM\bin', 'User')
[Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path', 'User') + ';C:\dev\ffmpeg\bin', 'User')
```

Then open a **new** terminal in this folder and run:

```powershell
cargo run --release -p cannotcut-app
```

The first build takes 10–15 minutes. After that it's quick.

<details>
<summary><b>Something went wrong?</b></summary>

- **Download stuck at "Updating crates.io index"**: add this to `%USERPROFILE%\.cargo\config.toml`:
  ```toml
  [http]
  multiplexing = false
  timeout = 60
  [net]
  retry = 10
  ```
- **Build fails in `ffmpeg-sys-next`**: check the two paths above, then open a new terminal.
- **"avcodec-62.dll was not found"**: FFmpeg's `bin` folder isn't on your `Path`. Copy its DLLs next to `cannotcut.exe` instead.

</details>

<details>
<summary><b>For developers</b></summary>

- Run the tests: `cargo test --workspace`
- Make a zip to share: `powershell -ExecutionPolicy Bypass -File package.ps1` (lands in `target\dist`)
- Changed dependencies? Update the licence list: `python crates/cannotcut-app/licences/rust_libraries.py`
- Command-line options: a project or media files to open, `--at=<frame>`, `--play`, `--export=<file.mp4>`, `--no-sound`, `--cpu-viewer`
- Set `CANNOTCUT_FRAME_LOG=<file>` to log how long each frame takes to draw

| Folder | What's in it |
|---|---|
| `crates/cannotcut-app` | The editor window |
| `crates/cannotcut-timeline` | Clips, tracks, undo, and `.otio` files |
| `crates/cannotcut-media` | Reading and writing video (FFmpeg) |
| `crates/cannotcut-audio` | Mixing and playing sound |
| `crates/cannotcut-export` | Exporting to MP4 |

</details>

## Licence

cannotcut is [MIT licensed](LICENSE). It also uses FFmpeg (LGPL 3.0), the fonts Anton SC, Antonio, Oswald, Inter and Roboto (SIL OFL 1.1), and [Tabler Icons](https://tabler.io/icons) (MIT). See *Menu → About and licences* in the app for everything.
