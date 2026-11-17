# Install the Node SDK

The SDK archive contains optimized native binaries for macOS arm64 and Windows x64. It does not include Electron. Installation needs no Rust compiler and runs no install scripts. The current package name is `goku-capture-sdk`; it is not published to the npm registry.

```sh
npm install https://github.com/dhruvdabhi101/goku/releases/download/sdk-v0.1.0/goku-capture-sdk-0.1.0.tgz
```

`NativeRecorder` wraps the Rust recorder. The Electron tester calls it on a Node worker thread, so synchronous native operations do not block the main UI thread.

```javascript
const { NativeRecorder } = require('goku-capture-sdk');
const recorder = new NativeRecorder('/absolute/path/to/captures');
const report = JSON.parse(recorder.permissions());
console.log(report.screen, report.accessibility, report.input, report.can_record);
```

`permissions()` checks without prompting. `requestPermission('screen' | 'accessibility' | 'input')` requests a permission explicitly. `openSettings(kind)` opens the corresponding macOS settings pane. Status checks cannot grant permission or guarantee that a subsequent capture succeeds. Reopen the installed app after a grant when required by macOS.

The development binding provides `start`, `stop`, `pause`, `resume`, `captureNow`, `status`, `events`, `recentFrames`, `frame`, and `frameImage`. Structured methods return JSON strings. `frameImage` returns a Buffer. The constructor accepts an optional filter JSON string as its second argument. The API is experimental.

The Node binding keeps both accessibility and OCR enabled on each capture. The Rust default is accessibility first with OCR fallback, but this binding intentionally retains its existing behavior pending a Windows redaction-coordinate fix. Redaction and the default image-withholding policy remain enabled. The synchronous methods belong on a Node worker thread, as in `examples/electron-app/native-worker.cjs`, not on an Electron UI thread. Starting a recorder is explicit; constructing one never starts recording. Use synthetic secrets in the tester until Windows pixel mapping has been validated in a VM.

The archive supports Node 20 or newer. The native addon uses Node-API version 6 rather than a specific Node ABI. Electron hosts must unpack `.node` files outside ASAR archives. macOS still requires the host's Screen Recording permission. Windows builds target x64 MSVC; CI verifies loading on a hosted Windows image, not a clean low-spec VM. A missing runtime-DLL error may require the [Microsoft Visual C++ x64 Redistributable](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist). Windows ARM64 and Intel macOS binaries are not included.

The package keeps `private: true` and `UNLICENSED` until the owner chooses a registry identity and license. `npm pack` and installation from an archive work with this flag. Registry publishing does not. A public source repository does not grant an open-source license.

## Build and verify an archive

Run the `Node SDK package` workflow in GitHub Actions. It builds release binaries on both platforms, combines them in one archive, and installs that exact archive on macOS and Windows with install scripts disabled. The checks load the native addon and verify that startup neither records nor writes a capture database. They do not request permissions or capture the screen.

The first archive bundles both binaries to avoid registry dependencies and postinstall downloads. Platform-specific optional packages would reduce download size as target support grows, but require separate registry packages and coordinated publication. Use npm trusted publishing after the initial registry package exists, rather than storing a long-lived npm token in this repository.
