//! Native IIO-over-USB PlutoSDR discovery, configuration, and RX streaming.
//!
//! Operations use [`MaybeFuture`]: `.wait()` on native, `.await` on native or
//! WebUSB. Enable `smol` or `tokio` for native asynchronous USB opening.
//! RX controls and owned [`RxStream`] handles share the same wait/await API.
//! TX streaming is not implemented.
//!
//! ```no_run
//! use plutosdr::{Device, MaybeFuture};
//! # fn main() -> plutosdr::Result<()> {
//! let mut device = Device::open().wait()?;
//! for iio in &device.info().devices {
//!     println!("{} {:?}", iio.id, iio.name);
//! }
//! device.shutdown().wait()?;
//! # Ok(())
//! # }
//! ```

mod baseband;
mod config;
mod device;
mod error;
pub mod iiod;
mod maybe_future;
mod rx;
pub mod usb;

pub use config::{GainMode, RxAttribute, ValueRange};
pub use device::{Device, DeviceBuilder};
pub use error::{Error, ErrorKind, Result};
pub use num_complex::Complex32;
pub use nusb::MaybeFuture;
pub use rx::{DEFAULT_RX_BUFFER_SAMPLES, RxStream};
pub use usb::discovery::DeviceDescriptor;
