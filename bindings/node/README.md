# Local Node binding

`NativeRecorder` wraps the Rust recorder. The Electron tester calls it on a Node worker thread, so synchronous native operations do not block the main UI thread.

```javascript
const { NativeRecorder } = require('./goku.node');
const recorder = new NativeRecorder('/absolute/path/to/captures');
const report = JSON.parse(recorder.permissions());
console.log(report.screen, report.accessibility, report.input, report.can_record);
```

`permissions()` checks without prompting. `requestPermission('screen' | 'accessibility' | 'input')` requests a permission explicitly. `openSettings(kind)` opens the corresponding macOS settings pane. Status checks cannot grant permission or guarantee that a subsequent capture succeeds. Reopen the installed app after a grant when required by macOS.

The development binding provides `start`, `stop`, `pause`, `resume`, `captureNow`, `status`, `events`, `recentFrames`, `frame`, and `frameImage`. Structured methods return JSON strings. `frameImage` returns a Buffer. This is a private test binding, not a published package or a stable API contract.
