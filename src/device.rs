use crate::{
    DeviceDescriptor, Error, Result,
    iiod::{Context, IiodClient},
    maybe_future::dual,
    usb::{
        NusbTransport,
        discovery::{self, InterfaceInfo},
    },
};
use nusb::MaybeFuture;
use std::time::Duration;

/// Pluto USB device with a cached IIO context and an owned control session.
pub struct Device {
    descriptor: DeviceDescriptor,
    interface: InterfaceInfo,
    info: Context,
    client: Option<IiodClient<NusbTransport>>,
}

impl Device {
    pub fn list() -> impl MaybeFuture<Output = Result<Vec<DeviceDescriptor>>> {
        discovery::list()
    }
    pub fn builder() -> DeviceBuilder {
        DeviceBuilder::default()
    }
    pub fn open() -> impl MaybeFuture<Output = Result<Self>> {
        Self::builder().open()
    }
    /// Select an exact USB serial string, preserving leading zeroes.
    pub fn open_serial(serial: impl Into<String>) -> impl MaybeFuture<Output = Result<Self>> {
        Self::builder().serial(serial).open()
    }
    /// Cached context, including discovered devices, channels and attribute metadata.
    pub fn info(&self) -> &Context {
        &self.info
    }
    pub fn descriptor(&self) -> &DeviceDescriptor {
        &self.descriptor
    }
    pub fn interface_info(&self) -> &InterfaceInfo {
        &self.interface
    }
    /// Repeat PRINT on the same session, replacing the cache only on success.
    pub fn refresh_info(&mut self) -> impl MaybeFuture<Output = Result<()>> + '_ {
        dual!(
            self,
            |this: &mut Self| {
                this.info = this
                    .client
                    .as_mut()
                    .ok_or(Error::DeviceClosed)?
                    .context()
                    .wait()?;
                Ok(())
            },
            |this: &mut Self| async move {
                this.info = this
                    .client
                    .as_mut()
                    .ok_or(Error::DeviceClosed)?
                    .context()
                    .await?;
                Ok(())
            }
        )
    }
    /// Close the owned IIOD pipe. Lazy, terminal once started, and retryable.
    pub fn shutdown(&mut self) -> impl MaybeFuture<Output = Result<()>> + '_ {
        dual!(
            self,
            |this: &mut Self| {
                if let Some(client) = &mut this.client {
                    client.shutdown().wait()?;
                }
                this.client = None;
                Ok(())
            },
            |this: &mut Self| async move {
                if let Some(client) = &mut this.client {
                    client.shutdown().await?;
                }
                this.client = None;
                Ok(())
            }
        )
    }

    /// Request browser permission from a user gesture before calling open.
    #[cfg(target_arch = "wasm32")]
    pub async fn request_permission() -> Result<()> {
        discovery::request_device()
            .await?
            .ok_or(Error::DeviceNotFound)
            .map(|_| ())
    }
}

/// USB selection and timeout settings; radio configuration is a later milestone.
#[derive(Debug, Clone)]
pub struct DeviceBuilder {
    serial: Option<String>,
    descriptor: Option<DeviceDescriptor>,
    interface: Option<u8>,
    timeout: Duration,
}
impl Default for DeviceBuilder {
    fn default() -> Self {
        Self {
            serial: None,
            descriptor: None,
            interface: None,
            timeout: Duration::from_secs(3),
        }
    }
}
impl DeviceBuilder {
    pub fn serial(mut self, serial: impl Into<String>) -> Self {
        self.serial = Some(serial.into());
        self
    }
    /// Explicit nusb selection also supports custom firmware VID/PID values.
    pub fn descriptor(mut self, descriptor: DeviceDescriptor) -> Self {
        self.descriptor = Some(descriptor);
        self
    }
    /// Disambiguate named IIO interfaces without bypassing name/layout checks.
    pub fn interface(mut self, number: u8) -> Self {
        self.interface = Some(number);
        self
    }
    /// Per-bulk-transfer timeout, shared by blocking and asynchronous operations.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    pub fn open(self) -> impl MaybeFuture<Output = Result<Device>> {
        dual!(
            self,
            |this: Self| {
                let descriptor = match this.descriptor.clone() {
                    Some(d) => this.select(vec![d])?,
                    None => this.select(Device::list().wait()?)?,
                };
                let transport =
                    NusbTransport::open(&descriptor, this.interface, this.timeout).wait()?;
                let interface = transport.interface_info().clone();
                let mut client = IiodClient::new(transport);
                let info = client.context().wait()?;
                Ok(Device {
                    descriptor,
                    interface,
                    info,
                    client: Some(client),
                })
            },
            |this: Self| async move {
                let descriptor = match this.descriptor.clone() {
                    Some(d) => this.select(vec![d])?,
                    None => this.select(Device::list().await?)?,
                };
                let transport =
                    NusbTransport::open(&descriptor, this.interface, this.timeout).await?;
                let interface = transport.interface_info().clone();
                let mut client = IiodClient::new(transport);
                let info = client.context().await?;
                Ok(Device {
                    descriptor,
                    interface,
                    info,
                    client: Some(client),
                })
            }
        )
    }
    fn select(&self, devices: Vec<DeviceDescriptor>) -> Result<DeviceDescriptor> {
        devices
            .into_iter()
            .find(|d| {
                self.serial
                    .as_ref()
                    .is_none_or(|s| d.serial.as_ref() == Some(s))
            })
            .ok_or(Error::DeviceNotFound)
    }
}
