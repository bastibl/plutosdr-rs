# plutosdr-rs

Native Rust PlutoSDR IIO-over-USB driver using `nusb`. No `libiio`, `libusb`,
`rusb`, SoapySDR, C IIO bindings, USB Ethernet, or TCP transport.

This is **milestone 1**: enumerate USB devices, inspect the composite USB
descriptors, claim the named `IIO` FunctionFS interface, open its control pipe,
and retrieve/parse the IIOD XML context. RF configuration and RX/TX streaming
are deliberately deferred. See [the source-based protocol notes](docs/protocol.md)
for the findings and follow-up protocol work.

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
RF configuration/streaming remain unimplemented, and WebUSB hardware access
remains unverified. The XML unit-test fixture is synthetic, not a device capture.

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
  Use exclusive access. No radio configuration commands are sent.
- Operations are lazy. A request cancelled after execution begins, a USB error,
  or incomplete framing poisons the session. Only shutdown is then allowed;
  reopen for further requests. Fully framed remote errors preserve the session.
- `shutdown` closes pipe 0 and drops the device's USB ownership on success.
  It is terminal once started, can be retried after failure, and is idempotent
  after success. Cached `info()` remains readable. Drop attempts cleanup but
  cannot report errors; native drop may block briefly, browser drop schedules
  cleanup. Browser interface release follows nusb's asynchronous drop behavior.
- Bulk transfers have a configurable three-second default timeout in both
  execution modes. Native pipe control and string requests use one second.
  nusb/WebUSB ignores control-transfer timeouts; browser control operations are
  bounded by the browser, not by that native timeout. WebUSB cannot cancel a
  submitted bulk transfer; never reuse a cancelled session.

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
```

Tests cover all two-fragment splits of a context reply in blocking and async
modes, single-byte async reads, surplus bytes, remote errors, size/line limits,
truncation, cancellation, shutdown retries, native Send guarantees, XML/DTD
parsing, and relocated/invalid endpoint layouts. The ignored hardware test
checks repeated PRINT, explicit shutdown, drop cleanup, and reopen by serial.
With `smol`, it runs both blocking and async lifecycle checks sequentially to
avoid competing claims on the same USB interface.
