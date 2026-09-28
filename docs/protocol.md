# Native Pluto IIO USB protocol

Research snapshot: 2026-09-11, extended for RX on 2026-09-14. The initial milestone covered:
USB discovery, opening the IIO control pipe, and ASCII `PRINT` context discovery.
RX attribute control and streaming are now implemented as described below.
The protocol is implemented independently in Rust;
the C sources below are references, not linked or built dependencies.

## Sources inspected

Upstream libiio main at `a56b3d47db7cad429b57d56e10739d46db896dfd`:

- [usb.c](https://github.com/analogdevicesinc/libiio/blob/a56b3d47db7cad429b57d56e10739d46db896dfd/usb.c):
  `iio_usb_match_interface`, `usb_verify_eps`, `usb_create_context`,
  `usb_reset_pipes`, `usb_open_pipe`, `usb_close_pipe`, `usb_open_buffer`,
  `usb_reserve_ep_unlocked`, `usb_shutdown`, `usb_iiod_client_ops`.
- [iiod/usbd.c](https://github.com/analogdevicesinc/libiio/blob/a56b3d47db7cad429b57d56e10739d46db896dfd/iiod/usbd.c):
  `ffs_strings`, `create_header`, `handle_event`, `usb_open_pipe`,
  `usb_close_pipe`, `usbd_client_thread`.
- [iiod-client.c](https://github.com/analogdevicesinc/libiio/blob/a56b3d47db7cad429b57d56e10739d46db896dfd/iiod-client.c):
  `iiod_client_read_integer`, `iiod_client_create_context_private`,
  `iiod_client_enable_binary`, `iiod_client_read_attr`, `iiod_client_write_attr`,
  `iiod_client_open_with_mask`, `iiod_client_read_unlocked`,
  `iiod_client_write_unlocked`, `iiod_client_close_unlocked`.
- [iiod/parser.y](https://github.com/analogdevicesinc/libiio/blob/a56b3d47db7cad429b57d56e10739d46db896dfd/iiod/parser.y):
  command grammar and `PRINT` / `ZPRINT` / `BINARY` actions.
- [iiod/ops.c](https://github.com/analogdevicesinc/libiio/blob/a56b3d47db7cad429b57d56e10739d46db896dfd/iiod/ops.c):
  `read_line`, `send_data`, `print_value`, attribute and buffer operations.
- [context.c](https://github.com/analogdevicesinc/libiio/blob/a56b3d47db7cad429b57d56e10739d46db896dfd/context.c)
  (`xml_header`) and [xml.c](https://github.com/analogdevicesinc/libiio/blob/a56b3d47db7cad429b57d56e10739d46db896dfd/xml.c): XML schema and parsing.

Also checked the descriptor and PRINT implementation in
[libiio v0.26](https://github.com/analogdevicesinc/libiio/tree/v0.26).
[Pluto firmware v0.39](https://github.com/analogdevicesinc/plutosdr-fw/releases/tag/v0.39)
lists libiio v0.26, so supporting the legacy ASCII exchange matters even though
current libiio negotiates a newer binary protocol.

Independent reference:
[SDRoxide's IIOD client](https://github.com/dividebysandwich/sdroxide/blob/e5f529e8c6e6b38848175c2f4239f4400caca735/crates/sdroxide-pluto/src/iiod.rs)
uses synchronous TCP. It is useful as a Rust comparison, but it is not a USB
reference. Its READBUF commentary says to read a mask on every chunk; the
inspected upstream client reads it only once per request. Future streaming
implementation must follow upstream client/server behavior and hardware traces.

## Interface and endpoint discovery

Normal Pluto USB identity is VID `0456`, PID `b673`; DFU PID `b674` is not a
running IIOD device ([ADI firmware documentation](https://wiki.analog.com/university/tools/pluto/users/firmware)).
The VID/PID is a discovery hint. The actual IIO function is identified by its
interface string descriptor, exactly `IIO`, as in `iio_usb_match_interface`.

`create_header` zero-initializes descriptors and sets interface class `02`
(communications), subclass `00`, protocol `00`, and `iInterface = 1` in the
FunctionFS-local string table (US English `0409`). Composite gadget assembly
can renumber interfaces, string indices, and endpoint addresses. Never assume
host interface 0, string index 1, or endpoint addresses `81`/`01`.

Enumerate configurations and alternate settings. Match `IIO` in the active
configuration, validate the endpoint layout, claim only that interface, and
select its alternate setting. Class/subclass/protocol are diagnostics, not a
substitute for the name. Do not claim RNDIS, serial, or mass storage interfaces.

Endpoints are bulk and ordered **IN, OUT, IN, OUT, ...** in the interface's
descriptors. `usb_verify_eps` requires a nonempty even count and alternating
directions. `usb_create_context` assigns pair `i` from descriptors `2*i` and
`2*i+1`; **pipe ID is the pair index**, not the USB endpoint number. Do not sort
addresses or pair endpoints by matching numeric addresses. Pair 0 is reserved
for context/attribute operations. Additional pairs host independent IIOD
interpreter sessions for streaming buffers. Their count is firmware-dependent.
FunctionFS initially describes `81/01`, `82/02`, etc.; those are not guaranteed
host addresses. Packet sizes declared by upstream are 64/512/1024 bytes for
full/high/super speed.

## Pipe lifecycle on endpoint zero

Vendor **OUT**, recipient **interface** (`bmRequestType = 0x41`):

| Operation | bRequest | wValue | wIndex | Data |
| --- | --- | --- | --- | --- |
| RESET_PIPES | 0 | 0 | IIO interface number | empty |
| OPEN_PIPE | 1 | pair index | IIO interface number | empty |
| CLOSE_PIPE | 2 | pair index | IIO interface number | empty |

libiio claims the interface, resets pipes, opens pipe 0, then starts its IIOD
client. These control requests have no IIOD integer reply; USB completion
reports their transport status. RESET closes all sessions on this IIO function,
so opening assumes exclusive ownership. It is not a USB device reset.

Daemon `usb_open_pipe` opens FunctionFS files `ep(2*i+1)` for device writes
(host IN) and `ep(2*i+2)` for device reads (host OUT), then starts an interpreter.
Closing signals that session to stop; reopening joins its old threads first.
libiio shutdown resets all pipes. The driver explicitly closes each owned
pipe on shutdown and performs best-effort cleanup on drop/open failure.

## Minimal exchange and framing

After RESET_PIPES and OPEN_PIPE(0):

```text
host bulk OUT, pair 0:  PRINT\r\n
device bulk IN:        <decimal XML byte length>\n
device bulk IN:        <exactly length bytes of UTF-8 XML>
device bulk IN:        \n
```

The final newline is **not counted** in the length. XML has its own declaration
and internal DTD on standard firmware. Parse the complete length-delimited
payload, not lines or USB packet boundaries. Preserve surplus received bytes,
handle partial completions, bound line/XML lengths, and reject a missing trailer.
Negative decimal replies represent Linux `-errno` (e.g. `-22` is EINVAL);
preserve the numeric value rather than interpreting it as a host OS error.
Other commands have different framing: VERSION is textual, not an integer reply.

The ASCII command language is shared with TCP, but transport behavior is not
identical. USB adds pipe control and endpoint allocation. In the USB path,
daemon `read_line` returns one FunctionFS read, while the TCP path searches for
newline. Send each short command as **one bulk OUT transfer**; do not fragment
or combine commands through a generic buffered writer. libiio similarly uses a
bulk read for its USB `read_line` callback. The Rust response parser still treats
incoming completions as fragments of a byte stream. Short packets/ZLPs can end
USB transfers; they are not the IIOD payload length or necessarily EOF.

Current libiio tries `BINARY\r\n` and uses binary operations after success.
ASCII remains the initial interpreter mode and supports PRINT directly. This
milestone never negotiates BINARY or ZPRINT, avoiding unnecessary version and
compression machinery. A future binary implementation needs its own framing;
binary buffer commands must not be mixed with ASCII sessions.

## ASCII attributes and streaming

XML describes context metadata, IIO device IDs/names/labels, input/output
channels, attribute names/filenames, and scan-element index/format/scale.
Attribute values generally require a subsequent READ; XML is not a snapshot
of all current radio settings. New XML can also contain explicit buffer nodes.

ASCII attribute commands from `iiod_client_read_attr` / `iiod_client_write_attr`:

```text
READ <device> [INPUT|OUTPUT <channel>] <attribute>\r\n
WRITE <device> [INPUT|OUTPUT <channel>] <attribute> <byte-count>\r\n
<exactly byte-count value bytes>
```

DEBUG and BUFFER forms address other attribute namespaces. READ returns a
decimal length, bytes, then newline. WRITE sends the value before reading the
integer status. Bulk/all-attribute operations have additional framing and are
out of scope. Pluto configuration resolves PHY/RX/TX devices from
context data (commonly `ad9361-phy`, `cf-ad9361-lpc`, `cf-ad9361-dds-core-lpc`),
then uses attributes rather than host register writes or fixed `iio:deviceN` IDs.

For an ASCII streaming session, reserve an additional endpoint pair, open its
USB pipe, then send:

```text
OPEN <device> <sample-count> <channel-mask> [CYCLIC]\r\n
```

The mask is hexadecimal, eight digits per 32-bit word, most significant word
first. The sample count counts scan frames; their byte layout comes from enabled
scan elements and alignment, not just an assumed pair of i16 values. Successful
OPEN returns a nonnegative status. CLOSE takes the device ID and returns status;
then close the USB pipe and release the reserved pair.

RX: `READBUF <device> <byte-count>\r\n`, followed by decimal chunk lengths.
The first positive chunk is preceded by the channel mask plus newline. Read
each announced raw chunk; finish when requested bytes are satisfied, or a zero
length terminates early. A negative length is an error. Do not wait for an extra
zero after receiving exactly the requested byte count.

TX: `WRITEBUF <device> <byte-count>\r\n`, read an initial integer acknowledgement,
send exactly that many raw bytes, then read a second integer completion status.
Do not send TX data before a successful acknowledgement. Cyclic operation and
buffer/block lifecycle need dedicated follow-up work and tests.

## Architecture and execution decisions

The local `~/src/hackrf-rs` was inspected: `discovery.rs`, `device.rs`,
`maybe_future.rs`, `usb/hardware.rs`, `streaming.rs`, `high_level.rs`, its examples,
and public/lifecycle tests. Reuse its `Device` / `DeviceBuilder` / descriptor
naming, `nusb::MaybeFuture` (.wait on native, .await on all targets), native Send
guarantees, explicit shutdown, and wasm-only permission request. Native async
opening uses nusb's optional `smol` or `tokio` integration; blocking use needs
neither. Do not import HackRF control requests, queue dimensions, IQ format, or
its half-duplex restriction.

Keep descriptor discovery and bulk/control calls in `usb`, wire parsing and XML
models in `iiod`, and public device conveniences above both. An IIOD client owns
one transport and serializes commands through mutable access. After cancellation
or incomplete framing it must reject further commands until reopened, since
the next received bytes could belong to the abandoned response. Browser WebUSB
cannot cancel submitted bulk transfers; explicit shutdown and reopening are
especially important. Browser compilation alone does not prove access to a
particular firmware/OS/browser combination. Hardware smoke tests are separate
from deterministic protocol tests.

## Hardware validation: 2026-09-14

The first milestone passed on a connected Pluto through native Linux USB using
the production nusb transport. No driver fixes were required.

Observed device metadata:

- USB `0456:b673`, product `PlutoSDR (ADALM-PLUTO)`.
- Firmware `v0.35`; context IIO version `0.24` (`version-git=v0.24`).
- Reported model `Analog Devices PlutoSDR Rev.B (Z7010-AD9364)`.
- Linux `5.10.0-98231-g9dfba10b795d`.
- Active configuration 1, IIO interface 5, alternate setting 0, class `02/00/00`.
- Bulk maximum packet size 512 bytes; pairs in descriptor order:

| Logical pipe | Host IN | Host OUT |
| --- | --- | --- |
| 0 (control) | `0x86` | `0x04` |
| 1 | `0x87` | `0x05` |
| 2 | `0x88` | `0x06` |

PRINT returned five devices: `adm1177-iio`, `ad9361-phy`, `xadc`,
`cf-ad9361-dds-core-lpc` (TX), and `cf-ad9361-lpc` (RX). The advertised RX scan
format is `le:S12/16>>0`; TX is `le:S16/16>>0`. Those are XML observations,
not validation of actual sample transfers.

Successful commands:

```sh
cargo run --example list
cargo run --example info
cargo run --features smol --example info_async
cargo test --test hardware -- --ignored --nocapture
cargo test --features smol --test hardware -- --ignored --nocapture
```

The extended hardware test verifies context equality across 100 refreshes in
10 blocking sessions and another 100 refreshes in 10 async sessions, alternating
explicit shutdown with drop cleanup. It also checks repeated shutdown,
rejection of queries after shutdown, and reopening by exact serial while a
previously shut-down Device value remains alive.

A subsequent Seify integration check on the same date passed with firmware
`v0.39`, IIO `0.26`, and Linux `6.1.0-gf3da30df6004`, reporting the same five IIO
devices. Seify's `pluto_hardware` test exercised registry discovery, typed and
dynamic handles, shared clone ownership, idempotent shutdown, and reopening
through the synchronous backend and both native async runtimes (smol and Tokio).
At that stage Seify exposed context metadata only; the RX work described below
adds one RX channel and RF controls.

These results validate pipe setup, PRINT framing/XML parsing, and normal
control-session lifecycle on this device. Streaming pipes 1/2, attribute
reads/writes, unplug/cancellation recovery on hardware, other host platforms,
and browser WebUSB access were not exercised.

## RX and configuration implementation

The RX implementation continues to use the legacy ASCII interpreter on current
firmware. References below are to the pinned libiio revision listed above:

- `iiod-client.c::iiod_client_attr_read` and `iiod_client_attr_write`: READ is
  length + bytes + newline; WRITE sends command and value in separate writes,
  then receives a signed count. Attribute names and IDs must be individual
  protocol tokens, and the value length is a byte count.
- `usb.c::usb_open_buffer` / `usb_close_buffer`: reserve an extra endpoint pair,
  OPEN_PIPE with its logical index in wValue, create an independent interpreter,
  then CLOSE the IIO buffer and CLOSE_PIPE. Never RESET_PIPES when starting RX.
- `iiod-client.c::iiod_client_open_with_mask`, `iiod_client_read_unlocked`,
  `iiod/ops.c::open_dev_helper` and `send_data`: OPEN mask bits refer to the
  server's ordered channel list; sample memory follows scan-element indices.
  READBUF replies contain a mask only with the first positive chunk of each
  request. Chunk lengths exclude the mask. A zero ends a short response; an
  exact-length response has no zero trailer. Unexpected masks are rejected.
- `examples/ad9361-iiostream.c::cfg_ad9361_streaming_ch`: RX PHY input voltage0
  provides sampling_frequency, rf_bandwidth, and rf_port_select; PHY output
  altvoltage0 provides RX LO frequency. gain_control_mode and hardwaregain are
  RX input voltage0 attributes. IDs are resolved from context, not fixed numbers.

Scope: one RX complex channel, signed 16-bit storage with validated scan format,
normalized Complex32 output, configurable noncyclic buffer size, and bounded
pull-based USB reads. RF ranges come from firmware *_available attributes.
No FIR synthesis/loading or low-rate resampling is performed. RX port names
represent AD936x internal selections, not extra exposed Pluto antenna connectors.
Native reads use blocking USB only for .wait(); async reads await nusb directly.
Cancelling an in-flight protocol operation poisons only its session; stop closes
that USB pipe before restart. A stream owns the claimed interface lifetime, and
explicit device shutdown is rejected while an RX stream handle exists.


RX validation on firmware v0.39 / IIO v0.26 (2026-09-14): control readback,
manual gain and all AGC modes, bandwidth and RF port selection, 786,432 samples
across three native RX sessions plus restart/drop checks, and 1,048,576 samples
through async RX with concurrent pipe-0 queries. Cancelling an in-flight RX
request poisoned its session as intended; stop/restart restored sample reads.
Seify's typed controls and dynamic streams also passed synchronous and async
hardware tests. Sample captures varied and remained within the normalized range;
no calibrated tone, timestamp continuity, or maximum-rate qualification was done.

Firmware attribute READ payloads include a NUL byte in their advertised length.
The generic IIOD method preserves it; RX configuration getters trim terminating
NUL/whitespace. Sample-rate readback at a requested 2,500,000 samples/s was
2,499,999 samples/s, consistent with hardware clock rounding. Available values
came from PHY attributes, not hardcoded assumptions about AD9363/AD9364 limits.

The AD936x range/mode mapping also follows
[`ad9361.c`](https://github.com/analogdevicesinc/linux/blob/main/drivers/iio/adc/ad9361.c),
functions `ad9361_phy_read_avail` and `ad9361_phy_lo_read`, and the
`ad9361_phy_ext_info` / RX port and AGC enum tables.

## Queued RX payload transfers (2026-09-14)

The HackRF reference uses `DirectRxStream` / `AsyncDirectRxStream` in
`~/src/hackrf-rs/src/streaming.rs` to keep bulk transfers queued and recycle
completed buffers. Pluto now shares these architectural properties, with a
protocol-specific boundary: a READBUF chunk's positive byte count must be
parsed before its payload reads can be queued.

`Transport::consume_exact` delivers a known-length payload through borrowed
chunks; its generic implementation uses ordinary short reads, while
`NusbTransport` queues up to four 64 KiB reads. `read_exact` is a copying adapter
for callers that need contiguous byte storage.
The sum of outstanding requested bytes stays within the remaining payload.
Short completions and ZLPs free their unused reservation for replacement reads.
Only the final partial USB packet is rounded up, after older reads finish; any
surplus is retained for the next response. Successful completion therefore
leaves no outstanding IN requests. This avoids speculative reads hanging at an
IIOD command boundary, particularly on WebUSB where cancellation is unavailable.

The nusb source reference is `Endpoint::{allocate,submit,next_complete,
wait_next_complete}` in `nusb-0.2.7/src/device.rs`. Its WebUSB `submit` immediately
calls JavaScript `transferIn`, so queueing four transfers creates four actual
browser requests before awaiting the first completion. Completion buffers are
recycled. `IiodClient::read_chunks` passes borrowed completions to `RxStream`,
which decodes into caller output. `read_buffer_into` remains available as a
contiguous-storage adapter.

Cancellation still poisons the IIOD session. Explicit async shutdown drains
already-submitted reads before closing the pipe. Browser Drop holds the endpoint
objects through background cleanup so another stream cannot reuse them while
old browser requests remain outstanding. The FunctionFS close operation stops
the pipe's interpreter (`usb_close_pipe` / `usbd_client_thread` in upstream
`iiod/usbd.c`); it is not a substitute for host-side completion ownership.

Five-second native release benchmarks at a 20 MS/s hardware clock, 2.462 GHz,
50 dB gain, without DSP (same hardware and saved pre-change executable):

| Mode | DMA buffer, complex samples | Before, MS/s | Queued/reused buffers, MS/s |
| --- | ---: | ---: | ---: |
| Async | 65,536 | 4.70 | 6.16 |
| Blocking | 65,536 | 5.13 | 6.40 |
| Async | 262,144 | 5.21 | 6.71 |

The default buffer size remains 65,536 to preserve latency. These results include
sample conversion and do not establish lossless capture at the hardware clock.
Native hardware checks passed cancellation, timeout recovery, stop/restart,
small-output tails, concurrent control and active Drop. Production WebUSB queue
behavior is checked with delayed mock JavaScript USB transfers under Node;
actual browser hardware throughput must be measured separately.

## Direct decoding and bounded read ahead

The next optimization pass removes the intermediate raw payload from full-buffer
RX reads. `SampleSink` converts borrowed USB completions directly into caller
`Complex32` storage. A four-byte carry handles arbitrarily split I/Q frames;
only samples beyond caller capacity are copied into reusable raw tail storage.
The stock `le:S12/16>>0` format has a fixed-shift conversion loop; other supported
formats retain the generic decoder. No unsafe uninitialized vector lengths or
platform-specific SIMD are needed.

`IiodClient::integer` and channel-mask parsing use stack storage, `ReadRequest`
validates and formats the command once per stream, the USB request ledger is a
fixed array, and OUT buffers are recycled. Async payload completion waits share
one deadline timer per chunk instead of allocating a timer per completion.
FutureSDR's one-channel `AsyncSource::work` uses a stack array for output slices.
Seify's dynamic dispatch still boxes a read future, and WebUSB/nusb still creates
browser transfer promises and copies each browser ArrayBuffer into WASM memory.
The stream loop itself adds no mutex or background task.

After consuming a complete response, `RxStream` writes exactly one next READBUF
command before returning. This lets the device prepare a buffer during caller
processing. It does **not** queue host reads across an unparsed header or run a
background receive loop. The next nonzero-timeout refill resumes that response,
with a fresh deadline; cached samples can still be polled with zero timeout.
The serialized client rejects other commands until the response is consumed.
Cancellation while consuming or writing a request poisons the session. Stop
uses FunctionFS CLOSE_PIPE when a response is outstanding, terminating the pipe
interpreter and its buffer, instead of appending a CLOSE behind unread data.
This uses the existing ASCII request/response protocol and `usb_close_pipe` /
`usbd_client_thread` lifecycle described above; it requires no firmware changes.

Paired five-second native release runs on the same hardware, using a saved
executable of the four-transfer implementation as the baseline:

| Mode | Buffer, complex samples | Four-transfer baseline, MS/s | Direct decoding + read ahead, MS/s |
| --- | ---: | ---: | ---: |
| Async | 65,536 | 6.06 | 7.11 |
| Async, repeat | 65,536 | 6.02 | 7.16 |
| Blocking | 65,536 | 6.39 | 7.05 |
| Async | 262,144 | 6.44 | 7.64 |

The default buffer and four-by-64-KiB queue are unchanged. The default async gain
is about 17–19%, reaching 28.4–28.6 MB/s of USB sample payload. These measurements
combine all changes; they do not assign the gain to an individual optimization.
Hardware lifecycle checks cover read-ahead stop/restart, active Drop, small
outputs, concurrent control, cancelled reads and timeout recovery. Fragmentation
tests exercise every split size, and exhaustive 16-bit-word tests compare the
specialized and generic sample conversion. WebUSB tests run the production nusb
endpoint path with delayed JavaScript mocks, checking four recycled buffers,
borrowed payload delivery, callback failure and cancellation cleanup. Browser
hardware throughput remains a separate measurement.
