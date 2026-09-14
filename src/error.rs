use std::fmt;

/// Driver result, preserving USB and remote IIOD failures separately.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors from discovery, transport, and protocol processing.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    Busy,
    StreamInactive,
    DeviceNotFound,
    IioInterfaceNotFound,
    AmbiguousIioInterface,
    DeviceClosed,
    SessionPoisoned,
    InvalidConfig(&'static str),
    Descriptor(String),
    Usb(nusb::Error),
    Transfer(nusb::transfer::TransferError),
    Timeout,
    Protocol(&'static str),
    /// Signed Linux error number returned by IIOD, independent of the host OS.
    Remote(i32),
    Xml(String),
}

/// Stable broad category for callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    NotFound,
    Closed,
    Busy,
    Disconnected,
    Timeout,
    Usb,
    Protocol,
    Remote,
    InvalidConfig,
}

impl Error {
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::Busy => ErrorKind::Busy,
            Self::StreamInactive => ErrorKind::Closed,
            Self::DeviceNotFound | Self::IioInterfaceNotFound => ErrorKind::NotFound,
            Self::DeviceClosed | Self::SessionPoisoned => ErrorKind::Closed,
            Self::Timeout => ErrorKind::Timeout,
            Self::Usb(e) => match e.kind() {
                nusb::ErrorKind::Disconnected => ErrorKind::Disconnected,
                nusb::ErrorKind::Busy => ErrorKind::Busy,
                _ => ErrorKind::Usb,
            },
            Self::Transfer(nusb::transfer::TransferError::Disconnected) => ErrorKind::Disconnected,
            Self::Transfer(nusb::transfer::TransferError::Cancelled) => ErrorKind::Usb,
            Self::Transfer(_) => ErrorKind::Usb,
            Self::Remote(_) => ErrorKind::Remote,
            Self::InvalidConfig(_) => ErrorKind::InvalidConfig,
            _ => ErrorKind::Protocol,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("an RX stream handle already owns the device"),
            Self::StreamInactive => f.write_str("RX stream is inactive"),
            Self::DeviceNotFound => f.write_str("no matching PlutoSDR USB device found"),
            Self::IioInterfaceNotFound => {
                f.write_str("no named IIO interface in the active USB configuration")
            }
            Self::AmbiguousIioInterface => {
                f.write_str("multiple IIO interfaces; select one explicitly")
            }
            Self::DeviceClosed => f.write_str("device is shut down"),
            Self::SessionPoisoned => {
                f.write_str("IIOD session interrupted or malformed; shut down and reopen")
            }
            Self::InvalidConfig(s) => write!(f, "invalid configuration: {s}"),
            Self::Descriptor(s) => write!(f, "USB descriptor error: {s}"),
            Self::Usb(e) => write!(f, "USB: {e}"),
            Self::Transfer(e) => write!(f, "USB transfer: {e}"),
            Self::Timeout => f.write_str("USB transfer timed out"),
            Self::Protocol(s) => write!(f, "IIOD protocol: {s}"),
            Self::Remote(n) => write!(f, "IIOD returned Linux error {n}"),
            Self::Xml(s) => write!(f, "IIO context XML: {s}"),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Usb(e) => Some(e),
            Self::Transfer(e) => Some(e),
            _ => None,
        }
    }
}
impl From<nusb::Error> for Error {
    fn from(e: nusb::Error) -> Self {
        Self::Usb(e)
    }
}
impl From<nusb::transfer::TransferError> for Error {
    fn from(e: nusb::transfer::TransferError) -> Self {
        Self::Transfer(e)
    }
}
