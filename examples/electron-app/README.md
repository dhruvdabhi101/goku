# Goku Capture Test

This macOS test app loads the Rust SDK through a native Node-API addon in a worker thread in the Electron process. It runs no server and does not start recording on launch.

Build with Node on your PATH and run `npm install && npm run build`. The bundle is created under `dist/`. It is ad-hoc signed for local testing, not notarized for distribution.

Open the installed app and inspect Permissions. Request each permission explicitly, enable Goku Capture Test in System Settings, then quit and reopen if needed. Start a session, switch to a safe app, then inspect Recent frames. The delayed capture button gives you five seconds to switch. Stop ends the session.

The test app uses accessibility plus OCR on each capture and the SDK's default pixel-redaction policy. Secret detection remains best effort. Use synthetic secrets only. Images and SQLite data stay in the app's Application Support directory; Open data folder shows the location.

This is a macOS development tester. It does not establish Windows runtime support or complete SDK verification. macOS grants attach to the installed application's identity; rebuilding an ad-hoc-signed app may require renewed permission grants.
