# stem-split

A simple desktop application for splitting audio into stems: vocals, drums, bass, and other instruments. Written in Rust with an egui interface. Neural-network inference runs locally; audio files are not uploaded.

> **Status:** the original plan is complete. The application packages as a Windows ZIP and an Apple Silicon macOS DMG. See [docs/PLAN.md](docs/PLAN.md) for progress and validation results.

## Features

- Choose one or more MP3, FLAC, WAV, OGG, or M4A files using a dialog, drag and drop, or command-line arguments. Files are processed two at a time.
- On the URL tab, enter the HTTP(S) address of a recording or playlist. After 500 ms without edits, a loading indicator appears, followed by the title. For a playlist, the right-aligned Playlist toggle beside its title opens a panel with a checkbox for every entry. All entries are selected initially; All/None checkboxes select or clear the whole list. Playlist titles show the total track count, and the panel also shows how many are selected. The list scrolls within the current window height. For playlists with more than five entries, starting Download or Split first opens the selection panel; press the action again after reviewing it. Playlists with five or fewer entries start directly. Choose a destination folder to download the best available audio for each selected entry, then run the usual separation, mixing, and cleanup. Live streams are not supported. Every entry has its own progress bar and error result. Unavailable entries are skipped while the others continue, using the normal cancellation button. Choose Original, WAV, or MP3 beside the destination Browse button. Original keeps the source codec/container; WAV uses 32-bit float and MP3 uses 320 kbit/s. Temporary decoding inputs preserve source quality independently of the saved format.
- Download saves the source audio files without separation. After returning to the form, you can split or mix them without downloading again, provided the URL and folder have not changed and all downloaded files still exist.
- Download sits next to the URL clear button. Collapse the Separation and mix section to show only the source and destination controls. The section state is remembered, and the window adjusts to its size.
- Choose where results are saved; local files default to their source folder.
- Choose a model:

  | Model | Stems | When to choose it |
  |---|---|---|
  | `htdemucs` | drums, bass, other, vocals | Default and fastest |
  | `htdemucs_ft` | the same four | Slightly better quality, about four times slower |
  | `htdemucs_6s` | the same four, plus guitar and piano | Separate guitar and piano |

- Mix selected stems into one file: use the stem checkboxes, All or None, then press Split and mix. The output is `song.mix.wav`, for example an instrumental mix. Save stems also writes every stem to its own file.
- Clean up separation artifacts with a second neural network. Enable Clean up artifacts, select Stems, The mix, or both, and choose a network:

  | Network | Removes |
  |---|---|
  | Reverb HQ | Reverb and smeared tails |
  | Vocal bleed (`UVR-MDX-NET-Voc_FT`) | Vocals leaking into instrumental stems, and instruments leaking into vocals |

  The vocal network cannot clean a mix containing both vocals and instruments. The mix checkbox is unavailable for that combination; individual stems can still be cleaned.
- View progress and elapsed time for each file and the whole batch, cancel processing, and see each file's outcome and duration.
- Choose WAV or MP3 to the right of Split/Split and mix. Stems and mixes use the selected extension: 32-bit float WAV or 320 kbit/s MP3 at 44.1 kHz. Download and output format choices are remembered independently.
- Use English or Russian. Language, model, device, active tab, each tab's destination folder, selected stems, and cleanup settings are remembered. Selected files and URL text are not saved between launches.
- Open or clear the shared cache using the small buttons beside the model selector. yt-dlp and FFmpeg download on first URL use into the model directory and are cleared together with the models.

Limits: mono or stereo audio, up to three hours. With M4A/AAC, results can start 20–50 ms later than the source in other applications because the decoder does not remove encoder delay.

Models download on first use and remain cached: `htdemucs` is 169 MB, `htdemucs_6s` is 110 MB, and `htdemucs_ft` is 675 MB. Cleanup networks are about 67 MB each. Every model file is verified with SHA-256. The cache is `%LOCALAPPDATA%\stem-split\cache\models` on Windows and `~/Library/Caches/stem-split/models` on macOS.

Cleanup adds processing time: about 0.5 s for Reverb HQ or 0.25 s for vocal bleed on the GPU per 6–12 seconds of audio in each cleaned stem or mix; CPU processing takes 15–20 times longer. Batches, including cleanup, run two files at a time. These measurements are from Windows; cleanup has not been validated on macOS.

## How it works

Inference uses [ONNX Runtime](https://onnxruntime.ai/) with [HTDemucs](https://github.com/facebookresearch/demucs) models from an [ONNX export](https://huggingface.co/HRSadeghi/demucs-onnx). Cleanup uses MDX-Net models from [UVR](https://github.com/Anjok07/ultimatevocalremovergui), distributed in [this model repository](https://huggingface.co/Politrees/UVR_resources). Windows uses DirectML; macOS uses CoreML with access to the GPU and Neural Engine. Failed acceleration falls back to the CPU. CoreML may execute some graph operators on the CPU, and performance depends on the model. Local-file processing needs neither Python nor FFmpeg. For URLs, the application installs yt-dlp and FFmpeg through the [yt-dlp crate](https://docs.rs/yt-dlp/latest/yt_dlp/).

On the test machine, a Ryzen 5 2600 with a GTX 1660 SUPER, `htdemucs` separates a 106-second track in 13 s on the GPU or 55 s on the CPU. `htdemucs_ft` takes 39 s on the GPU.

The core example provides the same separation without a window:

```sh
cargo run --release -p stem-split-core --example separate -- song.flac --model htdemucs
# Mix drums and bass; clean both the stems and mix with the reverb network.
cargo run --release -p stem-split-core --example separate -- song.flac \
    --mix drums,bass --cleanup reverb_hq --cleanup-target both
```

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for details.

## Requirements

- 64-bit Windows 10 or 11, or macOS 12 or later on Apple Silicon (M1 or newer). The macOS package does not support Intel. Linux is not currently validated.
- DirectX 12 graphics hardware for Windows acceleration; CoreML on macOS. Without acceleration, processing uses the CPU and takes longer.
- Rust 1.96 or later for building; the repository pins its validated toolchain in `rust-toolchain.toml`.

## Install a release

Download your platform's package from [GitHub Releases](https://github.com/bromzh/stem-split/releases).

On Windows, extract the ZIP and keep all files beside `stem-split.exe`.

On macOS, open the DMG, drag `stem-split.app` to the Applications shortcut, eject the image, and open the app from Applications. The image contains English installation instructions in `README.txt`. The build is ad-hoc signed, without an Apple Developer ID signature or notarization, so Gatekeeper may block it:

1. Try opening the installed application.
2. On macOS 13 or later, open System Settings > Privacy & Security. On macOS 12, open System Preferences > Security & Privacy > General.
3. Find the message about stem-split, click Open Anyway, and confirm Open.

If a trusted download is still reported as damaged, `README.txt` explains how to remove quarantine from this application only. Do not disable Gatekeeper globally.

## Build and run

```sh
cargo run --release
```

Pass a file directly with `cargo run --release -- song.flac`.

## Package a release

### Windows

```powershell
powershell -ExecutionPolicy Bypass -File tools\package.ps1
```

The script creates `dist\stem-split-<version>-windows-x64.zip` with everything needed on a machine without Rust or Visual Studio: `stem-split.exe`, `DirectML.dll`, four Visual C++ runtime libraries, and `README.txt`. Keep the archive's files together.

Packaging requires Visual Studio or Build Tools, which supply the redistributable Visual C++ libraries.

### macOS / Apple Silicon

```sh
bash tools/package-macos.sh
```

Requires Rust and Xcode Command Line Tools (`xcode-select --install`). Run on Apple Silicon without Rosetta. The script builds with `--locked` for `aarch64-apple-darwin` and creates `dist/stem-split-<version>-macos-arm64.dmg` plus its `.dmg.sha256` file. The DMG contains `stem-split.app`, an Applications shortcut, the license, and English installation and Gatekeeper instructions.

The app has one executable with ONNX Runtime linked statically; no separate third-party dylibs are needed. The script checks dependencies and the ad-hoc signature. Apple notarization is not performed. Models are not bundled and download on first use.

Application directories:

| Data | Windows | macOS |
|---|---|---|
| Models and download tools | `%LOCALAPPDATA%\stem-split\cache\models` | `~/Library/Caches/stem-split/models` |
| Settings and window position | `%APPDATA%\stem-split\data\app.ron` | `~/Library/Application Support/stem-split/app.ron` |
| Latest launch log | `%LOCALAPPDATA%\stem-split\data\stem-split.log` | `~/Library/Application Support/stem-split/stem-split.log` |

Pushing a `v<version>` tag also runs both platform builds in GitHub Actions. After validation, the ZIP, DMG, and DMG checksum are attached to the release. The tag version must match the root `Cargo.toml`.

## Development

Bump the version once per local set of changes, then keep it until that set has been committed and successfully pushed. Further local tasks and commits without a push do not cause another bump. After pushing the previous set, features increment the minor version and reset the patch version; fixes increment the patch version. The workspace version lives in the root `Cargo.toml` and is synchronized with `Cargo.lock`. Documentation-only changes do not bump it. The current local version is 1.2.0.

Before committing, all checks must pass without warnings:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The workspace has three crates: `crates/core`, the separation pipeline without a GUI; `crates/media`, URL audio downloads and tool installation; and `crates/app`, the egui window. Standalone experiments in `spikes/` are excluded from the application workspace.

See [docs/GUIDELINES.md](docs/GUIDELINES.md). The essential rules are:

- No `unwrap`, `expect`, `panic!`, or indexing that can panic.
- Express invariants through types so the compiler catches invalid states.
- No `unsafe`.

These rules are enforced by lints in [Cargo.toml](Cargo.toml).

## Plan

The detailed plan and acceptance criteria are in [docs/PLAN.md](docs/PLAN.md).

0. Validate HTDemucs ONNX inference through `ort` and DirectML on the target machine. **Complete.**
1. Workspace skeleton, two initial crates, and domain types. **Complete.**
2. Audio decoding, resampling, and WAV output. **Complete.**
3. Model registry, downloads, and cache. **Complete.**
4. Complete separation pipeline with progress and cancellation. **Complete.**
5. Interface. **Complete.**
6. Polish and release packaging. **Complete.**
7. Mix selected stems. **Complete.**
8. Reverb HQ artifact cleanup. **Complete.**
9. Second cleanup network and target checkboxes. **Complete.**
10. Preview URL metadata and download recordings or playlists through yt-dlp. **Implemented locally in 1.2.0.**

## Licenses

The project's own code is distributed under [MIT](LICENSE), which permits use, modification, and redistribution with few restrictions.

The local 1.2.0 URL integration uses `yt-dlp` 2.10 under GPL-3.0-only. The MIT license of the project's own sources does not replace this dependency's license. Downloaded yt-dlp and FFmpeg binaries also have their own licensing terms.

Neural-network weights are not included in the repository. The application downloads them from the [ONNX export](https://huggingface.co/HRSadeghi/demucs-onnx) and [UVR model collection](https://huggingface.co/Politrees/UVR_resources), independently of the project's license. MDX-Net models are distributed with UVR, whose code is MIT, but no separate weight-license statement is available. Demucs code is MIT; its repository likewise has no separate statement for weights, and some third-party exports treat them as CC BY-NC 4.0 for non-commercial use. The application is therefore intended for personal, non-commercial use. The model weights, rather than the project's MIT license, impose that limitation.
