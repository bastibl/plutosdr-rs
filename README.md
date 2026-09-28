# plutosdr-rs

Native Rust PlutoSDR IIO-over-USB driver using `nusb`. No `libiio`, `libusb`,
`rusb`, SoapySDR, C IIO bindings, USB Ethernet, or TCP transport.

Supports USB discovery, IIOD context inspection, RX configuration, and one
complex RX stream. The driver claims the named `IIO` FunctionFS interface and
uses independent control and streaming bulk endpoint pairs. TX is not implemented.
See [the source-based protocol notes](docs/protocol.md) for wire details.

The API follows `hackrf-rs`: `Device`, `DeviceBuilder`, discovery descriptors,
and operations with native `.wait()` or native/browser `.await`. All IIOD/XML
code is shared; USB code handles interface ownership and platform differences.
Unlike HackRF serials, Pluto serials are kept as exact strings.

## Try it

```sh
cargo run --example list
cargo run --example list -- --all       # inspect all USB devices/custom IDs
cargo run --example info
cargo run --example info -- YOUR_SERIAL
cargo run --features smol --example info_async
cargo run --example config             # read live settings and ranges
cargo run --example rx                 # configure and capture RX
cargo run --features smol --example rx_async
```

`list` opens devices to read descriptors but does not claim interfaces or open
IIOD pipes. It prints configurations, alternate settings, interface strings,
endpoint addresses/types/packet sizes, and logical pipe assignments. `info`
opens the native IIO pipe and prints discovered devices/channels/attributes.

Native Linux USB discovery and PRINT were hardware-verified on 2026-09-14 with
a Pluto reporting firmware `v0.35` and IIO version `0.24`. Both blocking and
async paths passed 100 context refreshes across 10 open/close cycles each,
including explicit shutdown and drop cleanup. See the
[hardware results](docs/protocol.md#hardware-validation-2026-09-14).
RX configuration, capture, cancellation, and restart were subsequently verified
on firmware v0.39 / IIO v0.26. WebUSB hardware access remains unverified. The XML unit-test fixture is synthetic, not a device capture.

## Native blocking

```rust,no_run
use plutosdr::{Device, MaybeFuture};

fn main() -> plutosdr::Result<()> {
    let mut device = Device::builder()
        // .serial("your exact USB serial")
        .open()
        .wait()?;

    for iio in &device.info().devices {
        println!("{} {:?}: {} channels", iio.id, iio.name, iio.channels.len());
    }
    device.refresh_info().wait()?;
    device.shutdown().wait()?;
    Ok(())
}
```

`Device::list()` finds standard `0456:b673` devices. For custom USB IDs, use
`usb::discovery::list_all()` (or `DeviceDescriptor::from_nusb`) and pass the
selected descriptor to `Device::builder().descriptor(...)`. Opening still
requires the `IIO` name and a valid bulk endpoint layout. Use `.interface(n)`
only to disambiguate multiple named IIO interfaces. The active configuration
must contain IIO; the driver does not switch the whole composite configuration.

## RX configuration and streaming

The same methods support native `.wait()` and native/browser `.await`:

```rust,no_run
use plutosdr::{Complex32, Device, GainMode, MaybeFuture};
# fn main() -> plutosdr::Result<()> {
let mut device = Device::open().wait()?;
device.set_frequency_hz(2_450_000_000).wait()?;
device.set_sample_rate_hz(2_500_000).wait()?;
device.set_bandwidth_hz(2_000_000).wait()?;
device.set_gain_db(30.0).wait()?; // switches to manual gain
// Or: device.set_gain_mode(GainMode::SlowAttack).wait()?;
let mut rx = device.rx_stream()?;
let mut samples = vec![Complex32::default(); rx.mtu()];
rx.start().wait()?;
let count = rx.read(&mut samples, None).wait()?;
println!("received {count} complex samples");
rx.stop().wait()?;
drop(rx);
device.shutdown().wait()?;
# Ok(())
# }
```

Controls include frequency (Hz), sample rate (samples/s), RF bandwidth (Hz),
gain (dB), `GainMode::{Manual, SlowAttack, FastAttack, Hybrid}`, and RF port
selection. Getters read hardware instead of returning requested values; AD936x
clock rounding can change readback by a few Hz. `rx_range(RxAttribute::...)`
queries firmware limits, including gain limits that change with LO frequency.
`read_rx_attribute(attr, true)` also exposes available mode/port strings.
Setting gain selects manual mode; failures may leave that mode applied. RX port
names describe internal AD936x inputs, not extra physical Pluto connectors.

No FIR filter loading or resampling is performed. With the connected firmware's
FIR state, the minimum sample rate is 2,083,333 samples/s; 2 MS/s is rejected.
Use the reported range for your firmware and current configuration. RX/TX
sample clocks are related in the hardware; sample-rate changes can affect TX.

`rx_stream_with_buffer(samples)` selects the DMA block size (default 65,536
complex frames). `read` converts signed scan words to normalized `Complex32`,
with each component in [-1, 1). Smaller destination buffers consume a cached
block without discarding its tail. The timeout bounds the complete READBUF USB
exchange; `None` uses the builder timeout. Zero polls cached samples, otherwise
returns `Error::Timeout` without USB I/O. An interrupted exchange requires
`stop` before `start`. Stop clears buffered samples and can be repeated.

One stream handle is reserved at a time, including while stopped. It owns the
USB lifetime and can outlive `Device`. Drop it before device shutdown or creating
a replacement handle. Control requests use pipe 0 while RX uses pipe 1, so they
can run concurrently. Retuning/reconfiguring while RX is active can mix old and
new settings in buffered data; stop/restart when that distinction matters.

RX queues up to four 64 KiB bulk-IN transfers inside each announced IIOD
payload, in blocking, async, and WebUSB modes. Completed USB buffers are decoded
directly into the caller's `Complex32` buffer and recycled. Full-buffer reads
need no raw payload staging copy or zero-fill; only overflow from smaller caller
buffers is copied into reusable tail storage. Requests remain bounded by the payload length, including short packets
and ZLPs, so no speculative transfer consumes the next IIOD response.

Each successful refill sends one READBUF ahead so Pluto can prepare the next
buffer while the caller processes samples. Its header is parsed and bulk-IN
reads are submitted on the next refill call; there is no background host task.
Stop closes the pipe to discard an outstanding read-ahead response. Discard the
output slice on a failed or cancelled read because decoding occurs in place.
There is no timestamping or reliable sample-loss indication in this legacy
exchange. Sustained throughput and sample continuity at the maximum advertised
rate are not guaranteed.

To measure the driver without DSP, at a 20 MS/s hardware clock:

```sh
cargo run --release --features smol --example rx_benchmark -- async
cargo run --release --features smol --example rx_benchmark -- blocking
# A larger DMA buffer amortizes IIOD requests, at the cost of latency:
cargo run --release --features smol --example rx_benchmark -- async 262144
```

On the connected Pluto, the initial transfer queue raised native async throughput
from 4.70 to 6.16 MS/s. Direct decoding and read ahead now deliver 7.1–7.2 MS/s
with the default 65,536-sample buffer; a 262,144-sample buffer reached 7.64 MS/s.
These are delivered sample rates, not continuity guarantees or browser results.
See [protocol notes](docs/protocol.md#direct-decoding-and-bounded-read-ahead) for
paired measurements and remaining costs.

## Native async

Enable `smol` or `tokio`, then await the same operations:

```rust,ignore
let mut device = plutosdr::Device::open().await?;
println!("{:#?}", device.info());
device.shutdown().await?;
```

With `tokio` alone, run inside a Tokio runtime. `smol` works with a general
executor, as shown in `info_async`. With both features nusb prefers `smol`.
No blocking USB transfer is run on the async executor; nusb offloads native
enumeration/opening and awaits endpoint completions. Native operations and
their futures are Send. Blocking use needs no runtime feature.

## WebUSB / WASM

```sh
rustup target add wasm32-unknown-unknown
cargo check --target wasm32-unknown-unknown --all-targets
cargo build --target wasm32-unknown-unknown --example webusb
```

`.cargo/config.toml` enables `web_sys_unstable_apis`, required by nusb's WebUSB
bindings. Applications depending on this library must enable that cfg in their
own build too; dependency-local Cargo configuration is not inherited.

In a secure browser window, call `Device::request_permission().await` from a
user gesture before `Device::open().await`. The browser enumerates only devices
already authorized for that origin. `examples/webusb.rs` exports an async
`inspect_pluto` function to bind to a button using `wasm-bindgen --target web`;
its return value is a printable context summary. There is no blocking browser
API. Custom IDs can use nusb's `request_device` and the descriptor builder path.

WebUSB permission, interface availability and OS driver binding still need
testing with real Pluto firmware and a WebUSB-capable browser. The build and
mock protocol tests cannot establish hardware compatibility. Native Linux
needs USB device access permissions (see [ADI's driver instructions](https://analogdevicesinc.github.io/documentation/tools/pluto-m2k/drivers/));
Windows uses nusb's WinUSB backend for the IIO interface. Do not replace drivers
on unrelated composite functions.

## Boundaries and lifecycle

- USB discovery matches the `IIO` interface string, not a fixed interface number
  or communications class alone. It preserves descriptor endpoint order.
- `IiodClient<T: Transport>` owns one serialized connection; `PRINT` parsing is
  independent of nusb and handles split/coalesced responses and length limits.
- XML includes channel direction, scan metadata, device/channel attributes,
  context properties and newer explicit buffers. Scan format is preserved as
  text. Unknown optional elements are ignored. Attribute values usually require
  a later READ; discovering an attribute does not read it.
- Opening resets all IIOD pipes on the claimed IIO interface, matching upstream.
  Use exclusive access. Opening alone does not configure the radio.
- Operations are lazy. A request cancelled after execution begins, a USB error,
  or incomplete framing poisons the session. Only shutdown is then allowed;
  reopen for further requests. Fully framed remote errors preserve the session.
- `shutdown` returns Busy while an RX stream handle exists. Otherwise it closes pipe 0 and drops the device's USB ownership on success.
  It is terminal once started, can be retried after failure, and is idempotent
  after success. Cached `info()` remains readable. Drop attempts cleanup but
  cannot report errors; native drop may block briefly, browser drop schedules
  cleanup. Browser interface release follows nusb's asynchronous drop behavior.
- Bulk transfers have a configurable three-second default timeout in both
  execution modes. Native pipe control and string requests use one second.
  nusb/WebUSB ignores control-transfer timeouts; browser control operations are
  bounded by the browser, not by that native timeout. WebUSB cannot cancel a
  submitted bulk transfer. Explicit shutdown drains queued requests before
  closing the pipe. Browser Drop retains the endpoint claims while background
  cleanup settles abandoned transfers, preventing replacement queues from
  racing old requests. A stalled device can keep that cleanup pending until
  disconnection; prefer awaiting `stop`/`shutdown`.

## Checks

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo test --doc
cargo check --target wasm32-unknown-unknown --all-targets --all-features
# Requires wasm-bindgen-cli matching Cargo.lock; runs mock tests under Node:
cargo test --target wasm32-unknown-unknown --lib --tests
# Explicit real-device smoke test:
cargo test --test hardware -- --ignored --nocapture
# Include native async lifecycle checks on the same device:
cargo test --features smol --test hardware -- --ignored --nocapture
cargo test --features smol --test rx_hardware -- --ignored --nocapture
```

Tests cover all two-fragment splits of a context reply in blocking and async
modes, single-byte async reads, surplus bytes, remote errors, size/line limits,
truncation, cancellation, shutdown retries, native Send guarantees, XML/DTD
parsing, and relocated/invalid endpoint layouts. The ignored hardware test
checks repeated PRINT, explicit shutdown, drop cleanup, and reopen by serial.
With `smol`, it runs both blocking and async lifecycle checks sequentially to
avoid competing claims on the same USB interface.

RX tests cover attribute framing and errors, fragmented READBUF chunks, masks,
signed/endian sample conversion, buffer tails, exclusive stream ownership,
stop/restart, active drop, and cancellation recovery. Hardware tests configure RX.
WebUSB tests run production nusb endpoints against delayed JavaScript USB
completions, verifying queue depth, short packets, ZLPs, preserved trailing
responses, cancellation draining, and endpoint ownership during Drop.
