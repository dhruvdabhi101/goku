# Goku — Focused-App Capture SDK

This repo will contain a small, embeddable SDK for capturing **what the user is working on** — not their whole screen.

It takes the useful part of Screenpipe's recording mechanism (event-driven screenshots + accessibility text + OCR), scopes it to the **focused application only**, adds **secrets-only redaction**, and exposes it as a native SDK for **Electron** and **Tauri** with **no server**.

## Read this first

1. **[docs/RESEARCH-screenpipe.md](docs/RESEARCH-screenpipe.md)** — how Screenpipe actually works, verified against source: triggers, paired capture, AX tree walking, OCR gating, storage, permissions, redaction, and the existing SDK.
2. **[docs/PLAN-capture-sdk.md](docs/PLAN-capture-sdk.md)** — the product plan: architecture, capture strategies, API surface, permission model, data model, phases, risks, and the decisions I need from you.
3. **[IMPLEMENTATION_PLAN.md](IMPLEMENTATION_PLAN.md)** — the engineering execution plan: milestones, file layout, acceptance criteria, and verification commands.

Suggested read order if you are short on time:

- Research: §1 (TL;DR), §3.3 (paired capture), §4.3 (the three capture scopes), §7.1 (permissions), §8 (redaction), §9.3 (SDK gaps).
- Plan: §1 (three decisions), §4 (capture scope), §7 (redaction), §8 (permissions), §19–20 (decisions + grill questions).

## The short version

```
event (app switch / click / typing pause / scroll stop / idle / manual)
  → focused-window screenshot
  → accessibility text first, OCR fallback
  → secrets redacted from text and pixels
  → JPEG + SQLite row with one shared timestamp
  → SDK: start / stop / status / recent frames / permissions
```

- **No server.** In-process SDK.
- **No video, audio, meetings, agents, sync, or telemetry.**
- **Focused app only**, not every monitor.
- **Secrets only**, not full PII.
- **Electron** via napi-rs; **Tauri** via a Rust plugin.
- **Real permission APIs**: Screen Recording, Accessibility, Input Monitoring — status, request, settings deep link, change events, degradation report.

## Status

A macOS arm64 and Windows x64 permission and capture tester is available in [examples/electron-app](examples/electron-app/README.md). It uses the Rust SDK through a native Node addon, starts idle, and exposes permission status, requests, settings shortcuts, session controls, and recent frames. Its presence does not establish low-spec performance or release readiness. Windows redaction coordinates and native timeout handling still need fixes and VM validation. Use synthetic test data rather than sensitive workloads.

Decisions in the product plan are locked, except the SDK license. Implementation is in progress for macOS 14+ and Windows 10 1903+ / Windows 11. Linux is not a target.

`focusedApp` (display capture that includes only the focused application's windows) exists on macOS. Windows Graphics Capture has no equivalent filter, so Windows uses window capture for that setting and says so in the status.

## Install the experimental Node SDK

```sh
npm install https://github.com/dhruvdabhi101/goku/releases/download/sdk-v0.1.0/goku-capture-sdk-0.1.0.tgz
```

The archive includes prebuilt release binaries for macOS arm64 and Windows x64. It needs Node 20 or newer, not Rust or Electron. It runs no install scripts and never starts recording on import. Import `NativeRecorder` from `goku-capture-sdk`; see [the Node SDK guide](bindings/node/README.md) for the API and host permission requirements.

This is a public archive, not an npm registry publication. The project remains `UNLICENSED` until the owner chooses a license. The [performance report](docs/performance.md) records measured improvements, runner benchmarks, and the remaining low-spec validation work.
