# Implementation Plan — Goku Capture SDK

Execution plan for the product plan in [docs/PLAN-capture-sdk.md](docs/PLAN-capture-sdk.md).

Status: in progress. On 2026-11-16 the operator locked macOS and Windows for this implementation. No application code existed when that call was made.

Local test app update, 2026-11-17. `examples/electron-app` loads the Rust SDK through `bindings/node`, displays native permission checks, and provides explicit request/settings and capture-session controls. An ad-hoc-signed Apple Silicon build was installed as `/Applications/Goku Capture Test.app`. The installed-bundle UI smoke test passed without starting recording or requesting permission. Screen Recording read as denied. Real capture is still unverified pending the user's grant. Core checks passed with `cargo test -p goku-core -- --test-threads=1`, including the bounded commit-space checks. The two singleton lifecycle tests use separate local locks and conflict under parallel test execution; this serial pass does not establish a parallel-suite pass. Windows and Tauri are not verified by this tester.

The previous draft treated Windows and Linux as compile stubs and left Windows for phase 3. That draft is wrong for this run. Windows is a real backend. Linux is out of v1 and has no stub module. A build on Linux fails with `unsupported platform` rather than pretending to capture.

---

## Objective

Ship a working, embeddable capture SDK that:

- starts/stops a capture session,
- captures only the focused application/window,
- captures on events (app switch, window focus, idle, manual; input events where available),
- extracts accessibility text first, OCR fallback,
- redacts secrets from text and mapped pixel regions,
- stores JPEGs + SQLite rows,
- exposes recent frames, status, diagnostics, permissions,
- works from Rust, Node/Electron (napi), and Tauri (plugin),
- runs no server.

---

## Repo layout (actual)

```
goku/
  Cargo.toml                     # workspace
  crates/
    goku-core/                   # all core modules for v1
      src/
        lib.rs
        types.rs                 # config, frame, status, errors, events
        permissions.rs           # macOS TCC status/request/settings
        capture/mod.rs           # screenshot strategy + image encoding
        capture/macos.rs         # ScreenCaptureKit
        a11y/mod.rs              # focused app/window + AX tree
        a11y/macos.rs            # axuielement walk
        ocr/mod.rs               # OCR trait + fallback
        ocr/apple.rs             # Apple Vision
        redact/mod.rs            # secret detection + text/pixel redaction
        store/mod.rs             # SQLite + JPEG/thumbnail files + retention
        engine/mod.rs            # Recorder orchestration + triggers
        engine/triggers.rs       # focus polling + idle + manual + input tap
      tests/
  bindings/
    node/                        # napi-rs addon
    tauri/                       # tauri v2 plugin
  examples/
    goku-cli/                    # real end-to-end CLI
    electron-app/                # Electron example
    tauri-app/                   # Tauri example
  docs/
    RESEARCH-screenpipe.md
    PLAN-capture-sdk.md
  IMPLEMENTATION_PLAN.md
```

---

## Milestones

### M0 — Workspace + core types + redaction + store

- [ ] Cargo workspace compiles on macOS.
- [ ] `Config`, `Frame`, `FrameSummary`, `Status`, `RecorderEvent`, `RecorderError`.
- [ ] Secret detector: provider keys, JWTs, private keys, connection strings, env assignments, password context, high-entropy context.
- [ ] Text redaction with labels + summary.
- [ ] Pixel region redaction (opaque black).
- [ ] SQLite schema: `sessions`, `frames`, indexes.
- [ ] JPEG + atomic write; path validation; retention.
- [ ] Unit tests for redaction and store.

Acceptance: `cargo test -p goku-core` passes; redaction fixtures prove positives and negatives.

### M1 — macOS permissions

- [ ] Screen Recording status via `CGPreflightScreenCaptureAccess` + real SCK probe.
- [ ] Accessibility status via `AXIsProcessTrusted`.
- [ ] Input Monitoring status via `CGPreflightListenEventAccess`.
- [ ] Request prompts for each.
- [ ] Open exact System Settings panes.
- [ ] `PermissionReport` with `canRecord`, `degradations`, `restartRequired`.
- [ ] Polling watcher for changes.

Acceptance: CLI `goku-cli doctor` prints the real status on this Mac.

### M2 — macOS capture

- [ ] Focused app/window metadata (pid, app, bundle, title, rect, display).
- [ ] `SCShareableContent` mapping pid → app + window.
- [ ] `focusedApp` strategy: display + including-app filter + crop.
- [ ] `focusedWindow` strategy: window-only filter.
- [ ] `displayCrop` strategy: display + crop.
- [ ] CGImage → RGBA → `image::DynamicImage`.
- [ ] JPEG encode with quality + max width.
- [ ] Bounded capture (timeout), fail-closed behavior.
- [ ] Focus-token coherence check.

Acceptance: CLI captures the focused app to a valid JPEG and prints dimensions/latency.

### M3 — Accessibility text

- [ ] `axuielement` focused application/window.
- [ ] Window title, app name, bundle id, bounds.
- [ ] Bounded DFS text walk with roles/values/descriptions/positions.
- [ ] Skip `AXSecureTextField` values.
- [ ] Browser URL extraction from `AXWebArea`.
- [ ] Timeouts + node/depth/text caps.
- [ ] Thin-text heuristic.

Acceptance: CLI prints real AX text from Chrome/TextEdit/Finder.

### M4 — OCR fallback

- [ ] Apple Vision OCR from CGImage (temp file or CVPixelBuffer path).
- [ ] Text + normalized boxes.
- [ ] OCR only when AX empty/thin or `textMode=both`.
- [ ] Timeout + error handling.

Acceptance: CLI OCRs a canvas/image window and prints text + box count.

### M5 — Capture engine + store

- [ ] `Recorder::start/stop/pause/resume/capture_now`.
- [ ] Trigger loop: focus polling (app switch/window focus), idle timer, manual.
- [ ] Debounce/coalescing/min interval.
- [ ] Capture transaction: pixels + AX + OCR + redaction + JPEG + DB row.
- [ ] Session lifecycle + interrupted-session cleanup.
- [ ] Events broadcast.
- [ ] `status()`, `recent_frames()`, `frame()`, `frame_image()`, `diagnose()`.
- [ ] Retention worker.

Acceptance: CLI runs a 10s session while switching apps; DB has frames with redacted text and valid JPEGs; recent frames API returns them.

### M6 — Input event triggers (macOS)

- [ ] Listen-only CGEventTap on a dedicated thread.
- [ ] Click → `click` trigger with settle.
- [ ] Key down → `typing_pause` after quiet period.
- [ ] Scroll → `scroll_stop` after quiet period.
- [ ] Graceful degradation without Input Monitoring.
- [ ] Secure input check (`IsSecureEventInputEnabled`) pauses capture.

Acceptance: typing in TextEdit then pausing produces a `typing_pause` frame.

### M7 — Node/Electron binding

- [ ] napi-rs addon `@goku/recorder` with `Recorder` class.
- [ ] `start/stop/pause/resume/status/captureNow/getRecentFrames/getFrame/getFrameImage/diagnose`.
- [ ] Permission functions.
- [ ] Event emitter via threadsafe function.
- [ ] Electron main/preload wrapper.
- [ ] Example app.

Acceptance: Node script starts a session, switches focus, reads a frame image buffer and text.

### M8 — Tauri plugin

- [ ] `tauri-plugin-goku` commands + events + v2 permissions.
- [ ] Example app skeleton.
- [ ] `cargo check -p goku-tauri`.

Acceptance: `cargo check` passes; commands mirror Node surface.

### M6b — Windows capture, text, and triggers

- [ ] Focused window via the foreground HWND, not a display app-filter.
- [ ] `focusedApp` requests fall back to window capture and set a degradation.
- [ ] `displayCrop` is monitor capture plus crop, and the frame records that other apps may be included.
- [ ] UIA walk on a COM thread with depth, node, and time caps.
- [ ] Windows.Media.Ocr when AX text is thin.
- [ ] Low-level hooks for click, typing pause, and scroll stop. Missing hook degrades. It does not fail the session.
- [ ] Borderless capture requested where the OS allows it.

Acceptance: `cargo check --target x86_64-pc-windows-msvc -p goku-core` passes, or the log names the toolchain gap. A live Windows capture is not claimed from this Mac.

### M9 — Verification and docs

- [ ] Unit tests (redaction, store, path validation, trigger debounce).
- [ ] Integration test (`#[ignore]` if permissions missing) for real capture/AX/OCR.
- [ ] `cargo fmt`, `cargo clippy`, `cargo test`.
- [ ] README usage + permission matrix + limitations.

---

## Key engineering decisions (locked for this implementation)

- **Rust core, in-process.** No server, no ports.
- **macOS 14+** for `SCScreenshotManager`.
- **Focused app default** = display capture with including-application filter, cropped to window.
- **AX first, OCR fallback.** OCR on the window image.
- **Secrets-only redaction**, text + mapped regions. Pixel redaction before final JPEG rename.
- **SQLite + JPEG files**, session-scoped.
- **One Recorder per process.**
- **macOS 14+ and Windows 10 1903+ / Windows 11.** Linux is not a target.
- **`focusedApp` is macOS-only.** ScreenCaptureKit can include one application's windows in a display capture. Windows Graphics Capture cannot. On Windows, `focusedApp` falls back to `focusedWindow` and the status records that fallback. `displayCrop` on Windows captures the monitor and crops, so other apps' pixels can appear. Do not describe that as compositor exclusion.
- **One-shot capture, not a video pipeline.** Use `screencapturekit` `SCScreenshotManager` on macOS. Use the `windows` crate's Graphics Capture API on Windows. Do not depend on `windows-capture`. That crate is a stream and encoder stack, and this SDK does not record video.
- **No tokio in the core.** One recorder thread and channels. Hosts already have their own runtimes.
- **`RedactedImage` is the only pixel type the store accepts.** The constructor paints or withholds. A raw buffer cannot be written under the final JPEG name.
- **Formal check on the commit transaction**, not on the OS capture calls. The check must fail on a broken variant before it is trusted.

---

## Verification commands

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p goku-cli -- doctor
cargo run -p goku-cli -- capture --out /tmp/goku-test --seconds 10
cargo run -p goku-cli -- frames --dir /tmp/goku-test --limit 5
cd bindings/node && npm run build && npm test
cargo check -p goku-tauri
cargo check --target x86_64-pc-windows-msvc -p goku-core
```

The Windows target check is a compile check. It is not a live capture. Live capture is verified on the OS that runs it.

---

## Current status

| Milestone | Status |
|---|---|
| M0 workspace/types/redaction/store | not started |
| M1 permissions | not started |
| M2 capture | not started |
| M3 accessibility | not started |
| M4 OCR | not started |
| M5 engine/store | not started |
| M6 input triggers | not started |
| M7 Node/Electron | not started |
| M8 Tauri | not started |
| M6b Windows capture/text/triggers | not started |
| M9 verification/docs | not started |
| M10 formal check of the commit transaction | not started |

_Update this table only when a milestone is actually implemented and verified._
