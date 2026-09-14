//! Noncyclic RX buffers on an independent FunctionFS pipe.
use crate::{
    Complex32, Device, Error, Result,
    iiod::{ChannelDirection, Context, IiodClient},
    maybe_future::dual,
    usb::{NusbTransport, transport::PipeFactory},
};
use nusb::MaybeFuture;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub const DEFAULT_RX_BUFFER_SAMPLES: usize = 65536;
const MAX_RX_BUFFER_SAMPLES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug)]
struct SampleFormat {
    big_endian: bool,
    bits: u32,
    shift: u32,
}
impl SampleFormat {
    fn parse(text: &str) -> Result<Self> {
        let err = || Error::InvalidConfig("RX requires signed 16-bit scan storage");
        let (endian, rest) = text.split_once(':').ok_or_else(err)?;
        let big_endian = match endian {
            "le" => false,
            "be" => true,
            _ => return Err(err()),
        };
        let rest = rest
            .strip_prefix('s')
            .or_else(|| rest.strip_prefix('S'))
            .ok_or_else(err)?;
        let (bits, rest) = rest.split_once('/').ok_or_else(err)?;
        let (storage, shift) = rest.split_once(">>").ok_or_else(err)?;
        let bits = bits.parse::<u32>().map_err(|_| err())?;
        let shift = shift.parse::<u32>().map_err(|_| err())?;
        if storage != "16" || bits == 0 || bits > 16 || shift > 16 - bits {
            return Err(err());
        }
        Ok(Self {
            big_endian,
            bits,
            shift,
        })
    }
    fn decode(&self, bytes: &[u8]) -> f32 {
        let word = if self.big_endian {
            u16::from_be_bytes([bytes[0], bytes[1]])
        } else {
            u16::from_le_bytes([bytes[0], bytes[1]])
        };
        let value = (u32::from(word) >> self.shift) & ((1 << self.bits) - 1);
        let signed = ((value << (32 - self.bits)) as i32) >> (32 - self.bits);
        signed as f32 / (1u32 << (self.bits - 1)) as f32
    }
}

#[derive(Debug)]
struct RxLayout {
    device: String,
    mask: String,
    formats: [SampleFormat; 2],
    offsets: [usize; 2],
}
impl RxLayout {
    fn from_context(context: &Context) -> Result<Self> {
        let device = context
            .devices
            .iter()
            .find(|d| d.name.as_deref() == Some("cf-ad9361-lpc"))
            .ok_or(Error::InvalidConfig("missing cf-ad9361-lpc RX device"))?;
        crate::iiod::attribute::token(&device.id)?;
        if device.channels.is_empty() || device.channels.len() > 512 {
            return Err(Error::InvalidConfig("unsupported RX channel count"));
        }
        let mut mask = vec![0u32; device.channels.len().div_ceil(32)];
        let mut formats = Vec::new();
        let mut indices = Vec::new();
        for name in ["voltage0", "voltage1"] {
            let (number, channel) = device
                .channels
                .iter()
                .enumerate()
                .find(|(_, c)| c.id == name && c.direction == ChannelDirection::Input)
                .ok_or(Error::InvalidConfig("missing RX I/Q channels"))?;
            let scan = channel
                .scan_element
                .as_ref()
                .ok_or(Error::InvalidConfig("missing RX scan layout"))?;
            if scan.index < 0 {
                return Err(Error::InvalidConfig("invalid RX scan index"));
            }
            mask[number / 32] |= 1 << (number % 32);
            formats.push(SampleFormat::parse(&scan.format)?);
            indices.push(scan.index);
        }
        if indices[0] == indices[1] {
            return Err(Error::InvalidConfig("duplicate RX scan index"));
        }
        let offsets = if indices[0] < indices[1] {
            [0, 2]
        } else {
            [2, 0]
        };
        Ok(Self {
            device: device.id.clone(),
            mask: mask
                .iter()
                .rev()
                .map(|word| format!("{word:08x}"))
                .collect(),
            formats: formats.try_into().unwrap(),
            offsets,
        })
    }
    fn convert(&self, bytes: &[u8], output: &mut [Complex32]) -> Result<usize> {
        if !bytes.len().is_multiple_of(4) || bytes.len() / 4 > output.len() {
            return Err(Error::Protocol("incomplete RX scan frame"));
        }
        for (frame, output) in bytes.as_chunks::<4>().0.iter().zip(output) {
            *output = Complex32::new(
                self.formats[0].decode(&frame[self.offsets[0]..]),
                self.formats[1].decode(&frame[self.offsets[1]..]),
            );
        }
        Ok(bytes.len() / 4)
    }
}

/// Owned receiver. Call start, read, then stop using wait or await.
/// The stream can outlive Device. Drop closes its pipe; only one handle is allowed.
/// Cancellation/transport failure requires stop before restarting.
pub struct RxStream {
    factory: PipeFactory,
    client: Option<IiodClient<NusbTransport>>,
    layout: RxLayout,
    claim: Arc<AtomicBool>,
    samples: usize,
    active: bool,
    timeout: Duration,
    pending: Vec<Complex32>,
    pending_offset: usize,
}
impl Device {
    pub fn rx_stream(&mut self) -> Result<RxStream> {
        self.rx_stream_with_buffer(DEFAULT_RX_BUFFER_SAMPLES)
    }
    /// Reserve one stream handle, without starting DMA. Samples count complex frames.
    pub fn rx_stream_with_buffer(&mut self, samples: usize) -> Result<RxStream> {
        if samples == 0 || samples > MAX_RX_BUFFER_SAMPLES {
            return Err(Error::InvalidConfig(
                "RX buffer must contain 1..=4194304 samples",
            ));
        }
        let layout = RxLayout::from_context(self.info())?;
        let factory = self.client_mut()?.transport_mut().additional_pipe(1)?;
        let timeout = factory.timeout();
        self.rx_claim
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::Busy)?;
        Ok(RxStream {
            factory,
            client: None,
            layout,
            claim: self.rx_claim.clone(),
            samples,
            active: false,
            timeout,
            pending: Vec::new(),
            pending_offset: 0,
        })
    }
}
impl RxStream {
    fn deliver(&mut self, output: &mut [Complex32]) -> usize {
        let n = output.len().min(self.pending.len() - self.pending_offset);
        output[..n].copy_from_slice(&self.pending[self.pending_offset..self.pending_offset + n]);
        self.pending_offset += n;
        n
    }
    pub fn mtu(&self) -> usize {
        self.samples
    }
    pub fn start(&mut self) -> impl MaybeFuture<Output = Result<()>> + '_ {
        dual!(
            self,
            |this: &mut Self| {
                if this.active {
                    return if this.client.as_ref().is_some_and(IiodClient::usable) {
                        Ok(())
                    } else {
                        Err(Error::SessionPoisoned)
                    };
                }
                if this.client.is_some() {
                    return Err(Error::SessionPoisoned);
                }
                this.client = Some(IiodClient::new(this.factory.create()?));
                this.client
                    .as_mut()
                    .unwrap()
                    .transport_mut()
                    .open_pipe()
                    .wait()?;
                this.client
                    .as_mut()
                    .unwrap()
                    .open_buffer(&this.layout.device, this.samples, &this.layout.mask)
                    .wait()?;
                this.active = true;
                Ok(())
            },
            |this: &mut Self| async move {
                if this.active {
                    return if this.client.as_ref().is_some_and(IiodClient::usable) {
                        Ok(())
                    } else {
                        Err(Error::SessionPoisoned)
                    };
                }
                if this.client.is_some() {
                    return Err(Error::SessionPoisoned);
                }
                this.client = Some(IiodClient::new(this.factory.create()?));
                this.client
                    .as_mut()
                    .unwrap()
                    .transport_mut()
                    .open_pipe()
                    .await?;
                this.client
                    .as_mut()
                    .unwrap()
                    .open_buffer(&this.layout.device, this.samples, &this.layout.mask)
                    .await?;
                this.active = true;
                Ok(())
            }
        )
    }
    /// Stop and release the USB pipe. The handle stays reserved and may restart.
    pub fn stop(&mut self) -> impl MaybeFuture<Output = Result<()>> + '_ {
        dual!(
            self,
            |this: &mut Self| {
                let active = std::mem::replace(&mut this.active, false);
                this.pending.clear();
                this.pending_offset = 0;
                if let Some(client) = this.client.as_mut() {
                    client.transport_mut().set_timeout(this.timeout)?;
                    let close = if active && client.usable() {
                        client.close_buffer(&this.layout.device).wait()
                    } else {
                        Ok(())
                    };
                    client.shutdown().wait()?;
                    this.client = None;
                    close?;
                }
                Ok(())
            },
            |this: &mut Self| async move {
                let active = std::mem::replace(&mut this.active, false);
                this.pending.clear();
                this.pending_offset = 0;
                if let Some(client) = this.client.as_mut() {
                    client.transport_mut().set_timeout(this.timeout)?;
                    let close = if active && client.usable() {
                        client.close_buffer(&this.layout.device).await
                    } else {
                        Ok(())
                    };
                    client.shutdown().await?;
                    this.client = None;
                    close?;
                }
                Ok(())
            }
        )
    }
    /// Read up to one buffer of normalized I/Q. Timeout bounds the complete READBUF exchange;
    /// None uses the device default. Zero timeout returns immediately without I/O.
    pub fn read<'a>(
        &'a mut self,
        output: &'a mut [Complex32],
        timeout: Option<Duration>,
    ) -> impl MaybeFuture<Output = Result<usize>> + 'a {
        dual!(
            (self, output, timeout),
            |(this, output, timeout): (&mut Self, &mut [Complex32], Option<Duration>)| {
                if !this.active {
                    return Err(Error::StreamInactive);
                }
                if output.is_empty() {
                    return Ok(0);
                }
                if this.pending_offset < this.pending.len() {
                    return Ok(this.deliver(output));
                }
                if timeout.is_some_and(|t| t.is_zero()) {
                    return Err(Error::Timeout);
                }
                let client = this.client.as_mut().ok_or(Error::StreamInactive)?;
                client
                    .transport_mut()
                    .set_timeout(timeout.unwrap_or(this.timeout))?;
                let bytes = client
                    .read_buffer(&this.layout.device, this.samples * 4, &this.layout.mask)
                    .wait()?;
                this.pending.resize(bytes.len() / 4, Complex32::default());
                this.layout.convert(&bytes, &mut this.pending)?;
                this.pending_offset = 0;
                Ok(this.deliver(output))
            },
            |(this, output, timeout): (&mut Self, &mut [Complex32], Option<Duration>)| async move {
                if !this.active {
                    return Err(Error::StreamInactive);
                }
                if output.is_empty() {
                    return Ok(0);
                }
                if this.pending_offset < this.pending.len() {
                    return Ok(this.deliver(output));
                }
                if timeout.is_some_and(|t| t.is_zero()) {
                    return Err(Error::Timeout);
                }
                let client = this.client.as_mut().ok_or(Error::StreamInactive)?;
                client
                    .transport_mut()
                    .set_timeout(timeout.unwrap_or(this.timeout))?;
                let bytes = client
                    .read_buffer(&this.layout.device, this.samples * 4, &this.layout.mask)
                    .await?;
                this.pending.resize(bytes.len() / 4, Complex32::default());
                this.layout.convert(&bytes, &mut this.pending)?;
                this.pending_offset = 0;
                Ok(this.deliver(output))
            }
        )
    }
}
impl Drop for RxStream {
    fn drop(&mut self) {
        // Keep the reservation until WebUSB cleanup completes, so a deferred
        // CLOSE_PIPE cannot accidentally close a replacement stream.
        #[cfg(not(target_arch = "wasm32"))]
        {
            drop(self.client.take());
            self.claim.store(false, Ordering::Release);
        }
        #[cfg(target_arch = "wasm32")]
        if let Some(mut client) = self.client.take() {
            let claim = self.claim.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if client.shutdown().await.is_ok() {
                    drop(client);
                    claim.store(false, Ordering::Release);
                }
                // On failure retain the reservation; reopening the device is needed.
            });
        } else {
            self.claim.store(false, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn signed_scan_conversion_and_reordered_iq() {
        for (format, negative, positive) in [
            ("le:S12/16>>0", [0x00, 0x08], [0xff, 0x07]),
            ("be:s12/16>>4", [0x80, 0x00], [0x7f, 0xf0]),
            ("le:S16/16>>0", [0x00, 0x80], [0xff, 0x7f]),
        ] {
            let fmt = SampleFormat::parse(format).unwrap();
            assert_eq!(fmt.decode(&negative), -1.0);
            assert_eq!(
                fmt.decode(&positive),
                1.0 - 1.0 / (1u32 << (fmt.bits - 1)) as f32
            );
        }
        for invalid in [
            "le:U12/16>>0",
            "le:S0/16>>0",
            "le:S17/16>>0",
            "le:S12/16X2>>0",
            "le:S12/16>>5",
            "le:S12/32>>0",
        ] {
            assert!(SampleFormat::parse(invalid).is_err());
        }
        let mut context = Context::from_xml(include_str!("../tests/fixtures/context.xml")).unwrap();
        let layout = RxLayout::from_context(&context).unwrap();
        assert_eq!(layout.mask, "00000003");
        assert_eq!(layout.device, "iio:device7");
        let mut out = [Complex32::default()];
        layout.convert(&[0, 8, 0xff, 7], &mut out).unwrap();
        assert_eq!(out[0], Complex32::new(-1.0, 2047.0 / 2048.0));
        assert!(layout.convert(&[0, 8, 0xff], &mut out).is_err());
        context.devices[1].channels[0]
            .scan_element
            .as_mut()
            .unwrap()
            .index = 2;
        let layout = RxLayout::from_context(&context).unwrap();
        layout.convert(&[0, 8, 0xff, 7], &mut out).unwrap();
        assert_eq!(out[0], Complex32::new(2047.0 / 2048.0, -1.0));
    }
}
