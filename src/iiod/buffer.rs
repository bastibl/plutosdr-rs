//! Legacy IIOD buffer creation and destruction, independent of USB.
use super::{IiodClient, Transport, attribute::token};
use crate::{Error, Result, maybe_future::dual};
use nusb::MaybeFuture;

impl<T: Transport> IiodClient<T> {
    /// Create a noncyclic IIO buffer on this session. Mask words are MSW first.
    pub fn open_buffer(
        &mut self,
        device: &str,
        samples: usize,
        mask: &str,
    ) -> impl MaybeFuture<Output = Result<()>> + '_ {
        let command = open_command(device, samples, mask);
        dual!(
            (self, command),
            |(this, command): (&mut Self, Result<String>)| { this.status(command?).wait() },
            |(this, command): (&mut Self, Result<String>)| async move { this.status(command?).await }
        )
    }

    /// Close the IIO buffer before releasing its transport pipe.
    pub fn close_buffer(&mut self, device: &str) -> impl MaybeFuture<Output = Result<()>> + '_ {
        let command = token(device).map(|_| format!("CLOSE {device}\r\n"));
        dual!(
            (self, command),
            |(this, command): (&mut Self, Result<String>)| { this.status(command?).wait() },
            |(this, command): (&mut Self, Result<String>)| async move { this.status(command?).await }
        )
    }
}

fn open_command(device: &str, samples: usize, mask: &str) -> Result<String> {
    token(device)?;
    if samples == 0
        || samples > 4 * 1024 * 1024
        || mask.is_empty()
        || mask.len() > 128
        || !mask.len().is_multiple_of(8)
        || !mask.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(Error::InvalidConfig("invalid noncyclic IIO buffer"));
    }
    Ok(format!("OPEN {device} {samples} {mask}\r\n"))
}
