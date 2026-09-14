//! IIOD wire protocol, independent of the USB backend.
mod buffer;
mod context;
mod protocol;
pub use context::{
    Attribute, BufferInfo, Channel, ChannelDirection, Context, IioDevice, ScanElement,
};
pub use protocol::{IiodClient, MAX_XML_BYTES, Transport};

pub(crate) mod attribute;
pub use attribute::AttributeTarget;
