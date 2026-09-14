use super::discovery::{DeviceDescriptor, InterfaceInfo, inspect_device};
use crate::{Error, Result, iiod::Transport, maybe_future::dual};
use nusb::{
    Endpoint, Interface, MaybeFuture,
    transfer::{Buffer, Bulk, ControlOut, ControlType, In, Out, Recipient},
};
use std::{collections::VecDeque, time::Duration};

const PIPE_TIMEOUT: Duration = Duration::from_secs(1);

/// One owned FunctionFS control pipe. No Ethernet/TCP path exists.
pub struct NusbTransport {
    input: Endpoint<Bulk, In>,
    output: Endpoint<Bulk, Out>,
    interface: Interface,
    _device: nusb::Device,
    descriptor: InterfaceInfo,
    timeout: Duration,
    pipe: u16,
    deadline: Option<web_time::Instant>,
    buffered: VecDeque<u8>,
    needs_cleanup: bool,
    closed: bool,
}

impl NusbTransport {
    /// Discover/claim the named IIO interface and open logical pipe zero.
    /// Opening resets all IIOD pipes on that interface, as upstream libiio does.
    pub fn open(
        descriptor: &DeviceDescriptor,
        interface_number: Option<u8>,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<Self>> + '_ {
        dual!(
            (descriptor, interface_number, timeout),
            |(descriptor, number, timeout): (&DeviceDescriptor, Option<u8>, Duration)| {
                validate_timeout(timeout)?;
                let device = descriptor.open().wait()?;
                let info = inspect_device(&device)
                    .wait()?
                    .iio_interface(number)?
                    .clone();
                let interface = device.detach_and_claim_interface(info.number).wait()?;
                interface.set_alt_setting(info.alternate_setting).wait()?;
                let transport = Self::from_claim(device, interface, info, timeout, 0)?;
                transport.pipe_command(0).wait()?;
                transport.pipe_command(1).wait()?;
                Ok(transport)
            },
            |(descriptor, number, timeout): (&DeviceDescriptor, Option<u8>, Duration)| async move {
                validate_timeout(timeout)?;
                let device = descriptor.open().await?;
                let info = inspect_device(&device)
                    .await?
                    .iio_interface(number)?
                    .clone();
                let interface = device.detach_and_claim_interface(info.number).await?;
                interface.set_alt_setting(info.alternate_setting).await?;
                let transport = Self::from_claim(device, interface, info, timeout, 0)?;
                transport.pipe_command(0).await?;
                transport.pipe_command(1).await?;
                Ok(transport)
            }
        )
    }

    fn from_claim(
        device: nusb::Device,
        interface: Interface,
        descriptor: InterfaceInfo,
        timeout: Duration,
        pipe: u16,
    ) -> Result<Self> {
        let pair = *descriptor
            .endpoint_pairs()?
            .get(usize::from(pipe))
            .ok_or(Error::InvalidConfig("missing streaming endpoint pair"))?;
        Ok(Self {
            input: interface.endpoint::<Bulk, In>(pair.in_address)?,
            output: interface.endpoint::<Bulk, Out>(pair.out_address)?,
            interface,
            _device: device,
            descriptor,
            timeout,
            pipe,
            deadline: None,
            buffered: VecDeque::new(),
            needs_cleanup: true,
            closed: false,
        })
    }

    pub fn interface_info(&self) -> &InterfaceInfo {
        &self.descriptor
    }

    fn pipe_command(&self, request: u8) -> impl MaybeFuture<Output = Result<()>> + use<> {
        self.interface
            .control_out(
                pipe_request(request, self.descriptor.number, self.pipe),
                PIPE_TIMEOUT,
            )
            .map_err(Error::from)
    }

    /// Factory for an independent pipe on the already claimed interface.
    pub(crate) fn additional_pipe(&self, pipe: u16) -> Result<PipeFactory> {
        self.check_open()?;
        if pipe == 0 || usize::from(pipe) >= self.descriptor.endpoint_pairs()?.len() {
            return Err(Error::InvalidConfig("missing streaming endpoint pair"));
        }
        Ok(PipeFactory {
            device: self._device.clone(),
            interface: self.interface.clone(),
            descriptor: self.descriptor.clone(),
            timeout: self.timeout,
            pipe,
        })
    }

    pub(crate) fn set_timeout(&mut self, timeout: Duration) -> Result<()> {
        validate_timeout(timeout)?;
        self.timeout = timeout;
        self.deadline = Some(web_time::Instant::now() + timeout);
        Ok(())
    }

    fn remaining_timeout(&self) -> Result<Duration> {
        match self.deadline {
            Some(deadline) => deadline
                .checked_duration_since(web_time::Instant::now())
                .filter(|d| !d.is_zero())
                .ok_or(Error::Timeout),
            None => Ok(self.timeout),
        }
    }

    pub(crate) fn open_pipe(&mut self) -> impl MaybeFuture<Output = Result<()>> + use<> {
        self.pipe_command(1)
    }

    fn check_open(&self) -> Result<()> {
        if self.closed {
            Err(Error::DeviceClosed)
        } else {
            Ok(())
        }
    }

    fn take_buffered(&mut self, max: usize) -> Option<Vec<u8>> {
        if self.buffered.is_empty() {
            None
        } else {
            Some(
                self.buffered
                    .drain(..max.min(self.buffered.len()))
                    .collect(),
            )
        }
    }
    fn accept_read(
        &mut self,
        completion: nusb::transfer::Completion,
        max: usize,
    ) -> Result<Option<Vec<u8>>> {
        completion.status?;
        self.buffered
            .extend(completion.buffer[..completion.actual_len].iter().copied());
        Ok(self.take_buffered(max))
    }
    fn cancel_pending(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.input.cancel_all();
            self.output.cancel_all();
        }
    }
}

pub(crate) struct PipeFactory {
    device: nusb::Device,
    interface: Interface,
    descriptor: InterfaceInfo,
    timeout: Duration,
    pipe: u16,
}

impl PipeFactory {
    pub(crate) fn timeout(&self) -> Duration {
        self.timeout
    }
    pub(crate) fn create(&self) -> Result<NusbTransport> {
        NusbTransport::from_claim(
            self.device.clone(),
            self.interface.clone(),
            self.descriptor.clone(),
            self.timeout,
            self.pipe,
        )
    }
}

fn pipe_request(request: u8, interface: u8, pipe: u16) -> ControlOut<'static> {
    ControlOut {
        control_type: ControlType::Vendor,
        recipient: Recipient::Interface,
        request,
        value: pipe,
        index: u16::from(interface),
        data: &[],
    }
}

fn validate_timeout(timeout: Duration) -> Result<()> {
    if timeout.is_zero() || timeout > Duration::from_secs(3600) {
        return Err(Error::InvalidConfig(
            "timeout must be greater than zero and at most one hour",
        ));
    }
    Ok(())
}

async fn timed<F: std::future::Future<Output = nusb::transfer::Completion>>(
    future: F,
    timeout: Duration,
) -> Result<nusb::transfer::Completion> {
    futures_lite::future::race(async { Ok(future.await) }, async {
        futures_timer::Delay::new(timeout).await;
        Err(Error::Timeout)
    })
    .await
}

impl Transport for NusbTransport {
    fn write_command(&mut self, command: &[u8]) -> impl MaybeFuture<Output = Result<()>> {
        dual!(
            (self, command),
            |(this, command): (&mut Self, &[u8])| {
                validate_command(command)?;
                this.write_data(command).wait()
            },
            |(this, command): (&mut Self, &[u8])| async move {
                validate_command(command)?;
                this.write_data(command).await
            }
        )
    }
    fn write_data(&mut self, command: &[u8]) -> impl MaybeFuture<Output = Result<()>> {
        dual!(
            (self, command),
            |(this, command): (&mut Self, &[u8])| {
                this.check_open()?;
                let timeout = this.remaining_timeout()?;
                let completion = this
                    .output
                    .transfer_blocking(command.to_vec().into(), timeout);
                check_blocking_timeout(&completion)?;
                check_write(completion, command.len())
            },
            |(this, command): (&mut Self, &[u8])| async move {
                this.check_open()?;
                let timeout = this.remaining_timeout()?;
                this.output.submit(command.to_vec().into());
                let result = timed(this.output.next_complete(), timeout).await;
                if result.is_err() {
                    this.cancel_pending();
                }
                check_write(result?, command.len())
            }
        )
    }

    fn read(&mut self, max_bytes: usize) -> impl MaybeFuture<Output = Result<Vec<u8>>> {
        dual!(
            (self, max_bytes),
            |(this, max): (&mut Self, usize)| {
                this.check_open()?;
                if max == 0 {
                    return Err(Error::InvalidConfig("zero read size"));
                }
                if let Some(bytes) = this.take_buffered(max) {
                    return Ok(bytes);
                }
                for _ in 0..16 {
                    let timeout = this.remaining_timeout()?;
                    let size = read_size(max, this.input.max_packet_size());
                    let completion = this.input.transfer_blocking(Buffer::new(size), timeout);
                    check_blocking_timeout(&completion)?;
                    if let Some(bytes) = this.accept_read(completion, max)? {
                        return Ok(bytes);
                    }
                }
                Err(Error::Protocol("too many empty USB completions"))
            },
            |(this, max): (&mut Self, usize)| async move {
                this.check_open()?;
                if max == 0 {
                    return Err(Error::InvalidConfig("zero read size"));
                }
                if let Some(bytes) = this.take_buffered(max) {
                    return Ok(bytes);
                }
                for _ in 0..16 {
                    let timeout = this.remaining_timeout()?;
                    let size = read_size(max, this.input.max_packet_size());
                    this.input.submit(Buffer::new(size));
                    let result = timed(this.input.next_complete(), timeout).await;
                    if result.is_err() {
                        this.cancel_pending();
                    }
                    if let Some(bytes) = this.accept_read(result?, max)? {
                        return Ok(bytes);
                    }
                }
                Err(Error::Protocol("too many empty USB completions"))
            }
        )
    }

    fn shutdown(&mut self) -> impl MaybeFuture<Output = Result<()>> {
        dual!(
            self,
            |this: &mut Self| {
                this.closed = true;
                if !this.needs_cleanup {
                    return Ok(());
                }
                this.cancel_pending();
                this.pipe_command(2).wait()?;
                this.needs_cleanup = false;
                Ok(())
            },
            |this: &mut Self| async move {
                this.closed = true;
                if !this.needs_cleanup {
                    return Ok(());
                }
                this.cancel_pending();
                this.pipe_command(2).await?;
                this.needs_cleanup = false;
                Ok(())
            }
        )
    }
}

fn validate_command(command: &[u8]) -> Result<()> {
    if command.is_empty()
        || command.len() > 1024
        || !command.ends_with(b"\r\n")
        || command[..command.len() - 2]
            .iter()
            .any(|b| !b.is_ascii() || *b == b'\n' || *b == b'\r')
    {
        return Err(Error::Protocol(
            "expected one short ASCII command ending CRLF",
        ));
    }
    Ok(())
}
fn check_write(completion: nusb::transfer::Completion, length: usize) -> Result<()> {
    completion.status?;
    if completion.actual_len != length {
        return Err(Error::Protocol("short command write"));
    }
    Ok(())
}
fn read_size(max: usize, packet: usize) -> usize {
    max.min(256 * 1024).div_ceil(packet) * packet
}

#[cfg(not(target_arch = "wasm32"))]
fn check_blocking_timeout(completion: &nusb::transfer::Completion) -> Result<()> {
    if completion.status == Err(nusb::transfer::TransferError::Cancelled) {
        Err(Error::Timeout)
    } else {
        Ok(())
    }
}

impl Drop for NusbTransport {
    fn drop(&mut self) {
        self.cancel_pending();
        if !self.needs_cleanup {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = self.pipe_command(2).wait();
        }
        #[cfg(target_arch = "wasm32")]
        {
            let interface = self.interface.clone();
            let number = self.descriptor.number;
            let pipe = self.pipe;
            wasm_bindgen_futures::spawn_local(async move {
                let _ = interface
                    .control_out(pipe_request(2, number, pipe), PIPE_TIMEOUT)
                    .await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn functionfs_commands_use_interface_recipient_and_logical_pipe_zero() {
        for command in [0, 1, 2] {
            let request = pipe_request(command, 7, 0);
            assert_eq!(request.control_type, ControlType::Vendor);
            assert_eq!(request.recipient, Recipient::Interface);
            assert_eq!(request.request, command);
            assert_eq!(request.index, 7);
            assert_eq!(request.value, 0);
            assert!(request.data.is_empty());
        }
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn commands_cannot_merge_and_usb_reads_are_packet_aligned() {
        assert!(validate_command(b"PRINT\r\n").is_ok());
        assert!(validate_command(b"PRINT\r\nPRINT\r\n").is_err());
        assert!(validate_command(b"PRINT").is_err());
        assert!(validate_command(b"").is_err());
        assert!(validate_command(&vec![b'a'; 1025]).is_err());
        for packet in [64, 512, 1024] {
            for max in [1, packet - 1, packet, packet + 1, 4096, usize::MAX] {
                let size = read_size(max, packet);
                assert!(size.is_multiple_of(packet));
                assert!(size >= max.min(256 * 1024));
                assert!(size <= 256 * 1024);
            }
        }
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn short_command_writes_are_errors() {
        let completion = nusb::transfer::Completion {
            buffer: b"PRINT\r\n".to_vec().into(),
            actual_len: 3,
            status: Ok(()),
        };
        assert!(matches!(
            check_write(completion, 7),
            Err(Error::Protocol("short command write"))
        ));
    }

    async fn timeout_expires() {
        assert!(matches!(
            timed(std::future::pending(), Duration::from_millis(1)).await,
            Err(Error::Timeout)
        ));
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn async_deadline_does_not_require_a_usb_runtime() {
        futures_lite::future::block_on(timeout_expires());
    }
    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn wasm_deadline_uses_javascript_timer() {
        timeout_expires().await;
    }
}
