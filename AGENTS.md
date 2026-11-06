# stem-split

A Rust and egui desktop application for splitting audio into stems. Inference uses ONNX Runtime (`ort`) with HTDemucs models: DirectML on Windows, CoreML on macOS, and CPU fallback.

Before working on code, read:

- [docs/GUIDELINES.md](docs/GUIDELINES.md): mandatory coding rules;
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): structure, types, algorithm, and design decisions;
- [docs/PLAN.md](docs/PLAN.md): staged plan, current status, and completed-stage results.

## Strict rules

- No `unwrap`, `expect`, `panic!`, `todo!`, `unreachable!`, indexing with `a[i]`, or string slicing. This includes tests: tests return `Result` and use `?`.
- Express invariants through types: newtypes with private fields and validating constructors, enums instead of flags, `NonZero*`, and distinct types for pipeline stages. First look for a way to make an error unrepresentable; only then consider a runtime check.
- Matches on project enums have no `_ =>` arm. Enums that need iteration must use the core's exported `closed_enum!` macro, which generates `ALL`.
- Errors are enums derived with `thiserror`. No `anyhow`, `Box<dyn Error>`, or `String` in core API signatures. Cancellation is not an error.
- `unsafe` is forbidden.
- Lint exceptions use only `#[expect(lint, reason = "...")]`. Do not weaken the lint configuration in `Cargo.toml` without an explicit request.
- Never block the UI thread: I/O, network operations, and inference run in worker threads.
- The core crate must not depend on `egui`, `eframe`, or `rfd`.

## Language

- Identifiers, comments, documentation comments, commit messages, README files, documentation, contributor instructions, release descriptions, and packaged instructions are in English.
- The interface supports English and Russian. All UI strings go through `view::text`, with a separate module for each language (`text::en`, `text::ru`) and exhaustive matches. Add every new string to all languages at once so missing translations fail to compile.
- Preserve the Russian UI localization and tests of its translations; the English-only documentation rule does not remove interface translations.

## Working branch

- Use `main` for ongoing work. Do not work on `dev` unless the owner explicitly requests it.
- When pushing the current work, target `origin/main`.

## Versions

- Automatically bump the version once for a local set of changes after the previous successful commit and push. Keep that version throughout subsequent local improvements, fixes, and new requests until the current set has both been committed and successfully pushed. A local commit without a push does not start a new bump cycle.
- A subsequent bump applies only to new changes after the previous set has been committed and successfully pushed. Choose the initial bump by scope: features increment MINOR (`1.0.0` to `1.1.0`); fixes increment PATCH (`1.0.0` to `1.0.1`). Reset PATCH when incrementing MINOR; choose MINOR when the set contains both features and fixes. If the version has already been bumped within the current local set, leave it unchanged.
- The version source is `[workspace.package].version` in the root `Cargo.toml`. Update workspace package versions in `Cargo.lock` at the same time. Experiments in `spikes/` do not inherit the application version.
- Documentation-only changes do not bump the version. Increment MAJOR only at the owner's explicit request.

## Required checks before finishing

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Packages in `spikes/` are outside the workspace; run the same commands in each affected package directory.

Release packaging uses `tools/package.ps1` on Windows and `bash tools/package-macos.sh` on Apple Silicon. The macOS package is a DMG containing an `.app` with one executable, a local ad-hoc signature, and Gatekeeper instructions. It is not notarized.

Tests requiring network access, a GPU, or model files are marked `#[ignore]` and run manually with `-- --ignored`. The complete headless workflow is the core crate's `separate` example.

Comparison against the Python implementation of Demucs was explicitly excluded from the plan by the project owner. Do not propose or perform it.

## Documentation

Update `docs/ARCHITECTURE.md` in the same change whenever decisions, structure, or dependencies change. When completing a stage, mark it complete in `docs/PLAN.md` and record its results there.
