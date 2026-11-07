stem-split — macOS / Apple Silicon
=================================


Requirements: Apple Silicon Mac (M1 or newer), macOS 12 or newer.
This is not an Intel or universal build. Rust and Python are not needed.
yt-dlp and FFmpeg install automatically for URL downloads/audio encoding.
The application contains one executable with ONNX Runtime linked into it;
only macOS system frameworks are loaded. Models are downloaded on first use.
Audio is processed locally. CoreML can use the GPU and Neural Engine; if the
provider fails, processing falls back to the CPU. Acceleration varies by model.

Installation
1. Download the DMG from https://github.com/bromzh/stem-split/releases.
2. Open the DMG and drag stem-split.app onto the Applications shortcut.
3. Eject the disk image and open stem-split from Applications.

Gatekeeper / first launch
This build is ad-hoc signed, NOT signed with an Apple Developer ID and NOT
notarized by Apple. macOS may therefore block a downloaded copy.
Only allow this application if you trust its source.

Try opening the installed app once. If macOS blocks it:
- On macOS 13 or newer, open System Settings > Privacy & Security.
- On macOS 12, open System Preferences > Security & Privacy > General.
Find the message about stem-split, click "Open Anyway", then confirm "Open".
You may need to authenticate. On older macOS versions you can also try
Control-clicking the app in Finder and choosing Open, then Open again.

If macOS reports that the app is "damaged", first download it again from the
official release. To check the download against the accompanying .sha256 file,
place both files in the same folder, open Terminal there and run:
  shasum -a 256 -c stem-split-@VERSION@-macos-arm64.dmg.sha256
If you trust the download and it is still blocked, remove quarantine ONLY
from this installed application in Terminal (no sudo is needed):
  xattr -dr com.apple.quarantine "/Applications/stem-split.app"
Then open the app again. If you installed it elsewhere, adjust this path.
Do not disable Gatekeeper globally. Managed Macs may require administrator
approval; these steps do not override your organization's security policy.

Default models/cache: ~/Library/Caches/stem-split/models
Log: ~/Library/Application Support/stem-split/stem-split.log
Use the gear beside the language selector to manage yt-dlp/FFmpeg, choose or
open the models/cache folder, and configure 1-8 parallel tracks (default: 2).
The URL tab supports recordings and playlist selection. Choose Original/WAV/MP3
for downloaded audio and WAV/MP3 for separated stems and selected-stem mixes.
Code: MIT (see LICENSE.txt). Model weights have separate licensing terms;
this application is intended for personal, non-commercial use. URL tools
have their own licensing terms; the Rust yt-dlp dependency is GPL-3.0-only.
