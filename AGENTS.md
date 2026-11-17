# AGENTS.md

Rules for any agent or person changing this repository. CLAUDE.md imports this file. Read it before you edit anything.

## What this project is

Goku is an in-process SDK that captures the focused application, reads accessibility text first and OCR second, redacts secrets from text and pixels, and stores a JPEG plus a SQLite row. Hosts call it from Rust, Node/Electron, or Tauri. There is no server.

Supported platforms are macOS 14+ and Windows 10 1903+ / Windows 11. Linux is not a target.

Out of scope until the operator says otherwise: video, audio, a local HTTP server, cloud sync, telemetry, Linux, and any code copied from Screenpipe.

## Architecture

One Rust crate holds the logic. Bindings and examples only translate.

~~~
crates/goku-core/src/
  types.rs      shared data types and Config. No logic beyond parsing.
  error.rs      RecorderError. Every public failure is a variant here.
  filter.rs     which windows may be captured (apps, titles, URLs). Pure.
  redact.rs     secret detection and text redaction. Pure.
  triggers.rs   debounce and priority of capture triggers. Pure.
  imagebuf.rs   RawImage, pixel painting, JPEG encoding. Pure.
  commit.rs     plan_frame decides if a capture becomes a row. Pure.
  store.rs      SQLite rows, JPEG files, startup recovery, retention.
  platform.rs   the Platform trait and connect(). No OS calls.
  macos.rs      the only file that calls macOS APIs.
  windows.rs    the only file that calls Windows APIs.
  engine.rs     Recorder lifecycle and the capture pipeline. Wires modules.
  lib.rs        module list and the public re-exports.
bindings/node/  napi translation of Recorder. No product logic.
examples/       goku-cli and the Electron tester. No product logic.
verify/         TLA+ model of the store write order.
crates/goku-core/tests/commit_space.rs   exhaustive check of plan_frame.
~~~

Dependencies point one way:

~~~
types, error
  <- filter, redact, triggers, imagebuf, commit   (pure)
  <- store, platform
  <- macos, windows, engine
  <- lib
  <- bindings, examples
~~~

A pure module never imports store, engine, platform, macos, or windows. `scripts/check-architecture.sh` enforces this in CI.

### The capture pipeline

`engine::run_capture` runs these steps in this order. Keep the order.

1. Read focus. Apply `CaptureFilter::check_window` (own app, apps, titles). A skipped window makes no OS capture call and writes no row.
2. Check secure input and screen permission.
3. Capture pixels for the resolved scope.
4. Walk accessibility, then run OCR if text is thin or the mode asks for it.
5. Apply `CaptureFilter::check_url` with the walked browser URL. A skipped window writes no row. The pixels are dropped in memory.
6. Read focus again. If the window changed, discard the capture.
7. Redact text and map secrets to pixel boxes.
8. `plan_frame` decides the row and the image. `prepare_image` is the only way to get a persistable image.
9. `Store::commit` writes `tmp/*.partial`, renames it, inserts the row, and deletes the file if the insert fails.

### Invariants

Breaking one of these is a bug, even if every test passes.

- The store never writes a JPEG built from a `RawImage`. It takes `PersistableImage` only.
- A final JPEG path exists only when a row points at it, or after a crash that the next `Store::open` cleans up.
- A capture whose focus changed between step 1 and step 6 is never stored.
- A secret that cannot be mapped to pixels withholds the image under the default policy.
- Captured text, titles, and URLs are never logged and never appear in error messages.
- Every OS call that can hang has a timeout and at most one in-flight worker.
- Input Monitoring is never needed by default. Click, typing, and scroll triggers are opt-in.

## How to add or change a feature

The goal: a feature touches few files, and each file has one reason to change.

1. Name the data shape first. Put new types in the module that owns the concern, or in `types.rs` if more than one module reads them.
2. A new concern gets its own module with a pure function and its own unit tests. `filter.rs` is the example to copy.
3. Wire it into `engine.rs` at one call site in the pipeline. Do not spread the same check across several places.
4. OS work goes behind a `Platform` method. Implement it in `macos.rs`, `windows.rs`, and the fake platform in the `engine.rs` tests.
5. Expose it in `bindings/node` only after the Rust API is settled.
6. If behavior a host can see changed, update the README or docs in the same change.

### Change budget

- One pull request is one change with its own tests. Do not mix a refactor and a feature.
- A pull request touches at most 12 source files, not counting tests, docs, and lockfiles. `scripts/check-architecture.sh` counts them in CI. If a change needs more, split it into a stack. Only the operator may add the `wide-change` label to override.
- No Rust file may exceed 1500 lines. `engine.rs` and `macos.rs` are close. When you touch them, move code out instead of growing them. Examples: the fake platform into its own test module, macOS OCR into its own file.
- A new public type or function needs a caller in the same change. No speculative APIs.

## How to write code here

- No tokio. The recorder is one worker thread with channels.
- No `unwrap` or `expect` outside tests and static regex construction. FFI boundaries must not panic.
- Never hold a mutex across a platform call.
- Validate at boundaries: config, bindings, file paths. Trust internal types.
- No inline comments except a short reason the code cannot show. No commented-out code. No dead code.
- Tests assert behavior a caller can observe. Never weaken, rename, or delete a test to make it pass. Fix the code.
- New dependencies must be MIT, Apache-2.0, BSD, or Zlib. Put platform-only crates under the matching target table in Cargo.toml.

## How to verify

Run these before you push. CI runs them on macOS and Windows.

~~~
cargo test --workspace
scripts/check-architecture.sh
cargo run -p goku-cli -- doctor
cd examples/electron-app && npm ci && npm run build && npm test
~~~

- Live capture is verified only on the OS that runs it. A Windows compile check on a Mac is not a Windows test.
- A change to `commit.rs` keeps `tests/commit_space.rs` passing, and that test must still fail on its broken variant.
- A change to the write order in `store.rs` updates `verify/StoreCommit.tla` in the same change.
- Never claim a capture, a permission grant, or a Windows result you did not observe.

## Writing docs and messages

Plain words. Short sentences. Name the real file, function, or command. No marketing words. Say what is verified and what is not.
