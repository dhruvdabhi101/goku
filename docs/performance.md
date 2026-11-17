# Performance work and limits

Goku is an in-process Rust SDK. The Electron tester is a separate host application; its Chromium runtime is not an SDK dependency. No low-spec device has been certified by this work.

## Verified CPU-path change

`imagebuf::encode_jpeg` now borrows the redacted RGBA buffer through `ImageBuffer` and calls `JpegEncoder::encode_image`. The previous implementation allocated an RGB copy before calling `encode`. Both use the existing `image` JPEG encoder, quality 80, and the same pixel values. No codec, storage durability, or secret detection setting changed.

The removed copy holds three bytes per pixel, or 23.7 MiB of image data at 3840 by 2160. This is buffer-size arithmetic, not measured process RSS.

The benchmark measures redaction of synthetic text, painting mapped secret boxes, and JPEG encoding. It excludes screen APIs, accessibility, OCR, SQLite, filesystem synchronization, and Electron. It must not be reported as an end-to-end capture speedup.

Six alternating samples per variant ran on an Apple M5 with 24 GiB RAM and 10 logical CPUs. Each sample completed three frames. The machine was in normal desktop use, with load averages around 2 to 3. Warmup and correctness checks run before measurement. Every output is nonempty, the baseline and candidate JPEGs are byte-identical, and decoding verifies dimensions and a black redacted region.

| Frame size | Old median per frame | Borrowed RGBA median | Old range | New range |
| --- | --- | --- | --- | --- |
| 1280 by 720 | 5.808 ms | 4.365 ms | 5.615 to 7.678 ms | 4.287 to 4.841 ms |
| 1920 by 1080 | 13.359 ms | 9.652 ms | 13.174 to 13.464 ms | 9.537 to 9.726 ms |
| 3840 by 2160 | 53.473 ms | 38.184 ms | 52.808 to 54.512 ms | 37.997 to 42.021 ms |

The measured CPU-only path is 25 to 29 percent faster on these fixtures. JPEG accounts for 97 to 99 percent of that path, so reducing text work cannot double its throughput. A separate, unreported two-second `sample` run showed stacks in JPEG encoding and its forward discrete cosine transform. It did not measure live capture.

Raw measurements are in [macos-m5-core.csv](benchmarks/macos-m5-core.csv), with [host details](benchmarks/macos-m5-host.txt). The pre-change implementation is retained as `copied_rgb` in the benchmark, so both implementations run in one process with identical inputs. Tests also compare colored, variable-alpha, odd-sized images at qualities 1, 80, and 100. `prepare_ms` includes cloning the fixture as well as painting, so it is not an isolated paint timer.

## Hosted-runner results

The [benchmark run](https://github.com/dhruvdabhi101/goku/actions/runs/44085318218) passed on both hosted platforms at revision `9b66255`. The same six alternating samples and three-frame batches ran on each host.

| Host and 1080p fixture | Old median per frame | Borrowed RGBA median | Old range | New range |
| --- | --- | --- | --- | --- |
| Windows Server runner, 4 logical CPUs | 51.703 ms | 41.090 ms | 51.570 to 53.272 ms | 41.012 to 42.863 ms |
| macOS arm64 runner, 5 logical CPUs | 26.652 ms | 18.371 ms | 23.612 to 30.463 ms | 16.855 to 19.161 ms |

The 1080p CPU-only reduction is 20.5 percent on this Windows runner and 31.1 percent on this macOS runner. Do not compare absolute speeds between these unrelated hosts. Raw data and toolchain details are in [windows-runner](benchmarks/windows-runner/) and [macos-runner](benchmarks/macos-runner/). The SDK package also passed [archive installation on both platforms](https://github.com/dhruvdabhi101/goku/actions/runs/44085323260).

## Avoidable costs removed from distribution

The Electron build script now builds `goku-node` with `--release`. Previous downloadable test apps included a debug Rust library. No debug-versus-release speedup is claimed without a separate comparison.

The Node binding still overrides the Rust default with `TextMode::Both`. Switching it to accessibility first would avoid OCR when accessibility returns sufficient text. That change was withheld because the native audit found Windows screen-coordinate boxes passed into a window-local pixel painter. Removing OCR coverage before fixing and testing that mapping would be unsafe. No OCR speedup is claimed.

## Run the benchmark

```sh
cargo bench -p goku-core --bench capture -- 6 > benchmark.csv
python3 scripts/benchmark-summary.py benchmark.csv
```

The `core benchmarks` GitHub Actions workflow runs the same comparison on macOS and Windows. It uploads raw CSV, a summary, the source revision, and host information. Shared runners are useful for repeatable functional workloads but do not represent a two-core budget laptop. Results are not a strict CI performance gate because scheduling and host noise vary.

## Next experiments, in priority order

1. Fix Windows pixel-box coordinates before reducing OCR coverage. `element_rect` returns screen coordinates, but `paint_rect` uses image-local coordinates. Validate window origins, DPI, cropping, and moved windows with synthetic secrets. Then compare accessibility-first and always-OCR modes without changing the fail-closed policy.
2. Bound Windows accessibility and OCR waits. A timeout followed by an unconditional thread join can still block capture. At most one worker may be in flight; spawning replacements after timeouts is not a fix. Test with a deliberately stalled fake worker before changing native lifecycles.
3. Honor `CaptureOptions.max_width` after redaction. The field defaults to 1920 but is currently unused. Scaling must preserve painted regions and update the plan's image dimensions before storage. Benchmark at 1440p and 4K and inspect synthetic secrets at scale boundaries.
4. Use Windows UI Automation cached properties. The walker builds a cache but reads `CurrentName`, `CurrentBoundingRectangle`, and other current properties. Microsoft documents a cross-process call for each current-property read. Compare cached reads on a synthetic large tree before changing provider handling.
5. Reuse Windows D3D resources and bound OCR image copies. Device/session creation and cloned images are source-level opportunities, not measured bottlenecks. Native profiling must decide whether reuse earns its state and invalidation complexity. macOS also JPEG-encodes the raw frame before giving it to Vision. Compare a direct image input with that conversion.
6. Measure idle wakeups and permission polling. The recorder waits in 50 ms intervals and checks focus every 200 ms. OS notifications could reduce polling, but change detection and secure-input behavior need regression tests first.
7. Measure retention on a large store. Byte retention repeatedly sums image sizes and deletes the oldest row. Improve query/index behavior only after a populated-database benchmark. Keep file synchronization and the proven write order intact.

## Low-spec acceptance run

Use a Windows VM with two virtual CPUs, 4 GiB RAM, and a 1920 by 1080 desktop as a proposed test environment, not a supported minimum. Record the exact VM, power mode, screen size, and workload. Measure idle CPU, steady-state and peak RSS, capture p50 and p95 latency, dropped frames, timeouts, and output correctness. Run accessibility-rich and OCR-fallback workloads separately. Verify planted secrets never reach stored text or JPEGs.

Compare the Node SDK without Electron against the Electron tester. A small SDK benchmark cannot establish that the full Chromium host runs smoothly. No universal lowest-spec hardware claim is justified without this test.

## Sources

- [image JpegEncoder documentation](https://docs.rs/image/0.25.10/image/codecs/jpeg/struct.JpegEncoder.html) documents streaming `encode_image` support through an image view.
- [npm trusted publishing](https://docs.npmjs.com/trusted-publishers/) documents GitHub Actions OIDC publishing without a long-lived token.
- [Microsoft UI Automation caching](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-cachingforclients) explains the cost of current-property reads and bulk-fetch alternatives.
- [Microsoft screen capture](https://learn.microsoft.com/en-us/windows/apps/develop/media-authoring-processing/screen-capture) describes frame-pool and device lifecycles.
- [Apple Vision image handlers](https://developer.apple.com/documentation/vision/vnimagerequesthandler) accept Core Graphics images without an intermediate JPEG.
