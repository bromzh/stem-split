# Changelog

Major milestones and features, grouped by their implementation dates in Git (including branch history). Version headings identify development milestones; published releases are marked explicitly.

## 2026-11-08 - 2.0.1: Crate separation (unreleased)

- Split the workspace into audio, inference, separation, enhancement, media-fetching, core and app crates.
- Isolated ONNX Runtime sessions/providers behind a shared borrowed-tensor interface, retaining independent HTDemucs and MDX-Net algorithms and catalogs.
- Moved FFmpeg installation/conversion/encoding to audio, with optional external-tool dependencies; renamed the URL crate to media-fetching and removed its dependency on core.
- Moved workers, metadata preview, tool/cache operations, progress tracking and timing into headless core services. UI supplies repaint callbacks and retains dialogs, localization and settings persistence.
- Preserved model identifiers, saved settings, cache filenames, headless example commands and release-package names.

## 2026-11-08 - 2.0.0: Release milestone

This release brings together the URL/audio-format and settings work recorded below for 1.2.0 and 1.3.0.

- Download recordings or selected playlist entries, inspect metadata automatically, reuse downloaded audio and continue after individual failures.
- Save original audio, WAV or MP3 downloads, and choose WAV/MP3 output for stems and mixes.
- Manage yt-dlp/FFmpeg, choose the shared models/cache folder and configure 1-8 parallel tracks from the new settings page.
- Retain English/Russian localization, compact playlist controls, saved choices, background cancellation and muted error colors.
- Prepare Windows x64 ZIP and Apple Silicon macOS DMG packages through the tagged release workflow.

## 2026-11-08 — 1.3.0: Settings

- Added a settings page, opened by a gear beside the language selector, with English and Russian translations.
- Added installation status, download, update, removal and cancellation for yt-dlp and FFmpeg, with background workers.
- Added a saved parallel-track limit of 1–8 (default 2), applied to separation and cleanup with CPU threads shared between active tracks.
- Added opening, choosing and resetting the shared models/cache folder. The saved folder applies to models, media tools and temporary decoding files; existing files remain in their previous location.
- Softened error colors in light and dark themes.

## 2026-11-07 — 1.2.0: URLs, playlists and audio formats

- Added separate Files and URL tabs, validated HTTP(S) sources, automatic debounced metadata previews and cancellation of superseded requests.
- Added playlist selection with individual checkboxes, All/None controls, track counts, a scrolling sidebar and review before starting playlists with more than five entries.
- Added best-quality audio downloads through automatically installed yt-dlp/FFmpeg, download-only mode and reuse of downloaded tracks for later separation.
- Added per-entry progress and results; unavailable entries fail independently while remaining downloads and successful separations continue.
- Added Original, float WAV and 320 kbit/s MP3 source downloads, plus WAV/MP3 stems and mixes. Temporary decoding preserves source quality; completed source downloads survive cancellation or failure.
- Added saved format selectors, independent destination folders, remembered source/disclosure state, compact download controls and automatic window fitting.
- Enabled two concurrent tracks for cleanup as well as separation.
- Standardized project documentation and packaged instructions in English while retaining the Russian interface.

## 2026-11-06 — 1.0.0 release: Mixing and cleanup

- Added mixes of selected stems, All/None selection and optional separate stem output, with saved mix settings.
- Added Reverb HQ and Vocal Bleed (UVR-MDX-NET-Voc_FT) cleanup, independently targeting stems, the mix or both. Unsupported vocal/instrumental mixes are rejected by the vocal cleanup network.
- Added cleanup progress/time estimates, CPU fallback, verified cleanup-model downloads and automatic removal of temporary raw stems after cancellation or failure.
- Published version 1.0.0 and added tagged Windows/macOS release builds, validation and package uploads through GitHub Actions.
- Pinned the validated Rust toolchain and translated packaged installation/Gatekeeper instructions into English.

## 2026-11-05 — Cleanup validation

- Added an MDX-Net probe to validate artifact-cleanup models on DirectML and CPU before application integration.

## 2026-10-14 — Desktop polish, batches and macOS

- Added Russian localization, automatic system-language selection and the language menu.
- Added persistent language/model/device/output/window settings, drag and drop, and controls to open or clear downloaded models.
- Added multiple-file selection, two-track batch processing, shared CPU threads, output-name conflict detection, per-file results and batch time estimates.
- Added Windows ZIP packaging with the application icon, executable version, DirectML/Visual C++ libraries, file logging and startup-error dialogs; published the initial 0.1.0 release.
- Added Apple Silicon CoreML acceleration with CPU fallback and macOS DMG packaging with an ad-hoc-signed app and Gatekeeper instructions.
- Added the MIT license for the project's own code.

## 2026-10-13 — Working audio separation

- Validated HTDemucs ONNX exports on DirectML and CPU, selected host-side STFT/iSTFT and disabled incorrect DirectML graph fusion.
- Added the Rust core/application workspace, validated domain types and an egui desktop window.
- Added MP3, FLAC, WAV, OGG/Vorbis and M4A/AAC/ALAC decoding, mono-to-stereo conversion, 44.1 kHz resampling and atomic 32-bit float WAV output.
- Added pinned htdemucs, htdemucs_ft and htdemucs_6s models, automatic cancellable downloads, SHA-256 verification and a reusable per-user cache.
- Added end-to-end overlapping-chunk separation, output plausibility checks, GPU-to-CPU fallback and the headless `separate` example.
- Added file/output/model/device selection, native dialogs, command-line file input, background jobs, progress, cancellation, elapsed/remaining time, overwrite confirmation, results and localized error messages.

## 2026-10-12 — Project foundation

- Established the architecture, implementation stages and coding rules: typed invariants/errors, no unsafe code or panics, strict lints and background processing.
