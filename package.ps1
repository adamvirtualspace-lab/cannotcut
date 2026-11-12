# Builds cannotcut for release and gathers what it needs into one folder, and a .zip of it, to
# hand to someone: the program, the FFmpeg libraries it reads and writes video with, and every
# licence those and the rest of it come under.
#
#   powershell -ExecutionPolicy Bypass -File package.ps1
#
# Needs the same setup as any build (CARGO_HOME, FFMPEG_DIR and the rest, see the project
# notes). The build runs at low priority and on 3 jobs so that the laptop stays usable; it takes
# a while the first time. The result goes under $env:CARGO_TARGET_DIR\dist (or target\dist).

param([switch]$SkipBuild)

$ErrorActionPreference = "Stop"
$root = $PSScriptRoot
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root "target" }
$version = (Select-String -Path (Join-Path $root "Cargo.toml") -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$name = "cannotcut-$version-windows"
$dist = Join-Path $target "dist"
$out = Join-Path $dist $name

if (-not $SkipBuild) {
    Push-Location $root
    try {
        cmd /c "start /belownormal /wait /b cargo build --release -p cannotcut-app -j 3"
        if ($LASTEXITCODE -ne 0) { throw "the build failed" }
    } finally {
        Pop-Location
    }
}

$exe = Join-Path $target "release\cannotcut.exe"
if (-not (Test-Path $exe)) { throw "no release build at $exe" }
if (-not $env:FFMPEG_DIR) { throw "FFMPEG_DIR is not set" }

if (Test-Path $out) { Remove-Item -Recurse -Force $out }
New-Item -ItemType Directory -Force (Join-Path $out "licences") | Out-Null

Copy-Item $exe $out
# The FFmpeg libraries, beside the program, where Windows looks for them first.
Get-ChildItem (Join-Path $env:FFMPEG_DIR "bin") -Filter *.dll | Copy-Item -Destination $out

$app = Join-Path $root "crates\cannotcut-app"
$licences = Join-Path $out "licences"
Copy-Item (Join-Path $app "licences\*.txt") $licences
Copy-Item (Join-Path $app "fonts\*-OFL.txt") $licences
Copy-Item (Join-Path $app "icons\LICENSE") (Join-Path $licences "TablerIcons-MIT.txt")

@"
cannotcut $version

A video editor. Start it with cannotcut.exe; keep the .dll files beside it, as it needs them.

It is built with the work of others, under the licences in the licences folder (also shown in
the app: Menu > About and licences):

- FFmpeg (the av*.dll and sw*.dll files): GNU LGPL 3.0, see FFmpeg-LGPL-3.0.txt and
  GPL-3.0.txt. Its source is at https://ffmpeg.org.
- The fonts Anton SC (changed so that its small capitals show), Antonio, Oswald, Inter and
  Roboto: SIL Open Font License 1.1, see the *-OFL.txt files.
- Tabler Icons: MIT, see TablerIcons-MIT.txt.
- The Rust libraries it is made of: see rust-libraries.txt.
"@ | Set-Content -Path (Join-Path $out "README.txt") -Encoding utf8

$zip = Join-Path $dist "$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path $out -DestinationPath $zip

$size = (Get-Item $zip).Length / 1MB
"Packaged in $out"
"and $zip ({0:N1} MB)" -f $size
