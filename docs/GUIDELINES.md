# Coding guidelines

These rules apply to all repository code, including tests and examples. Most are enforced by the compiler and Clippy through `[lints]` in [Cargo.toml](../Cargo.toml); the rest are checked during review.

Three main principles:

1. **No panics.** Any operation that can fail returns `Result` or `Option`.
2. **Invariants belong in types.** Invalid states should fail to compile rather than be checked repeatedly at runtime.
3. **Idiomatic Rust.** Ownership, iterators, exhaustive matches, typed errors, and explicit dependencies.

## 1. No panics

Forbidden everywhere, including tests:

| Forbidden | Replacement |
|---|---|
| `.unwrap()`, `.expect()` | `?` with `ok_or(..)` or `map_err(..)`, `let .. else`, or `match` |
| `panic!`, `todo!`, `unimplemented!` | Return an error; do not commit unfinished code |
| `unreachable!()` | Change the types so the impossible branch cannot exist |
| `slice[i]`, `&s[a..b]` | `.get(i)`, iterators, `zip`, `chunks_exact`, `split_first`, and slice patterns such as `[a, b, rest @ ..]` |
| `assert!`, `debug_assert!` outside tests | A validating constructor returning `Result` |
| `std::process::exit` | Return a value from `main` |
| `std::thread::spawn`, which panics on an OS error | `std::thread::Builder::spawn`, which returns `Result` |
| Integer arithmetic that can overflow | `checked_*`, `saturating_*`, or division by `NonZero*` |

Additional requirements:

- Handle `JoinHandle::join()`: a worker-thread panic becomes an error visible to the user.
- Avoid `Mutex` where possible; see section 5. If one is necessary, handle `PoisonError` explicitly.
- Tests return `Result<(), E>` and use `?`. `assert!` and `assert_eq!` are allowed in tests.

```rust
// Bad
let rate = spec.sample_rate.unwrap();
let first = samples[0];

// Good
let rate = spec.sample_rate.ok_or(DecodeError::MissingSampleRate)?;
let Some((first, rest)) = samples.split_first() else {
    return Err(DecodeError::EmptyStream);
};
```

## 2. Invariants in types

Follow "parse, don't validate": validate external data once at the boundary and turn it into a value that cannot be constructed incorrectly.

- **Newtypes with private fields.** Construction goes through a function returning `Result`; getters and downstream code do not repeat validation.
- **Units in types.** Use `SampleRate`, `FrameCount`, and `ByteCount` instead of bare `u32`, `usize`, or `f32`. Two parameters of the same primitive type in succession are a reason to introduce a newtype.
- **Standard invariant-bearing types.** Use `NonZeroU32`, `NonZeroUsize`, `[f32; 2]` for a stereo frame, and `PathBuf` for paths, never `String`.
- **Enums instead of flags.** Avoid combinations of `bool` and `Option` with meaningless states. Do not introduce boolean function parameters: prefer `write(path, OverwritePolicy::Replace)` to `write(path, true)`.
- **Closed sets use enums and exhaustive matches.** `_ =>` is forbidden for project enums: adding a variant must break compilation wherever it needs handling. Do not use `#[non_exhaustive]` for project types.
- **Generate iteration together with the enum.** An enum that needs iteration uses `closed_enum!`, which generates both the enum and `ALL` from one list. Do not maintain a separate list that can silently become outdated.
- **Constants avoid fallible construction.** For example, use `NonZeroU32::MIN.saturating_add(44_099)` rather than `NonZeroU32::new(44_100)` followed by an `Option` match.
- **Compute derived data in constructors.** Validate and retain a path's directory and base name during construction, rather than making every getter return `Option`.
- **Distinct types for pipeline stages.** `DecodedAudio`, with any sample rate, and `ModelAudio`, always 44.1 kHz, cannot be confused because only the resampler constructs the latter.
- **No sentinels.** Do not use `-1`, an empty string, or `0` for absence; use `Option` or an enum.
- **Meaningful defaults only.** Do not implement `Default` merely to make construction convenient.
- **Minimal visibility.** Keep fields private and prefer `pub(crate)` to `pub`.
- Add **`#[must_use]`** to types and functions whose results must not be ignored.
- **Conversions use `From` or `TryFrom`.** Use `as` only where no standard conversion exists, such as `usize` to `f32`, and only with `#[expect(clippy::cast_.., reason = "..")]`.
- **RAII for resources.** A temporary file is removed in `Drop` unless `commit()` has been called. Cleanup must not depend on someone remembering to call a function.

```rust
// Bad: zero and negative values are representable; validation is scattered.
fn resample(samples: Vec<f32>, channels: usize, from: i32, to: i32) -> Vec<f32>

// Good: the API cannot be called with invalid values.
fn resample(audio: DecodedAudio) -> Result<ModelAudio, ResampleError>
```

## 3. Errors

- Give each layer its own error enum derived with `thiserror`. Core public signatures contain no `anyhow`, `Box<dyn Error>`, or `String`.
- Variants retain context: the path, model identifier, expected and actual values, and the underlying cause via `#[source]` or `#[from]`.
- User cancellation is a normal outcome, not an error: `Ok(JobEnd::Cancelled)`.
- Never discard errors. `let _ = fallible()` is forbidden by a lint. If an error really must be ignored, match it explicitly and explain why in a comment.
- Error `Display` implementations produce technical English text for logs. The UI builds localized user-facing messages from typed variants.

## 4. Ownership and APIs

- Functions accept borrowed values, such as `&Path` and `&[T]`, and return owned values. A `clone()` used only to satisfy the borrow checker needs justification.
- Separate pure logic from I/O. Chunk planning, windows, and overlap-add are pure functions testable without ONNX Runtime or files.
- Introduce traits at external boundaries, such as `Separator` and `ProgressSink`, rather than for every struct.
- Prefer iterators to index-based loops.
- No global mutable state: no `static mut` or singleton behind a `Mutex`. Pass dependencies explicitly.
- Isolate platform code in dedicated modules guarded by `#[cfg(..)]`. OS-specific `cfg` does not appear elsewhere.

## 5. Concurrency

- The UI thread performs no slow work: frame handlers do not read or write files, access the network, or run inference. Only immediate checks following a user action, such as whether a chosen file exists or results already exist, and modal system dialogs are allowed.
- Work runs in a separate thread and communicates through typed channel messages. Do not use shared mutable memory.
- Cancellation uses `CancelToken`, checked between chunks and between download blocks.
- Let the compiler derive `Send` and `Sync`. No manual `unsafe impl`.

## 6. `unsafe`

Crate-level `unsafe_code = "forbid"` prohibits unsafe code. If it is ever needed, isolate it in a separate crate with its own justification and review.

## 7. Dependencies

- Justify every new dependency in the commit message.
- Prefer pure-Rust crates without system libraries when other factors are equal.
- Set `default-features = false` when defaults pull in unnecessary functionality.
- Pin prerelease dependencies, currently `ort`, exactly with `=x.y.z-rc.N`.
- The core crate must not depend on GUI libraries.

## 8. Lints and formatting

Before every commit, these checks must pass without warnings:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- Apply a lint exception as `#[expect(lint, reason = "...")]` at the narrowest useful scope. Bare `#[allow]` is forbidden.
- Weaken the `Cargo.toml` lint set only in a separate, explicitly justified commit.

## 9. Tests

- Test constructors of invariant-bearing types with valid and invalid inputs.
- Test pure pipeline logic using a fake `Separator`. An identity separator, which returns its input for every stem, must reproduce the original signal after overlap-add within epsilon.
- Tests requiring network access, a GPU, or model files use `#[ignore = "reason"]` and are excluded from ordinary `cargo test` runs.

## 10. Language and comments

- Identifiers, comments, documentation comments, and commit messages are in English.
- README files, `docs/`, contributor instructions, release descriptions, and packaged instructions are in English.
- The interface is available in English and Russian. All UI text goes through `view::text`; rendering code contains no user-facing string literals. Choose translations through exhaustive locale matches, with the same function set in each language module. Preserve Russian localization and translation tests.
- Comments explain why code works a certain way, rather than restating what it does.
- Public functions returning `Result` have an `# Errors` section in their documentation.

## Before-commit checklist

- [ ] Formatting, strict Clippy, and tests pass.
- [ ] Every new `#[expect]` has a `reason`.
- [ ] New external inputs, including paths, numbers, and strings, become validated domain types at the boundary.
- [ ] Matches on project enums have no `_ =>` arm.
- [ ] New UI-thread code performs no I/O.
