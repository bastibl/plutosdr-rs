//! USB enumeration, descriptor inspection, and one native FunctionFS pipe.
pub mod discovery;
pub(crate) mod transport;
pub use transport::NusbTransport;
mod read_queue;
