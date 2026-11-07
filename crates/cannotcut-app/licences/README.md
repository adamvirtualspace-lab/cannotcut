# Licences

What the About screen (Menu > About and licences) shows, built into the app. When handing the
app out, give these files and the ones in `../fonts` and `../icons` alongside it as well.

- `FFmpeg-LGPL-3.0.txt`: FFmpeg's licence, copied from the FFmpeg build the app ships with
  (its DLLs sit beside `cannotcut.exe`; its source is at https://ffmpeg.org).
- `GPL-3.0.txt`: the GNU GPL 3.0, which the LGPL 3.0 adds its permissions to, from
  https://www.gnu.org/licenses/gpl-3.0.txt.
- `rust-libraries.txt`: every Rust library built into the app, with its licence and copyright
  lines, then the full licence texts. Made by `rust_libraries.py`; run it again after
  changing the dependencies.
