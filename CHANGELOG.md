# Changelog

## 0.1.0 — Initial experimental release

Native Rust PlutoSDR driver using `nusb` and the native IIO FunctionFS USB
interface. No libiio, libusb, rusb, SoapySDR, or USB Ethernet/TCP dependency.
The public API is experimental and may change between releases.

### Supported

- Discover and open Pluto devices by inspecting the named IIO interface.
- Retrieve IIOD context/XML, devices, channels, scan layouts, and attributes.
- Configure RX frequency, sample rate, bandwidth, gain/AGC, and RF port.
- Receive one complex channel as normalized `Complex32` samples.
- Native blocking and asynchronous APIs, plus browser WASM/WebUSB support.
- Four queued bulk-IN transfers, reusable buffers, direct sample decoding, and
  one bounded read-ahead request.
- Explicit stop/restart and cancellation recovery through pipe shutdown.

### Validation and limits

- Native Linux hardware tests cover RX, configuration, concurrent control,
  cancellation, timeout recovery, stop/restart, and stream cleanup.
- Native benchmarks delivered about 7.1–7.2 MS/s with the default buffer and
  7.64 MS/s with a larger buffer. These are throughput measurements, not
  guarantees of uninterrupted samples.
- WASM protocol and mocked WebUSB lifecycle tests run under Node. Chrome/WebUSB
  decoded real WLAN frames with live gain/channel changes on connected hardware.
  Detailed qualification is recorded separately in `docs/protocol.md`.
- The FutureSDR WLAN browser example can retain a stale "running" status after
  USB unplug. Reconnecting, reloading the page, and starting RX restored frame
  decoding. Automatic disconnect reporting/recovery is not qualified.
- TX, timestamps, and reliable sample-loss/overflow reporting are not implemented.
- RX uses 16-bit storage for each I/Q component. Continuous 20 MS/s requires
  80 MB/s and exceeds USB 2.0 capacity. Custom firmware 8-bit modes are unsupported.
- Other host operating systems and firmware variants are not hardware-qualified.
- Rust 1.88 or newer is required. License: MIT OR Apache-2.0.
