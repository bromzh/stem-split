# Architecture

This document describes the application. Dependency versions and the state of third-party projects were checked on 2026-10-12. Model selection and DirectML configuration were confirmed by stage 0 measurements, summarized in sections 8 and 11. Dated feature milestones are recorded in [CHANGELOG.md](../CHANGELOG.md).

## 1. Purpose and scope

`stem-split` is a desktop application that separates an audio file into stems such as vocals, drums, and bass using an open neural network.

The first version includes choosing an input file, choosing an output directory (the source folder by default), selecting an HTDemucs model, progress and cancellation, understandable errors, and automatic first-use model downloads.

Multiple files can be processed in one batch (section 13), but this is not a queue: new files cannot be added to a running batch.

Later additions include mixing selected stems into one file (section 9) and a second MDX-Net neural network for artifact cleanup after separation (section 16).

Out of scope: playback, a mixer with individual volume controls, model training, and separated-stem output formats other than WAV/MP3. Downloaded source audio retains its native format.

## 2. Key decisions

| Decision | Reason |
|---|---|
| ONNX Runtime through `ort`, with a custom pipeline | One executable plus runtime DLLs, with no Python or FFmpeg needed for local-file inference. Progress, cancellation, and errors use project types |
| HTDemucs models: `htdemucs`, `htdemucs_ft`, `htdemucs_6s` | A standard approach to 4–6 stem separation, with available ONNX exports |
| An ONNX export with STFT and iSTFT outside the graph | The only tested export that works with DirectML; exports embedding STFT failed on the GPU |
| DirectML acceleration with CPU fallback | Works with any DirectX 12 GPU, needs no CUDA installation, and is ten times faster than the CPU on the target machine |
| Disable DirectML graph fusion | With fusion enabled, DirectML silently returns incorrect values; see section 11 |
| Seven crates: audio, inference, separation, enhancement, media-fetching, core, and app | Audio and model families have independent APIs; core owns headless orchestration; only app depends on GUI libraries |
| eframe's glow/OpenGL renderer instead of default wgpu | For this form-based interface, the measured release executable is 6.8 MB instead of 13.6 MB, allocated memory is 120–180 MB instead of 490 MB, and startup is 0.6–0.8 s instead of 1.1 s |
| Worker threads and channels; a separate Tokio runtime only for URL downloads | Inference stays synchronous. The asynchronous `yt-dlp` API, tool installation, and subprocess control stay in `media-fetching`, off the UI thread |
| Accumulate the mix in memory and write it last | Works for both single-graph and per-stem-graph models, using memory comparable to the input |
| Cleanup is a separate post-separation stage using UVR MDX-Net networks | Uses the same `ort` and DirectML stack, with STFT performed by the application |
| Raw stems awaiting cleanup spill to files beside the results and are removed by `Drop` | Six stems of a five-minute track would occupy about 640 MB in memory; files are cleaned up automatically on failure or cancellation |
| Windows and Apple Silicon macOS, with isolated platform modules | DirectML and CoreML selection is centralized. Linux currently uses only the CPU and has not been validated |

## 3. Backend alternatives considered

| Option | Outcome | Reason |
|---|---|---|
| Custom `ort` pipeline with HTDemucs ONNX exports | **Selected** | Control over types, progress, cancellation, and three models |
| [`stem-splitter-core`](https://github.com/gentij/stem-splitter-core) | Reference implementation | Global callbacks, no cancellation, one model, environment-variable settings |
| [`charon-audio`](https://crates.io/crates/charon-audio) | Reference implementation | Version 0.1.2 from September 2026 was too young to adopt as a dependency |
| [`demucs-rs`](https://github.com/nikhilunni/demucs-rs) on Burn | Deferred | Not on crates.io; wgpu performance on the target GPU is unknown |
| Python CLI [`audio-separator`](https://github.com/nomadkaraoke/python-audio-separator) | Possible second backend | Better models such as RoFormer, but requires Python, 2–3 GB of PyTorch, and FFmpeg |
| Native RoFormer ([ONNX export](https://huggingface.co/silverdaw/bs-roformer-rhythm-onnx), [`uvr-rs`](https://github.com/IronHpc/UVR-rs)) | Deferred | Needs custom STFT/iSTFT; `uvr-rs` supports only the CPU |

The `Separator` trait in section 7 allows a second backend without redesigning the pipeline.

## 4. Models

| Identifier | Stems | Notes |
|---|---|---|
| `htdemucs` | drums, bass, other, vocals | Default and fastest |
| `htdemucs_ft` | drums, bass, other, vocals | Fine-tuned, slightly better quality, about four times slower |
| `htdemucs_6s` | the same, plus guitar and piano | The authors describe piano separation as mediocre |

Shared parameters: 44.1 kHz, stereo, 7.8-second segments (343,980 frames).

### Model files

Use [`HRSadeghi/demucs-onnx`](https://huggingface.co/HRSadeghi/demucs-onnx), pinned to verified revision `9128f6ee0f1b8e333b5b8d12613aefb3bebc249a`. Registry URLs point to that revision rather than `main`.

| Model | Files | Download size |
|---|---|---|
| `htdemucs` | `htdemucs.onnx` | 169 MB |
| `htdemucs_6s` | `htdemucs_6s.onnx` | 110 MB |
| `htdemucs_ft` | `htdemucs_ft_0.onnx` through `htdemucs_ft_3.onnx`, one per stem | 4 × 169 MB |

All graphs share this contract:

| Tensor | Shape | Content |
|---|---|---|
| Input `mix` | `[batch, 2, 343980]` | Normalized waveform |
| Input `mag` | `[batch, 4, 2048, 336]` | Spectrogram planes: left real, left imaginary, right real, right imaginary |
| Output `wav` | `[batch, S, 2, 343980]` | Time-domain branch |
| Output `spec` | `[batch, S, 4, 2048, 336]` | Spectral branch, with the same plane layout |

Each final stem is `wav` plus inverse STFT of `spec`. `htdemucs_ft` contains four graphs of the same architecture. Each emits all four stems, but only the stem it specializes in is retained, in order: drums, bass, other, vocals.

Rejected sources:

| Repository | Why it was rejected |
|---|---|
| [`smank/htdemucs-onnx`](https://huggingface.co/smank/htdemucs-onnx) | STFT is embedded as convolutions. CPU execution works, but inverse-transform `ConvTranspose` fails with a memory-allocation error on DirectML. Retained as a reference for the custom STFT |
| [`gentij/htdemucs-ort`](https://huggingface.co/gentij/htdemucs-ort) | DirectML cannot create a session for the `.ort` file; only one model is available |

Only fp32 weights are used. fp16 is excluded because GTX 16xx GPUs are known to produce numerical errors with half precision. The tested int8 CPU version was only 5% faster and differed more from fp32, so it is not used.

Demucs code is MIT. Its repository has no separate weight-license statement, and some third-party exports treat weights as CC BY-NC 4.0. Until clarified, the project is intended for personal, non-commercial use.

## 5. Project structure

```text
Cargo.toml                    # Seven workspace crates, shared versions and lints
crates/
  audio/                      # Audio/domain contracts, cancellation, codecs,
                              # resampling, frame sinks and atomic/staged output
    src/ffmpeg.rs             # Optional FFmpeg installer, conversion and encoder
    src/process.rs            # Owned cancellable external-process runner
  inference/                  # ONNX Runtime sessions, device fallback, tensor API,
                              # execution errors and shared model-file descriptors
    build.rs                  # DirectML.dll placement for tests and examples
  separation/                 # HTDemucs identities/catalog, STFT, chunk planning,
                              # overlap-add and separation backend
  enhancement/                # MDX-Net identities/catalog, STFT, windowing,
                              # compensation, prediction polarity and backend
  media-fetching/             # HTTP(S) URLs, metadata, playlists, yt-dlp
                              # installation and source-download workflows
  core/                       # Job/batch orchestration, verified model cache,
                              # workers, preview, tools, tracking and timing
    examples/                 # Headless separate and audio_roundtrip examples
  app/                        # stem-split executable: screens, form, playlist UI,
                              # native dialogs, translations and settings storage
spikes/                       # Standalone experiments outside the workspace
docs/
```

Dependency direction is acyclic: `app -> core`; `core -> audio, inference, separation, enhancement, media-fetching`; `separation/enhancement -> audio, inference`; `inference -> audio`; and `media-fetching -> audio`. Lower layers never depend on core or UI. Core intentionally re-exports domain and family APIs for its callers. The `closed_enum!` macro and `CancelToken` are owned by audio and remain exported by core.

Audio owns generic filesystem/audio values, atomic publication, `DecodedAudio`, `ModelAudio` and `FrameSink`. The resampler remains the only production constructor of `ModelAudio`. HTDemucs `ModelId` and graph metadata belong to separation; MDX-Net `EnhancerId`, cleanup parameters and metadata belong to enhancement. Each identity's `spec()` implementation therefore remains in its owning crate. Shared `FileSpec`, repositories, device/thread choices, `InferenceError` and `StftError` belong to inference. Each family retains its own STFT algorithm.

Only inference depends directly on ORT and its platform features. Its `DeviceSession` accepts named borrowed float `TensorInput` values and returns borrowed `TensorOutputs`; raw ORT sessions stay private. Outputs retain runtime ownership until dropped, avoiding additional audio-sized copies. Model input/output contract checks, plausibility guards and retry policies remain in the family backends.

FFmpeg installation, conversion and MP3 encoding live behind audio's optional `ffmpeg` feature, enabled by core and media-fetching. Its existing Tokio/yt-dlp installer dependencies are optional; audio without this feature has no downloader, HTTP client or ONNX dependency. Media-fetching owns yt-dlp installation and delegates FFmpeg operations to audio, mapping typed tool failures at the boundary. No new third-party dependency was introduced by this split.

Core owns background workers, metadata debounce/cancellation, tool/cache operations, runtime commands/configuration, tracking and estimates. `Wake::notify` and `Wake::notify_after` supply thread-safe immediate/delayed event-loop hints through a callback; the default is silent for headless polling. App adapts it to egui repaint requests. `CacheLocation`, `Mode` and `UrlAction` are core types; their UI persistence and translations stay in app. No library depends on egui, eframe or rfd.

The `test-support` features expose only deterministic fake separators/enhancers and collecting sinks needed by cross-crate tests. They are enabled through dev dependencies and are absent from ordinary builds. Core's headless examples remain at their existing command paths. Inference's build script places `DirectML.dll` beside test/example executables; release packaging remains unchanged.

## 6. Data flow

```text
 UI thread (app)                        Worker thread (core::job)
 ----------------                      -------------------------
 Form ---> SeparationRequest --------> ModelStore::ensure ---> cached model
                                        audio::decode     ---> DecodedAudio
                                        audio::resample   ---> ModelAudio (44.1 kHz)
                                        separate::plan    ---> 7.8-second chunks
                                        Separator         ---> chunk stems
 Progress <--- event channel ---------  separate::overlap  ---> completed frames
                                        stems for cleanup ---> *.wav.raw (Spill)
                                        mix               ---> in-memory sum
                                        enhance::clean    ---> cleaned stems and mix
 CancelToken ------------------------>  audio::wav        ---> *.wav.part ---> *.wav
 JobEnd / JobError <-------------------  completion
```

A multi-file run is a `core::batch::Batch`: one job per file sharing the model, device, and output settings. Its leading thread downloads the model once, then starts the configured number of file jobs at a time (two by default), each on its own thread. The next starts as soon as a slot becomes free. Events share one channel and include the file's index.

## 7. Types and invariants

This section identifies errors caught by the compiler. Signatures are illustrative. Domain types through `Sha256Digest`, except those explicitly introduced in later stages, are owned by audio and the model-family crates, then re-exported by `core::domain`.

Iterable enums (`Stem`, `ModelId`, `AudioFormat`, `DevicePreference`, and application `Locale` and `Text`) use the core's exported `closed_enum!` macro. It generates both the enum and `ALL`, so iteration cannot fall behind the variants.

| Type | Invariant | Enforcement |
|---|---|---|
| `ModelId` | The model exists | Enum; exhaustive descriptor match |
| `Stem` | A valid stem name | Enum; the variant determines its filename |
| `StemLayout` | Nonempty, fixed-order model stems | `first: Stem`, `rest: &'static [Stem]`; no public constructor; constants `FOUR` and `SIX` |
| `AudioFormat` | Supported input extension | Enum, also supplies dialog filters |
| `ModelSpec` | At least one graph | `first: GraphSpec` and `rest: &'static [GraphSpec]` |
| `GraphSpec` | Known file size, SHA-256, and emitted stems | Registry constants; `hex!` validates digest syntax and length at compile time; `emits` is `All` or `Only(Stem)` |
| `LocalModel` | All graph files exist and are verified | Created only by `ModelStore::ensure`; nonempty like `ModelSpec` |
| `Ensured` | Download cancellation is not an error | `Ready(LocalModel)` or `Cancelled` |
| `CancelToken` | Cancelled work cannot resume | A shared flag can only be set; clones share it |
| `SampleRate` | Positive sample rate | `NonZeroU32` |
| `FrameCount`, `ByteCount` | Frames cannot be confused with bytes or samples | Separate wrappers |
| `StereoFrames` | Exactly two equally long channels | `Vec<[f32; 2]>` |
| `DecodedAudio` | Stereo at a known nonzero sample rate | `SampleRate` and `StereoFrames`; no hidden invariant, so construction is public |
| `ModelAudio` | Stereo at exactly 44.1 kHz | No stored rate; only `resample` constructs it, with a core-private constructor |
| `ChunkPlan` | At least one chunk | Built from `NonZeroUsize`; `first: Chunk`, `rest: Vec<Chunk>` |
| `Chunk` | Offset, length, and position in the model window agree with track length | Constructed only by the planner |
| `Segment` | Exactly 343,980 samples per channel | Initially silent with the required length; later operations only write into it |
| `ChunkStems` | Sample count is stems × 2 × segment length | Validating constructor at model-output parsing |
| `InputFile` | Absolute existing regular file with supported extension and known directory/base name | Constructor validates and computes derived components once; getters do not return `Option` |
| `OutputDir` | Absolute existing directory | Validating constructor; `OutputDir::beside(&InputFile)` needs no revalidation |
| `OutputChoice` | Source folder or an explicit directory | `BesideInput` or `Custom(OutputDir)`; the former follows changes to the input |
| `OverwritePolicy` | Replacement is explicit | `Refuse` by default, or `Replace` |
| `SeparationRequest` | All fields are present and validated | Composed from checked types; constructor resolves the output directory |
| `InputFiles` | At least one file, each at most once | `first` and `rest`; list constructor returns `Option` and removes duplicates, case-insensitively on Windows |
| `Batch` | Nonempty jobs sharing model/device, with no duplicate output path | Built from `InputFiles` and shared settings; duplicate directory/base-name pairs cause `OutputClash` before work starts |
| `CpuThreads` | At least one CPU session thread | `NonZeroUsize`; `share(n)` divides cores among concurrent sessions |
| Job passes | At least one pass | Separate first-pass argument and a vector of remaining passes |
| `Sha256Digest` | Exactly 32 bytes | `[u8; 32]`; registry values are constants |
| `PartialFile` | No abandoned partial file or premature replacement | Writes `<name>.part`; `Drop` removes created resources unless `publish()` succeeds |
| `WavWriter` | A stem file is complete or absent | Owns `PartialFile`; only `finish(self)` publishes |
| `StemSet` | A stem set, including an empty UI selection | Bitmask over `Stem`; `within(layout)` retains model stems |
| `MixStems` | Nonempty stems all belonging to the model | Validating constructor from `StemSet` and `StemLayout` |
| `CleanupTargets` | Cleanup has a target | `Stems`, `Mix`, or `Both`; no cleanup is represented by absence, and `from_ticks` returns `Option` |
| `MixOutput` | The cleanup network supports the selected mix | Private fields; constructor rejects a vocal-network mix containing both vocals and instruments and retains mix polarity |
| `OutputKind` | Explicit cleanup targets belong only to mixes | `Stems { cleanup: Option<EnhancerId> }` or `Mix(MixOutput)` |
| `EnhancerId` | The cleanup network exists | Enum with exhaustive descriptor match |
| `MdxParams` | Nonempty network window, with bins fitting inside FFT | Checked by `chunking()` and STFT construction; every registered network is tested |
| `NetworkOutput` | Output meaning determines polarity | `for_stem` always succeeds; `for_mix` is absent for mixed vocals/instruments |
| `Spill` | No abandoned raw stem | Removes its file in `Drop` |

Inference boundary:

```rust
pub trait Separator {
    fn separate(&mut self, segment: &Segment) -> Result<ChunkStems, InferenceError>;
    fn device(&self) -> ActiveDevice;
}
```

Implementations are `OrtSeparator` and an identity fake for tests. The rest of the pipeline depends only on the trait. One `Separator` corresponds to one graph. STFT and iSTFT stay within `OrtSeparator` because they are specific to the export contract.

UI boundary:

```rust
pub enum Progress {
    FetchingModel(DownloadProgress),
    Decoding,
    LoadingModel,
    Separating { done: usize, total: NonZeroUsize, device: ActiveDevice },
    Enhancing { done: usize, total: NonZeroUsize, device: ActiveDevice },
    Finalizing,
}

pub trait ProgressSink {
    fn report(&self, progress: Progress);
}

pub enum JobEnd {
    Completed(SeparationOutput),
    Cancelled,
}

pub fn run(
    request: &SeparationRequest,
    store: &ModelStore,
    progress: &dyn ProgressSink,
    cancel: &CancelToken,
) -> Result<JobEnd, JobError>;
```

Any `Fn(Progress)` closure implements `ProgressSink`. The model store is passed explicitly, without global state. `run` reserves output names, ensures the model, decodes input, executes passes, and publishes files. Name collisions therefore surface before downloading or inference.

## 8. Separation algorithm

The pipeline follows Demucs `apply_model`.

1. Decode the whole input. Duplicate mono into both channels; reject more than two channels with `UnsupportedChannels`. Reject recordings longer than three hours because input stays in memory and WAV has a similar practical size limit.
2. Resample to 44.1 kHz if needed. Preserve duration within one frame and remove the resampler's own delay.
3. Normalize using the mean and standard deviation of the mono channel average: subtract the mean and divide by the deviation. Skip division for silence with near-zero deviation.
4. Plan 343,980-frame chunks with a 257,985-frame step, giving 25% overlap. As in Demucs, center a short final chunk inside the model window: actual preceding audio fills the left side, silence the right.
5. Make one complete-track pass per model graph. `htdemucs` and `htdemucs_6s` have one graph; `htdemucs_ft` has four, each contributing one stem. Only one session is held in memory at a time.
6. For each chunk, check cancellation, call `Separator`, check plausibility, apply a triangular window peaking at one in the center, and add samples and weights to the accumulators.
7. Frames before the next chunk's start are final: divide by accumulated weights, restore scale by multiplying by the deviation and adding the mean, then write them.
8. After the final chunk, write the remainder and rename `*.wav.part` to `*.wav`.

For each chunk, `OrtSeparator` performs:

1. Forward STFT per channel following Demucs `_spec`: Hann window 4096, hop 1024, normalization `1/sqrt(4096)`, reflection padding by 1536 samples on the left and to a hop multiple on the right, then centering. Retain the middle 336 of 340 spectrogram frames and discard the Nyquist bin.
2. Run the graph with `mix` and `mag`.
3. Inverse STFT per output stem and channel following `_ispec`: restore four discarded frames as zeros and compute the envelope across all 340 frames.
4. Add the `wav` time-domain output.

The spectral branch attenuates the first and last 1536 samples. This is a model property, not an implementation error: overlap-add gives chunk edges almost zero triangular weight.

Plausibility requires all values to be finite and output level to be no more than one order of magnitude above input level. This guards against DirectML silently returning invalid values (section 11). A failing GPU chunk is recomputed on the CPU, which is then used for the rest of the job.

The single difference from the reference algorithm is that random shifts are not applied; results are deterministic.

Comparison against Python Demucs was not performed and was explicitly excluded by the owner. Correctness rests on matching an export with embedded STFT, comparing GPU with CPU, and fake-separator tests of chunking and reconstruction.

Input stays in memory, about 106 MB per five minutes; output is streamed, so stem count has little effect on memory use.

Complete 106-second track processing on the target machine, 19 chunks, models cached:

| Model | GPU / DirectML | CPU |
|---|---|---|
| `htdemucs` | 12.6 s | 55.4 s |
| `htdemucs_6s` | 10.7 s | Not measured |
| `htdemucs_ft` | 38.9 s | Not measured |

Times include decoding, graph loading (about two seconds per graph), and file output.

## 9. Output files

- `OutputFormat::{Wav, Mp3}` selects 32-bit float WAV or 320 kbit/s MP3, both at 44.1 kHz stereo. WAV float preserves sums outside `[-1, 1]`. Request paths, overwrite checks, stems and mixes all use the selected extension.
- Names are `<input-base-name>.<stem>.wav`, such as `song.vocals.wav`, directly in the chosen directory. A mix is `<name>.mix.wav`.
- The mix sums selected stems in memory as frames become final and is written after the last pass. Without Save stems, no stem files are created or listed in overwrite confirmation. `htdemucs_ft` runs only the selected stem passes in that mode.
- Stems awaiting cleanup are raw `<name>.<stem>.wav.raw` files beside the results. Each job reads and deletes its own raw stems one by one; up to two jobs can own distinct sets. A subsequent job replaces raw files left by a killed process.
- Streaming calculations and cleanup use float-WAV staging files. WAV publishes directly; MP3 calls the application-supplied `AudioEncoder` before publication. `WavWriter::finish_as` owns both staging files and publishes the chosen extension only after successful encoding, preserving prior files on failure/cancellation.
- Existing files are never silently overwritten. `OverwritePolicy::Refuse` reserves a name with an empty `create_new` file. If it exists, `AlreadyExists` triggers confirmation. Reservation is atomic, so concurrent writers cannot both claim the same name.
- With `OverwritePolicy::Replace`, the old file stays intact until the final rename; failed or cancelled work does not damage it.
- Failure or cancellation removes partial files and reserved names.
- WAV limits each file to 4 GiB. `WavWriter` enforces this because `hound` does not.

`AudioEncoder`, `EncodeError` and the normal `Encoded::{Ready, Cancelled}` outcome are audio boundary types re-exported by core. `job::run_with_encoder` accepts the encoder; the WAV-only `job::run` remains usable without media dependencies. `audio::ffmpeg::FfmpegEncoder` installs in the shared model cache and implements MP3 encoding with cancellable owned subprocesses. The batch worker prepares it once and shares it across the two concurrent file jobs. `tempfile` is an audio dependency for encoded staging and a core dev dependency for regression tests.

## 10. Model store

- `separation::model` and `enhancement::model` own static repository/path, size, SHA-256, emitted-stem, and cleanup `MdxParams`/`NetworkOutput` descriptions; core assembles their public catalog. `Repository::DemucsOnnx` and `Repository::UvrResources` each point to a pinned revision.
- `ModelStore::ensure_files` downloads the separation model and cleanup network together with combined progress. `missing_size` reports the required download to the form. The backend describes the shared graph input/output contract.
- `directories` supplies the per-user cache location: `%LOCALAPPDATA%\stem-split\cache\models` on Windows.
- Cache filenames include a digest prefix, such as `htdemucs-abaac48bc8fd55fa.onnx`. A later registry revision cannot mistake an older file with the same graph name for new content.
- Download to `*.part` in 64 KiB blocks, report progress, and check cancellation after each block. Rename only after size and SHA-256 match. Otherwise remove the file and report expected and actual values.
- Since files enter the cache only after verification, subsequent use checks name and size without rehashing every launch.
- Progress covers all missing files, including all four `htdemucs_ft` graphs. A complete cache needs no network access or download progress.
- Cancellation keeps completed downloads and removes the current partial file.
- `usage` returns `Usage::NoDirectory` before the cache exists, or `Usage::Files(ByteCount)`. `clear` deletes regular files and returns bytes freed, including models, retired model files, and crash-leftover `*.part` files. It does not remove subdirectories or the cache directory itself.
- Cache deletion cannot overlap a job, since a model could disappear before loading. The application disables starts during deletion.
- Connections use HTTPS and OS certificate validation, honoring certificates installed by administrators or antivirus software.
- Proxies come from `HTTPS_PROXY`, `ALL_PROXY`, and `NO_PROXY`. Windows system proxy settings are not read.

## 11. Execution device

- `DevicePreference { Auto, Gpu, Cpu }` is the user's choice, initially `Auto`.
- `ActiveDevice` reports the actual device to the UI; the original Windows set is `DirectMl` and `Cpu`.
- Auto tries DirectML on Windows or CoreML on macOS, then falls back to CPU on failure and logs the reason.
- Only `inference::session` builds the provider list: DirectML under `#[cfg(windows)]`, CoreML under `#[cfg(target_os = "macos")]`, and CPU elsewhere.

CoreML uses `ComputeUnits::All` (CPU, GPU, and Neural Engine) and `MLProgram` (Core ML 5, macOS 12+). Registration uses `error_on_failure`, so session-creation failures explicitly select the CPU. Some operators may still run on the CPU. A GPU label identifies the provider, not an exclusively GPU graph. This document has no CoreML speed measurements or CPU comparisons yet.

Stage 0 verified these DirectML settings:

| Setting | Value | Reason |
|---|---|---|
| `ep.dml.disable_graph_fusion` | `1` | Required: with static dimensions, fusion can silently return values 100,000 times too large. Disabling it matches CPU results at about a 10% speed cost |
| Provider registration | `error_on_failure` | Otherwise failed DirectML silently becomes a CPU session while the UI reports the wrong device |
| Adapter selection | `PerformancePreference::HighPerformance` | Systems may contain virtual and integrated adapters |
| Memory pattern and parallel execution | Disabled | Required by ONNX Runtime's DirectML documentation |

The graph's dynamic `batch` dimension need not be fixed; graph fusion is disabled explicitly regardless of input shape.

A single CPU job receives all logical cores, about 10% faster than default on the target machine. For two concurrent jobs, `CpuThreads::share` splits them in half. Throughput remains similar, 79–81 seconds for two tracks versus 97 seconds sequentially, without oversubscribing the machine.

`batch::PARALLEL_JOBS` supplies the default of 2; Settings can override it for each new batch. Target-machine measurements for a 106-second `htdemucs_6s` track:

| Concurrent files | GPU | CPU |
|---|---|---|
| 1 | 10.6 s | 48.6 s |
| 2 | 14.5 s, or 7.2 s per file | 78–81 s, about 40 s per file |
| 3 | 20.1 s, or 6.7 s per file | Not measured |
| 4 | 39.7 s; exceeds 6 GB VRAM | Not measured |

The GPU benefits because one job can read files, load graphs, or compute STFT on the CPU while another waits for the GPU. Three files barely improve on two and leave less VRAM for larger models.

Cleanup batches default to two jobs through `batch::PARALLEL_JOBS_WITH_CLEANUP = PARALLEL_JOBS`; the saved parallel-track choice overrides this default too. The earlier serial policy was based on historical measurements: cleanup sessions occupy 2.3–3.6 GB. For two 5:50 tracks with both stem and mix cleanup, Reverb HQ took 620 s concurrently versus 154 s sequentially and used 5.9 GB VRAM. The vocal network took 120 s concurrently versus 142 s sequentially.

## 12. Threads, progress, and cancellation

- Use `std::thread::Builder`, which returns `Result`. A batch leader starts `spawn_scoped` file jobs within `thread::scope` and receives completion messages when a slot becomes free.
- The leader downloads models once; concurrent downloads of the same graph would overwrite the same `*.part`.
- First read stated track lengths from headers through `audio::stated_length`, without decoding, and report `job::expected_runs` for time estimates.
- Events use `std::sync::mpsc` with file indices. The application's `ProgressSink` sends an event and calls `Context::request_repaint`. An explicit final message closes the workflow; channel loss without it means the leader panicked.
- The UI drains `try_recv` at frame start and never waits.
- `CancelToken` uses `Arc<AtomicBool>` and is checked between chunks and download blocks.
- A dropped receiver is treated as cancellation.
- Closing the window requests cancellation and waits up to ten seconds for partial-file cleanup. Jobs react within one model run. A stalled download may take longer, in which case the worker is left behind.
- `catch_unwind` converts a dependency panic in a file job to `Failure::Crashed`; the rest of the batch continues.
- Cancellation stops running jobs and prevents waiting ones from starting; completed files remain.
- Workers open their own model store from the saved `CacheLocation`, shared with metadata inspection and source downloads. `core::downloads` has a UI-side store for immediate metadata checks at startup, after jobs, and after deletion, rather than every frame.
- Cache deletion runs on its own thread and reports through a channel. Missing completion is treated as a crash and shown as an error.
- Core warnings and dependency panic reports go to stderr and `%LOCALAPPDATA%\stem-split\data\stem-split.log`, recreated each launch. Release builds have no console, so the file is their only log output.
- Failure to create a window is reported through a native message box.

## 13. Interface

One window uses a screen state machine:

```rust
enum Screen {
    Editing(Form),
    Settings { form: Form, tools: Tools },
    Confirming { form: Form, existing: Vec<PathBuf> },
    Running(Box<Run>), // Form, workers, file tracking, and cancellation state.
    Done(Finished),   // Form, per-file results, and total time.
    Failed { form: Form, failure: Failure },
}
```

Each screen owns only its valid data: no progress bar without work, no Split button during a batch. The form moves between screens, preserving choices after success, failure, or cancellation.

- Each screen has a dedicated action type, such as `EditingAction` or `ConfirmingAction`. `app.rs` maps a screen and action to the next screen, making invalid combinations unrepresentable.
- View functions receive data, render, and return an action; they do not own application state.
- Split requires selected local files or a valid URL and destination. Existing results trigger a combined overwrite-confirmation screen.
- Files may be supplied as command-line arguments.
- Combo boxes have accessible names associated with their labels.

### Settings / local 1.3.0

A gear button beside the language selector opens a dedicated settings page while retaining the form and playlist choices. The page scrolls on small windows. It shows cached yt-dlp/FFmpeg installation status and offers Download, Update, and Delete separately for each tool. The cache path and automatic first-use installation behavior remain visible. Errors use muted red foregrounds in both light and dark themes.

`core::tools::Tools` owns one cancellable worker at a time, with typed errors, a completion channel and a joined thread. Status scanning, cache usage measurement, installation, updates and deletion run off the UI thread. Opening Settings cancels metadata inspection and waits asynchronously for its worker to finish before scanning or enabling tool actions. While tool management runs, navigation back and other tool actions are disabled; cancellation and bounded shutdown remain available. The settings page cannot overlap batch execution or cache deletion. Returning transfers the worker's measured cache usage to the main form without filesystem I/O.

`media_fetching::Tool` is an iterable closed enum. `media_fetching::tools::manage` shares executable paths and installation logic with automatic downloads. Updates stage a fresh binary privately before replacing the installed file; failure or cancellation leaves the previous binary intact. Deletion targets only the chosen executable and tolerates an already missing file. No model files are removed. Core uses the existing workspace `thiserror` dependency for typed worker errors.

The settings page also opens the shared models/cache folder, chooses an existing folder through the native picker, and restores the platform default. `CacheLocation::{UserCache, Custom(OutputDir)}` is stored with the form settings and passed explicitly to metadata, media-download and batch workers. There is no process-wide environment override. Choosing a folder immediately refreshes model availability; tool presence and total usage are then scanned off the UI thread. Opening a not-yet-created default folder creates it on the worker before launching the file manager. Switching folders never copies, moves or deletes existing cached files or prepared inputs; their RAII owners retain their original paths. A saved custom folder that is no longer valid falls back to the platform default on startup.

The installed `yt-dlp` Rust crate 2.10.0 does not offer release-tag/version selection: `LibraryInstaller` accepts custom repositories and executable names but both yt-dlp and FFmpeg fetchers resolve the latest release. `Option<String>` in the installer is a filename override, not a version. Explicit version selection would require a separate release-resolution/download implementation; it is not exposed in Settings through this dependency.

Parallel track count is saved as a positive integer, with a UI/persistence range of 1-8 and default 2. Missing, zero, out-of-range or malformed saved values fall back to 2. `Batch::with_parallel_jobs` caps concurrency by the actual track count and recalculates every request's CPU thread share. It applies equally to local and downloaded tracks, separation and cleanup. The UI explains the extra RAM/VRAM cost; URL downloads remain sequential.

### URL audio / local 1.2.0

Files and URL are separate tabs. Local selections are retained independently of downloaded URL files. URLs must be absolute HTTP(S) addresses without embedded credentials. Downloading requires an existing destination folder, used for both source audio and results; metadata preview does not require a folder. Model, device, mix, and cleanup settings are shared.

A clear button beside the URL empties it and restores text focus; its tooltip and accessible name are localized. Source and destination cells share dimensions across tabs, with a common minimum label-column width, so common settings do not jump. `form::StartBlock` is the source of actionable disabled-state explanations: missing files, empty or invalid URL, missing folder, no mix stems, or cache deletion. Shared explanations are not duplicated.

`media_fetching::download` runs in a worker with its own single-thread Tokio runtime. `yt_dlp::client::deps::LibraryInstaller` installs platform yt-dlp in media-fetching and FFmpeg in audio when absent. Completed executables are published directly in `ModelStore::directory()`. Temporary installation files have RAII owners in that cache. Existing `usage` and `clear` count and remove the tools as ordinary files alongside models, without recursively deleting unrelated directories.

`audio::process` owns cancellable/reaped child processes; `media_fetching::process` provides yt-dlp arguments and streamed download progress and reads streaming progress. User yt-dlp configuration is ignored and its separate disk cache disabled. Metadata uses `--skip-download --flat-playlist --yes-playlist --dump-single-json`. `MediaInfo` is a single item or an ordered nonempty playlist with validated HTTP(S) URLs. Selection creates an exact nonempty snapshot; the download worker never re-expands the parent playlist.

The best source audio is downloaded with `bestaudio/best`. `SaveFormat::{Original, Wav, Mp3}` either preserves codec/container bytes or encodes a float WAV / 320 kbit/s MP3 in private staging before publication with a safe title, unique suffix and chosen extension. A live or unavailable entry fails independently; `DownloadEvent::{Progress, Finished}` includes the selected item's index. The loop reports failures and continues with later items. Shared setup failures are attached to each affected item rather than replacing the entire playlist with one error screen.

`DownloadedAudio` owns the native source path and a core `InputFile`. MP3/FLAC/WAV/M4A inputs are used directly when supported. Other containers/codecs, including WebM/Opus, get private float-WAV decoding copies for inference, without altering the native download. FFmpeg is installed when source transcoding, private decoding or MP3 stem/mix output is needed. Saving MP3 never feeds the re-encoded lossy file back into inference: its private WAV comes from the original stream. These copies share one RAII directory in the model cache; references survive the ordinary split/mix batch and input reuse. The final owner schedules directory cleanup off the UI thread. The existing `log` dependency records cleanup failures. Incomplete native downloads remain inside destination-local RAII directories and disappear on error/cancellation.

`SourceProgress(index, progress)` and `SourceFinished(index, result)` update independent tracker rows, including queued entries. A row shows its own stage, bytes/percentage, elapsed time and error detail. `SourceReady` transfers successfully prepared audio owners into the form, even when some items failed. Download-only results contain all per-file outcomes; split/mix runs only successful inputs and appends download failures to its final results. Original native paths label separation rows while core jobs decode the prepared input paths. Source files remain in the selected destination. Cancellation prevents later downloads and subsequent separation from starting.

Added dependencies: `yt-dlp` 2.10 for binary installation; `tokio` for its async API and subprocess I/O/cancellation; `url` for HTTP(S) validation; `serde` and `serde_json` for minimal metadata; `tempfile` for RAII intermediates; and `thiserror` for typed errors. `yt-dlp` disables defaults and enables `rustls` and `cache-memory`; published 2.10.0 otherwise fails to compile because a `utils` import is conditional. No persistent library cache is created. The dependency is GPL-3.0-only, distinct from the project's own MIT sources.

`core::preview` owns the metadata state machine: Empty, Pending, Loading, Ready, Failed. Edits restart a 500 ms monotonic debounce and cancel the previous request. A worker installs only yt-dlp if necessary and calls `media_fetching::inspect`; no media or FFmpeg is downloaded. At most one preview worker runs at a time; cancelled workers are reaped before another starts. Results are accepted only while the matching URL is loading, so stale responses cannot replace new input. Work is paused off the URL tab and during cache deletion. Shutdown cancels the worker and waits with the normal bounded patience.

Metadata has a 24-point reserved area below the URL, measured before requests begin. A title is truncated with a full tooltip; successfully inspected playlists show their total count and a right-aligned Playlist toggle in the same row. Loading/error/title/button states retain the same height, including long titles and large playlists. Shared source-cell sizing preserves expanded-tab alignment. Cache deletion is disabled while metadata work owns a tool; cache usage is measured by that worker and passed back without UI-thread I/O. Download/start cancels preview work; independent staging makes concurrent tool publication safe. The folder cell remains 60 points high. A source-format combo is directly beside its URL destination Browse button. The result-format combo is on the right of the lower action buttons; both labels/tooltips are localized. Missing or unknown stored format keys fall back to Original/WAV.

### Playlist selection panel / local 1.2.0

`playlist::Choices` keeps excluded entry positions in a set and an explicit Open/Closed panel state. Choices exist only for successfully inspected playlists, default to all entries included, and are transient. A URL change clears the panel, selections, and downloaded-input reuse. Changing a checkbox also invalidates the reused downloaded set. Switching to Files hides the panel without losing the current URL choices.

The right-hand panel is 300 points wide, with a 12-point gap and independent vertical scrolling clipped to the current viewport. Only the main form contributes to the native minimum height; opening/closing the panel changes required width while retaining current window height. The source region shows title, total count and the only panel toggle on one line. The sidebar has no duplicate close button. Long checkbox labels truncate with full tooltips. All/None checkboxes above the list select or clear the entire playlist and reflect individual choices; changing either invalidates the predownloaded selection. Both the URL preview and sidebar show the original playlist track count beside its title, with localized number agreement. A separate selected/total counter updates with every checkbox change. The count stays visible while long titles truncate.

Starts wait for successful metadata, a destination, and at least one included entry. A fixed single-line explanation area under the split buttons shows disabled reasons without changing height when selection becomes empty. For an original playlist longer than five entries, starting Download, Split, or Split and mix opens a closed selection panel and returns to editing. A subsequent start while it is visible proceeds. Playlists with five or fewer entries start without interception. The media worker receives the exact nonempty selected `MediaInfo` snapshot from `MediaInfo::selected`, preserving source order. It never re-expands the original playlist, so unchecked entries cannot be downloaded due to a refreshed list. Full per-entry live checks and best-audio selection remain unchanged.

### Preloading and window size / local 1.2.0

Download saves audio independently. `UrlAction::{Download, Separate(Mode)}` tells the worker what follows `SourceReady`: preloading ends with `Outcome::Downloaded` and no model or inference; separation starts the normal batch. The native `DownloadedAudio` owners and their prepared `InputFiles` remain in the current form and are reused while the URL and folder are unchanged and every native and prepared file still exists. Changing URL or destination discards this selection but leaves audio on disk.

The form no longer scrolls inside a fixed-height area reserving a large footer. It is measured at its natural height and a shared content width of 624 points. `EditingView` returns both the action and required size. The application sets the native minimum with panel margins, fits the window on opening, and subsequently grows it as needed. The limit accounts for monitor size, window decorations, and system-panel allowance. A whole-form fallback scroll remains for displays too small to fit the minimum; the input-file list scrolls independently. Resize commands are issued after a completed egui layout pass.

Split and Split and mix are 34 points tall, use the theme's accent color, and have bold labels. Moving language selection out of the old bottom-aligned layout removed the large empty footer. Disabled-state explanations remain visible.

### Compact preloading / local 1.2.0

Download is in the URL row immediately after Clear URL. It uses the same `StartBlock` logic and disabled tooltip. Below source and destination is the Separation and mix disclosure header. `SeparationPanel::{Collapsed, Expanded}` controls the whole lower area: model, stems, cleanup, device, start buttons, and their explanations. Language selection remains available on the right of that header. State changes have no height animation; the window is refitted and shrinks when the section collapses.

`separation_panel` saves `collapsed` or `expanded`. Unknown or missing values show the section. Collapsing does not change separation settings or independent downloading. The URL input takes the remaining row width after its buttons. Download-disabled explanations are available on the button tooltip even when separation controls are hidden; start-button explanations remain below the separation controls.

Compact URL mode ignores the hidden local-file selection when measuring height. Switching to that tab also refits the window. Expanded mode retains alignment between tabs.

### Multiple local files

- The dialog and drag-and-drop accept several files. A new selection replaces the old; duplicates are removed. Unsupported files are skipped and reported, with only the first three named individually. If none are accepted, the previous selection stays.
- One file is shown with its folder; multiple files show a count and a list scrolling after roughly four lines.
- Files that would produce identical output names are rejected before starting, including `song.mp3` and `song.flac` in one folder or equal base names from different folders with a shared destination.
- Every running file has its name, progress/status bar, and time. A model-download row precedes the files; a total clock appears for multi-file batches.
- Results show each file's time, device, and output list or failure. The heading counts successful files. A shared destination gets one Open folder button; otherwise each file has one.
- Cancellation before any result returns to the form. Otherwise results include cancelled or never-started files.

### Time reporting

A file can show `0:08 · ~0:12 left`; a batch can show `All files: elapsed 0:13 · about 0:07 left`.

- File time starts with that file; batch time starts with Split, including model downloads.
- Remaining time applies to the measurable current stage, downloading, separation, or cleanup. Reading files, loading graphs, and final output have no estimate.
- Estimate stage completion from the average pace after its first progress report, excluding initial connection, graph loading, and warmup. It appears at the second report.
- Between reports, estimates count down and never go negative. Display rounds up and never shows less than one second.
- Loading another `htdemucs_ft` graph does not reset separation pace; loading time contributes to the average.
- Results round per-file duration to the nearest second, for example `Finished in 0:13 on the graphics card.` Multi-file totals are shown separately.
- Batch estimates simulate two scheduling lanes. Running files occupy their lanes for their measured remaining time. Waiting files cost average startup plus subsequent runs at measured pace. Startup is the average from file start to its first run. Header lengths determine expected runs; missing lengths use the observed average. Files are scheduled in order into the first free lane, and the latest lane completion is the batch estimate.
- This keeps fixed file startup separate from model work and does not split one long file across two lanes. On four files, two 106-second tracks and two short ones, estimates at 7, 10, and 14 seconds were 14, 11, and 7 seconds, compared with actual 13, 10, and 6.
- Arithmetic is isolated in `core::timing`, with time passed in for tests rather than sleeping. Running screens repaint every 200 ms so clocks advance without worker events.

Original form sketch:

```text
+ stem-split ----------------------------------------+
| File:   [ D:\music\song.flac              ] [Browse] |
| Output: (*) next to the file                       |
|         ( ) [ D:\stems                   ] [Browse] |
| Model:  [ htdemucs - 4 stems, fast              v ] |
| [ Split ]                                         |
| [############.........] 12 / 31 - GPU (DirectML)    |
+---------------------------------------------------+
```

`rfd` provides native file and directory dialogs. They are modal, available only from the main form, and block interaction while open. `view::text` matches every `JobError` and nested error to a readable explanation plus selectable technical detail in small text.

### Localization

No third-party i18n crate is used; locale completeness is enforced by types:

```rust
closed_enum! {
    pub(crate) enum Locale { En, Ru }
}
closed_enum! {
    /// A user-visible string without parameters.
    pub(crate) enum Text { File, Browse, Split, Cancel, /* ... */ }
}
impl Text {
    pub(crate) const fn get(self, locale: Locale) -> &'static str;
}
/// Parameterized messages are functions.
pub(crate) fn progress_status(locale: Locale, progress: Progress) -> String;
pub(crate) fn failure(locale: Locale, failure: &Failure) -> Message;
```

- Every user-facing string belongs in `view::text`, not in rendering literals.
- `text::en` and `text::ru` expose the same functions. The parent exhaustively selects a module by `Locale`; each module exhaustively selects text by the described value. A new language, string, or core error cannot compile until handled everywhere.
- Shared formatting of time, megabytes, and filenames is computed once in the parent module.
- Russian has its own noun/number agreement function; its translations remain intact.
- Core returns typed `Progress` and `JobError`, not UI text. Technical error descriptions are not translated.
- A subdued globe opens language selection. Every language labels itself in its own language, so it can be found from an unfamiliar interface. `Text::Language` supplies an accessible name and tooltip. The original form placed this in the bottom-right corner; the compact form places it beside the disclosure header.
- First launch follows the OS interface language through `sys-locale`; subsequent launches use the saved choice.
- Bundled egui fonts support Latin and Cyrillic; other scripts need additional fonts.

### Drag and drop

Hovering files darkens the window and shows a hint. Dropping on editing, confirmation, result, or error screens selects new inputs and returns to editing. A running batch ignores drops. Dropped files use the same validation as dialog selections.

### Mix and cleanup controls

- Stems: one checkbox per current model stem, All (toggling it again clears all), None, and Save stems. Selection is retained across all six stem types, so guitar reappears when returning to `htdemucs_6s`; `StemSet::within` selects the current model's subset.
- Cleanup: Clean up artifacts, separate Stems and The mix checkboxes, and a network selector. The mix is unavailable for an unsupported combination; its stored tick remains but has no effect. A tooltip explains how to change the selection.
- Split and Split and mix share overwrite confirmation through `form::Mode`. The latter requires at least one selected model stem.
- Split cleans stems only when cleanup and Stems are enabled; The mix does not affect it.
- The form combines model and cleanup downloads through `ModelStore::missing_size`.
- Stable keys save `mix_stems` as comma-separated tags, `keep_stems`, `cleanup`, `cleanup_stems`, and `cleanup_mix` as `on`/`off`, and `enhancer` by network name. Any unknown stem invalidates the saved stem list as a whole.

### Downloaded models and tools

Two small icon-font buttons beside the model selector open the cache folder or delete downloaded files after a popover confirmation with the size to free. Clicking outside the confirmation deletes nothing. Before the directory exists, Open is disabled but its tooltip still shows the path. Delete is disabled when nothing is cached.

Starts are disabled during background deletion. Afterwards, model availability is checked again and the download notice returns. Folder-open, locked-file, and similar failures appear beneath the model selector. `Text::OpenDownloads` and `Text::ClearDownloads` name the icons for accessibility and tooltips.

### Persistent settings

Save language, model, device, save format (`save_format`), output format (`result_format`), active tab (`source_tab`), local-file destination (`output_folder`), URL destination (`url_output_folder`), stems, cleanup, and disclosure state (`separation_panel`). Do not save selected files or URL text. For legacy settings without `source_tab`, the earlier shared folder initializes the URL destination; after saving, destinations are independent. Deleted directories are discarded.

- eframe's `persistence` feature stores settings in `%APPDATA%\stem-split\data\app.ron`, including window geometry; content fitting sets the minimum on opening.
- Values use stable keys such as `htdemucs_ft`, `cpu`, and `ru`, not interface labels.
- Validate every loaded value. A missing, unknown, or stale value falls back independently without discarding the rest.

## 14. Errors

`DecodeError`, `ResampleError`, `ModelStoreError`, `InferenceError`, and `WriteError` are wrapped by `JobError`. Cancellation is a separate outcome.

`CacheError` covers measuring and deleting cached downloads and is separate from `JobError`: a job never clears the cache. `MixStemsError` (empty selection or a stem outside the model layout) and `MixCleanupError` (unsupported mix content) arise while constructing a request. The form prevents these requests by disabling invalid actions and ignoring inapplicable selections.

Domain errors `InputFileError`, `OutputDirError`, `OutputClash`, `ChannelLengthMismatch`, and `DigestParseError` occur while validating input, before a job starts. They are separate from `JobError`. The app maps every `JobError` variant to user-facing text with an exhaustive match.

## 15. Dependencies

| Crate | Version | Purpose | Layer |
|---|---|---|---|
| `eframe` | 0.36 | Window and UI; `eframe::egui`; features `glow`, `default_fonts`, `accesskit`, `persistence`, `wayland`, `x11` | app |
| `sys-locale` | 0.3 | System language on first launch | app |
| `winresource` | 0.1 | Executable icon and version metadata; build only | app |
| `rfd` | 0.17 | Native dialogs | app |
| `open` | 5 | Open the output directory | core, app |
| `ort` | `=2.0.0-rc.13` | ONNX Runtime 1.28; `std`, `download-binaries`, `copy-dylibs`, `tls-rustls`, `api-27`; Windows `directml`, macOS `coreml` | inference |
| `ort-sys` | `=2.0.0-rc.13` | Direct Windows dependency to run its build script before the inference build script; unused in source | inference, Windows |
| `realfft` | 3.5 | Forward and inverse STFT | separation, enhancement, inference error contracts |
| `symphonia` | 0.6 | Decode MP3, FLAC, WAV/PCM, OGG/Vorbis, ISOMP4/AAC/ALAC and metadata | audio |
| `rubato` | 5 | Synchronous FFT `Fft` resampling, 4096-frame blocks | audio |
| `hound` | 3.5 | WAV output | audio |
| `ureq` | 3 | Blocking HTTPS; `rustls` and `platform-verifier`, no compression | core |
| `sha2` | 0.11 | Model integrity verification | core |
| `hex-literal` | 1 | Compile-time checked registry digests | separation, enhancement |
| `directories` | 6 | Model cache and log paths | core, app |
| `thiserror` | 2 | Typed errors | library crates |
| `tempfile` | 3 | Encoded output staging and temporary test files/directories | audio/media-fetching; library tests |
| `log` | 0.4 | Skipped corrupt packets, GPU fallback, app warnings to stderr | audio, inference, media-fetching, core, app |

Media-crate dependencies and executable installation are described in section 13. `ort` remains a release candidate and is pinned exactly. Its `ndarray` and `tracing` features are disabled: tensors use slices and ONNX Runtime messages go to stderr.

### Windows distribution

`tools/package.ps1` produces an approximately 21 MB archive:

| File | Purpose |
|---|---|
| `stem-split.exe` (35 MB) | Application with statically linked ONNX Runtime |
| `DirectML.dll` (18 MB) | GPU acceleration, version 1.15 |
| `vcruntime140.dll`, `vcruntime140_1.dll`, `msvcp140.dll`, `msvcp140_1.dll` | Dynamic Visual C++ runtime required by the prebuilt ONNX Runtime; it may be absent on a clean machine |
| `README.txt` | English installation and usage instructions |

The script uses redistributables from the newest installed Visual Studio or Build Tools: the runtime must be at least as recent as the compiler. A static application runtime is incompatible with the prebuilt ONNX Runtime. Release builds have no console and embed the icon/version.

Windows 10's system `DirectML.dll` is version 1.0 and cannot initialize the provider; Auto would silently use CPU. `copy-dylibs` places the current library beside the application under `target/<profile>/`. Without symlink privileges it stops there; the inference build script copies the library into `deps/` and `examples/` for tests and examples.

### macOS distribution

On Apple Silicon, `bash tools/package-macos.sh` builds `dist/stem-split-<version>-macos-arm64.dmg` and its SHA-256 checksum using `--locked`, explicit `aarch64-apple-darwin`, and macOS 12 minimum. The image contains the app, an Applications link, license, and English README.

The app contains one executable plus Info.plist/icon resources. ONNX Runtime is static. Packaging rejects any `otool -L` dependency outside `/System/Library/` and `/usr/lib/`. Models are downloaded to `~/Library/Caches/stem-split/models`; settings/logs live in `~/Library/Application Support/stem-split/`.

The verified ad-hoc signature does not provide Developer ID or notarization. Gatekeeper may block downloads; instructions allow this particular application in Security settings, with removing quarantine from the installed app as a fallback. They do not require disabling Gatekeeper globally. Intel/universal builds and clean macOS 12 testing remain outstanding.

### Versions and publication

The shared app/core version comes from `[workspace.package].version` in root `Cargo.toml`, synchronized with `Cargo.lock`. Bump once per local batch, then retain that version until the batch is committed and successfully pushed. A local commit alone does not start a new cycle. The first change selects MINOR with PATCH reset for features, PATCH for fixes, and MINOR for both. Documentation-only changes do not bump. `AGENTS.md` records this policy. The published release is 2.0.0; the current local refactoring version is 2.0.1.

`rust-toolchain.toml` pins Rust 1.96.1 with rustfmt and Clippy. `.github/workflows/release.yml` runs version checks, formatting, Clippy, and tests for a `v<version>` tag on `windows-2022` and ARM64 `macos-15`, then packages with `--locked`. Both builds must succeed before uploading to an existing release or creating one. Manual dispatch also requires a tag. DMG format and ad-hoc signing are retained.

## 16. Artifact cleanup

### Networks

| Network | File | Size | FFT / hop | Bins × frames | Compensation | Output |
|---|---|---|---|---|---|---|
| `reverb_hq` | `models/MDXNet/Reverb_HQ_By_FoxJoy.onnx` | 66.8 MB | 6144 / 1024 | 3072 × 512 (11.9 s) | 1.035 | Reverberation |
| `vocal_bleed` | `models/MDXNet/UVR-MDX-NET-Voc_FT.onnx` | 66.8 MB | 7680 / 1024 | 3072 × 256 (5.9 s) | 1.021 | Vocals |

Source: [Politrees/UVR_resources](https://huggingface.co/Politrees/UVR_resources), revision `929e057b81aa49bc2e6490bef8671f47b2c120f6`. Parameters come from UVR's `model_data_new.json`, keyed by MD5 of the final 10,240,000 bytes: `cd5b2989…` for Reverb HQ and `77d07b26…` for Voc_FT.

Graph `input` and `output` have shape `[1, 4, bins, frames]`: left real/imaginary, right real/imaginary. DirectML graph fusion is disabled as for HTDemucs; GPU/CPU differences are at most 5e-8.

### Polarity

`NetworkOutput` interprets the predicted content:

- `Unwanted` (Reverb HQ): always subtract `input - output × compensation`.
- `Vocals` (Voc_FT): retain compensated output for a vocal stem, removing instrumental bleed; subtract it for instrumental stems, removing vocal bleed. Homogeneous vocal-only or instrumental-only mixes follow the same rule. `MixOutput::new` rejects mixed vocals/instruments, and the form disables Mix cleanup for that selection.

### Algorithm

Follow UVR `demix` without window blending:

1. A window contains `hop × (frames - 1)` samples. Discard half an FFT (`trim`) at each edge and keep the center.
2. Advance by that center length. The first window begins `trim` before the track, padding missing samples with silence.
3. Compute each channel's STFT (`center=True`, reflection, periodic Hann, no normalization). Keep the low bins and zero the lowest three, as UVR does. Run the graph, inverse-transform with discarded bins zeroed, and keep the center.
4. Apply polarity and plausibility checks; retry on CPU after a GPU failure.

### Job stage

Cleanup runs after separation and before publication, so cancellation/errors leave neither final nor raw files.

| Targets | Separation writes | Cleanup |
|---|---|---|
| Stems (including Split) | Used stems to `*.wav.raw` | Each stem with its polarity; mix sums cleaned stems |
| Mix | Final stem files and an in-memory mix | Mix only |
| Both | As for Stems | Stems, then clean their summed mix again |

`Progress::Enhancing` counts windows across every cleaned stem/mix. `job::expected_runs` includes these windows. `core::timing` tracks cleanup at its own pace; batch estimates approximate cleanup using the separation pace.

## Refactoring validation (2026-11-08)

The seven-crate workspace retains all 311 pre-refactoring tests and adds one CPU identity-graph test for the borrowed tensor adapter (312 passing tests, five external-resource tests ignored by default). The adapter test constructs its tiny ONNX fixture locally and checks names, values, shapes and typed errors without external weights. Background tool tests also verify callback notification delivery.

Formatting, strict workspace Clippy, headless example builds and rustdoc with warnings denied pass on Windows. Audio without its FFmpeg feature and media-fetching each compile independently. Cargo metadata checks confirm all seven dependency boundaries, exclusive direct ORT ownership by inference and exclusive GUI ownership by app. The cached-FFmpeg integration test passes manually after moving to audio. Windows release packaging succeeds with the local DirectML and Visual C++ libraries. Native macOS packaging, live UI interaction and real-model/GPU validation were not rerun for this refactoring; the existing macOS release workflow remains available.

## 17. Risks and open questions

Stage 0 resolved DirectML startup, speed, session memory (about 1.4 GB), settings, DLL requirements, and custom STFT correctness.

| Risk | Mitigation/status |
|---|---|
| DirectML silently returns invalid output; reproduced with graph fusion, other drivers may differ | Disable fusion, validate every chunk, retry on CPU. A cached one-time GPU/CPU check per model/driver pair remains a possible addition |
| No clean Windows machine was available | Tested from an isolated folder with restricted PATH; runtime/DirectML libraries loaded locally and remaining dependencies were Windows system libraries |
| Drag/drop was not exercised in a live window | State transitions are tested; egui supplies its usual drop event |
| Third-party redistribution license inventory is incomplete | Package includes a short notice. The project now uses MIT; model/dependency restrictions still apply, including the media crate's GPL-3.0-only dependency |
| All GPU measurements use one machine; AMD/Intel behavior is unknown | Device-independent plausibility checks and CPU fallback; broader evidence awaits users |
| No Python Demucs comparison, excluded by the owner | Compare against the CPU reference export and whole-track GPU/CPU output; listening remains the user's task |
| Model repository may change or disappear | Pin revisions and SHA-256; export matching graphs if the source disappears |
| Cancellation waits while an HTTP body stalls | `ureq` has no body idle timeout; consider timed Range requests if needed |
| Interrupted model downloads restart | Models are 110–169 MB; Range requests could add resume support |
| Two app instances write the same model `.part` | SHA-256 rejects corruption; retry |
| Limited VRAM | One session is about 1.4 GB; session/run failures fall back to CPU |
| glow requires OpenGL 2.0; missing drivers, some VMs/RDP sessions may prevent window creation | Switch eframe features to wgpu if this occurs; no application code changes required |
| Weight licenses | Documented in README; personal use until clarified |
| No random shifts; quality may be slightly below default Demucs | Possible quality option listed under Later |
| GPU fallback reason appears only in logs | UI changes device name; add a Progress reason if needed |
| Native dialogs block the window | Available only from the form; asynchronous rfd is an option |
| Cleanup quality assessed numerically, not by listening | Signal-level and GPU/CPU checks completed; candidate comparison is recorded in stage 9a |
| Cleanup sessions need 2.3 GB (Voc_FT)–3.6 GB (Reverb HQ) | The requested two-job policy may use CPU fallback on smaller GPUs; historical parallel measurements are retained in section 11 |
| Instances share app.ron and GPU | Last closing instance saves settings; concurrent work slows both |
| M4A/AAC decoder retains 1024 frames (23 ms) of encoder delay in the tested file | Symphonia reads but does not apply container edits. Stems align with each other but may be offset in other editors. README documents this; MP3 delay removal works |
| Corrupt packets are skipped with a log entry, losing a few milliseconds | Matches media-player behavior; truncation midstream remains an error |
