//! Native IIO-over-USB PlutoSDR discovery and context inspection.
//!
//! Operations use [`MaybeFuture`]: `.wait()` on native, `.await` on native or
//! WebUSB. Enable `smol` or `tokio` for native asynchronous USB opening.
//! This first milestone does not configure the radio or stream IQ samples.
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

mod device;
mod error;
pub mod iiod;
mod maybe_future;
pub mod usb;

pub use device::{Device, DeviceBuilder};
pub use error::{Error, ErrorKind, Result};
pub use nusb::MaybeFuture;
pub use usb::discovery::DeviceDescriptor;
