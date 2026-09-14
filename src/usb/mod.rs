//! USB enumeration, descriptor inspection, and one native FunctionFS pipe.
pub mod discovery;
mod transport;
pub use transport::NusbTransport;
