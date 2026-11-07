# Implementation plan

Core functionality is complete. The current local work extends URL input and playlist handling; external-service and platform checks remain listed separately below. Versioning follows [AGENTS.md](../AGENTS.md). Design and coding rules live in [ARCHITECTURE.md](ARCHITECTURE.md) and [GUIDELINES.md](GUIDELINES.md).

## Milestones

| Stage | Scope | Status |
|---|---|---|
| Design | Backend alternatives, typed boundaries, coding guidelines | Complete |
| 0 | Validate ONNX exports, STFT, DirectML behavior, DLLs and performance | Complete |
| 1 | Core/app workspace, domain types, localization, initial window | Complete |
| 2 | Decode, resample, float WAV output, atomic partial-file ownership | Complete |
| 3 | Pinned model registry, verified cache, downloads and cancellation | Complete |
| 4 | HTDemucs pipeline, overlap-add, GPU plausibility checks and CPU fallback | Complete |
| 5 | Screen state machine, background jobs, progress, timing and overwrite confirmation | Complete |
| 6 | Russian UI, persistent settings, drag/drop, Windows packaging | Complete |
| 7 | Selected-stem mixes and optional separate stem output | Complete |
| 8 | Reverb HQ cleanup | Complete |
| 9 | Vocal-bleed cleanup and independent Stems/Mix targets | Complete |
| Platform/release | Apple Silicon CoreML/DMG, tagged Windows/macOS builds, release 1.0.0 | Complete |
| 10 | Separate media crate, URL inspection/downloads and playlist selection | Implemented locally |
| 11 | Native audio downloads, per-item progress/errors, two-file splitting and compact playlist controls | Implemented locally |
| 12 | Saved Original/WAV/MP3 audio and WAV/MP3 stems/mixes, persistent format selectors | Implemented locally |
| 13 | Settings page, media-tool management, selectable shared cache folder, saved parallel-track limit and muted error colors | Complete locally |
| Documentation | English project/release prose, retained Russian interface translations | Complete |

## Current URL and playlist workflow

- Files and URL are separate tabs. Selected local files remain independent of URL downloads. A destination folder is required for downloads and results, but not for metadata.
- Valid HTTP(S) input starts inspection after 500 ms without edits. Requests run off the UI thread, are cancelled when superseded, and cannot apply stale responses.
- The URL preview shows a loader, error, or one title row. A playlist row contains its title and total count on the left and the single panel toggle on the right. The sidebar has no duplicate close button.
- Successfully inspected playlists expose the right-hand selection panel. All entries are initially checked. Localized All/None controls and individual checkboxes update the selected count and exact download subset.
- For playlists with more than five entries, the first start opens a closed panel for review. A subsequent start while it is open proceeds. Five or fewer entries start directly. Empty selections disable starts with a visible reason.
- The sidebar scrolls within the application height and adds width only. Long titles truncate with tooltips. The lower model/stems/cleanup/device controls remain collapsible.
- Every selected item has its own queued/running/completed/failed progress row. A failed item is reported by name and skipped; later downloads and successful separations continue. Results retain per-file download failures.
- Downloads offer Original/WAV/MP3 beside the destination Browse button. Original keeps the source codec/container; WAV uses float32 and MP3 uses 320 kbit/s. The inference input retains source quality independently of this choice. Unsupported inference formats use disposable WAV decoding files in a shared cache directory, retained by their input owners and cleaned off the UI thread.
- Completed source files remain in the chosen destination after cancellation or failure. Partial files have RAII owners. Selected native files and prepared inputs can be reused while the URL, folder, selection and files still match.
- A second selector beside Split/Split and mix chooses WAV or MP3 for both stems and mixes. Overwrite checks and output paths use the selected extension; encoding completes before atomic publication. Both format choices are saved with the UI settings.
- Separation uses the saved parallel-track count (1-8, default 2), including cleanup jobs, with CPU threads shared between actual jobs. Model downloads happen once before the batch. GPU failures still fall back to CPU.
- yt-dlp and FFmpeg executables use the model cache. Metadata requires only yt-dlp; FFmpeg is installed when an inference input needs decoding. Cache deletion is blocked while tools are active.
- Persist language, model/device, stem/cleanup settings, active source tab, independent destination folders, window geometry and disclosure state. Do not persist URL text, selected files, playlist contents or checkboxes.

## Settings and tool management

- A gear beside the language selector opens Settings, retaining all input and playlist choices.
- Both languages expose the parallel-track count, yt-dlp/FFmpeg status, per-tool Download/Update/Delete actions, cancellation, and Back navigation.
- Open, choose and reset the shared models/cache folder in Settings. Persist the location and pass it to every metadata, source-download and inference worker. Opening creates a missing default directory off the UI thread; switching leaves existing cached files in place.
- Checked the dependency source: `yt-dlp` 2.10.0 installs only the latest release. Its optional string controls the executable filename. Release-version selection is not supported by its installer and is therefore not offered through this wrapper.
- Metadata work is cancelled and reaped asynchronously before tool management starts. Installation, updates, deletion and cache measurements run on a dedicated worker; batch starts and cache clearing cannot overlap this page.
- Updates stage the replacement before publication, preserving existing binaries on error/cancellation. Removal leaves the other tool and model files untouched.
- The saved concurrency applies to local and URL separation and cleanup, capped by track count. Invalid saved values fall back to two. Both themes use muted error foreground colors.

## Validation

Required local checks:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p stem-split
```

Latest Windows regression checks cover:

- 500 ms debounce, cancellation and stale-response rejection;
- single-item/flat-playlist parsing, source ordering, live/empty/invalid metadata;
- the 4/5/6-entry review boundary and interception of all three start actions;
- individual/All/None selections, empty selection, localized total/selected counts and input invalidation;
- fixed-height metadata/sidebar rendering in both locales, long titles and 100-entry lists;
- separate download progress rows, per-file failure results and continuing after a failed download;
- byte-identical native-format publication and the ordinary batch handoff;
- two-job scheduling and shared CPU threads, including both cleanup networks;
- format-key roundtrips/fallbacks, selected-extension overwrite checks, MP3 publication rollback on error/cancellation, visible format controls, and real cached-FFmpeg encoding/decoding.

Network/GPU/model-dependent tests remain ignored by default. No real site download, model inference run or macOS build is included in the current minimal local checks. The updated Windows executable launched, but live-window inspection failed twice with capture timeouts; headless egui rendering covers settings layout in both languages. Latest settings regression checks include saved concurrency bounds/roundtrip, CPU sharing with cleanup, tool deletion isolation, cancelled updates, background status refresh, cache-location persistence/reset/fallback, switching without moving files, batch-worker cache routing, and headless layout in both languages. Formatting, strict Clippy and 311 tests passed (106 app, 198 core, 7 media; 5 external-resource tests ignored); the Windows debug executable also builds. The cached-FFmpeg integration test was also run manually: generated MP3 and WAV files decoded successfully.

### Earlier acceptance evidence

| Area | Recorded evidence |
|---|---|
| Model source | HRSadeghi external-STFT exports selected; smank embedded inverse STFT fails on DirectML, gentij ORT cannot create a DirectML session |
| GPU correctness | Fusion disabled; CPU/GPU whole-track difference −86 to −92 dB relative to stem level; custom versus reference transforms checked |
| Audio | Bit-exact float WAV roundtrip; resampling frequency/duration checks; real MP3, FLAC, Vorbis and AAC checks. AAC retains a documented 1024-frame delay |
| Model store | Local HTTP fault/cancellation tests; real 110 MB 6s model fetched and verified in 40 s |
| Native UI | Split, overwrite, cancel, close, device changes, timing, errors and settings exercised in Windows release windows |
| Windows package | Isolated-folder/system-PATH launch loaded local DirectML and four VC++ DLLs; GPU split completed in 10 s. No clean Windows installation was available |
| macOS package | Apple Silicon bundle/image/checksum/signature/system-only dependencies and eight-second startup verified; macOS 12 minimum |
| Cleanup | Both MDX networks compared on CPU/GPU; cancellation preserved previous output and removed raw/partial files; listening quality remains unassessed |
| Releases | Published 0.1.0 and 1.0.0 descriptions and Windows/macOS packaged README files translated to English, preserving application binaries/resources and updating DMG checksums |

### Performance reference

Target Windows machine: Ryzen 5 2600, GTX 1660 SUPER 6 GB, 32 GB RAM. These are historical measurements, not new guarantees for the current workflow.

| Scenario | Result |
|---|---|
| One model chunk, DirectML | 0.283–0.294 s; about 1.4 GB per separation session |
| 106-second track, htdemucs | GPU 12.6 s / CPU 55.4 s |
| 106-second track, htdemucs_6s | GPU 10.7 s |
| 106-second track, htdemucs_ft | GPU 38.9 s |
| Two 6s files, GPU | 14.5 s parallel versus 21.3 s sequential |
| Two 6s files, CPU | 79–81 s parallel versus 97 s sequential |
| Cleanup session VRAM | About 2.3 GB Voc_FT / 3.6 GB Reverb HQ |
| Two cleanup jobs, 5:50 track, Both targets | Reverb HQ: 620 s parallel / 154 s sequential; Voc_FT: 120 s parallel / 142 s sequential |

The previous single-file cleanup limit was based on those VRAM measurements. The owner now explicitly requests two concurrent files; plausibility checks and CPU fallback remain enabled. Python Demucs comparison was excluded by the owner and is not planned.

## Remaining validation

- Exercise real playlists containing unavailable entries and cancellation midway through downloads; inspect native extensions/codecs and mixed success/failure results.
- Exercise download-only followed by reuse for separation, especially WebM/Opus sources and cache cleanup.
- Check the live window and compact sidebar on small monitors and both display languages.
- Remeasure two-file cleanup on available GPUs and verify native-download preparation on macOS.
- Complete macOS whole-track/CoreML comparisons, a clean macOS 12 install and a quarantined downloaded app; a clean Windows package launch also remains outstanding.

## Later

- RoFormer vocal/instrumental models behind the Separator trait.
- FLAC output for separated stems.
- Add files to a running queue.
- Random shifts as a quality option.
- Linux acceleration; macOS Intel/universal builds, Developer ID and notarization.
