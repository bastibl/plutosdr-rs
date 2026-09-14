use crate::{Error, Result, maybe_future::dual};
use nusb::MaybeFuture;
use std::{num::NonZeroU8, time::Duration};

pub const PLUTO_VID: u16 = 0x0456;
pub const PLUTO_PID: u16 = 0xb673;
const DESCRIPTOR_TIMEOUT: Duration = Duration::from_secs(1);

/// Device information available without claiming an interface.
#[derive(Debug, Clone)]
pub struct DeviceDescriptor {
    pub vid: u16,
    pub pid: u16,
    pub serial: Option<String>,
    pub product_string: Option<String>,
    pub manufacturer_string: Option<String>,
    info: nusb::DeviceInfo,
}

impl DeviceDescriptor {
    /// Wrap an explicitly selected nusb device, including custom Pluto USB IDs.
    pub fn from_nusb(info: nusb::DeviceInfo) -> Self {
        Self {
            vid: info.vendor_id(),
            pid: info.product_id(),
            serial: info.serial_number().map(str::to_owned),
            product_string: info.product_string().map(str::to_owned),
            manufacturer_string: info.manufacturer_string().map(str::to_owned),
            info,
        }
    }
    pub fn is_likely_pluto(&self) -> bool {
        self.vid == PLUTO_VID && self.pid == PLUTO_PID
    }
    pub(crate) fn open(&self) -> impl MaybeFuture<Output = Result<nusb::Device>> + use<> {
        self.info.open().map_err(Error::from)
    }
    /// Open only to read descriptors; never claim interfaces or open IIOD pipes.
    pub fn inspect(&self) -> impl MaybeFuture<Output = Result<UsbInspection>> + '_ {
        dual!(
            self,
            |this: &Self| { inspect_device(&this.open().wait()?).wait() },
            |this: &Self| async move { inspect_device(&this.open().await?).await }
        )
    }
}

pub fn list() -> impl MaybeFuture<Output = Result<Vec<DeviceDescriptor>>> {
    list_all().map_ok(|devices| {
        devices
            .into_iter()
            .filter(DeviceDescriptor::is_likely_pluto)
            .collect()
    })
}
/// Enumerate all USB identities for diagnostics/custom firmware selection.
pub fn list_all() -> impl MaybeFuture<Output = Result<Vec<DeviceDescriptor>>> {
    nusb::list_devices().map(|r| Ok(r?.map(DeviceDescriptor::from_nusb).collect()))
}

/// Browser chooser; call from a user gesture in a secure browser window.
#[cfg(target_arch = "wasm32")]
pub async fn request_device() -> Result<Option<DeviceDescriptor>> {
    Ok(
        nusb::request_device(&[nusb::DeviceSelector::all().with_vid_pid(PLUTO_VID, PLUTO_PID)])
            .await?
            .map(DeviceDescriptor::from_nusb),
    )
}

#[derive(Debug, Clone)]
pub struct UsbInspection {
    pub active_configuration: Option<u8>,
    pub interfaces: Vec<InterfaceInfo>,
}
#[derive(Debug, Clone)]
pub struct InterfaceInfo {
    pub configuration: u8,
    pub number: u8,
    pub alternate_setting: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub name: Option<String>,
    pub name_error: Option<String>,
    pub string_index: Option<NonZeroU8>,
    pub endpoints: Vec<EndpointInfo>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointInfo {
    pub address: u8,
    pub transfer_type: nusb::descriptors::TransferType,
    pub max_packet_size: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointPair {
    pub pipe_id: u16,
    pub in_address: u8,
    pub out_address: u8,
}

impl InterfaceInfo {
    /// Verify FunctionFS descriptor order; never sort or match endpoint numbers.
    pub fn endpoint_pairs(&self) -> Result<Vec<EndpointPair>> {
        let endpoints = &self.endpoints;
        if endpoints.is_empty() || !endpoints.len().is_multiple_of(2) {
            return Err(Error::Descriptor(
                "IIO needs an even, nonzero endpoint count".into(),
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for ep in endpoints {
            if ep.transfer_type != nusb::descriptors::TransferType::Bulk
                || ep.max_packet_size == 0
                || ep.address & 0x0f == 0
                || ep.address & 0x70 != 0
                || !seen.insert(ep.address)
            {
                return Err(Error::Descriptor(
                    "invalid, duplicate, or non-bulk IIO endpoint".into(),
                ));
            }
        }
        endpoints
            .as_chunks::<2>()
            .0
            .iter()
            .enumerate()
            .map(|(i, pair)| {
                if pair[0].address & 0x80 == 0 || pair[1].address & 0x80 != 0 {
                    return Err(Error::Descriptor(
                        "IIO endpoint order must be IN, OUT".into(),
                    ));
                }
                Ok(EndpointPair {
                    pipe_id: i as u16,
                    in_address: pair[0].address,
                    out_address: pair[1].address,
                })
            })
            .collect()
    }
}
impl UsbInspection {
    /// Select a named IIO interface in the active configuration, preferring its
    /// lowest valid alternate setting. Multiple interface numbers need selection.
    pub fn iio_interface(&self, number: Option<u8>) -> Result<&InterfaceInfo> {
        let candidates: Vec<_> = self
            .interfaces
            .iter()
            .filter(|i| {
                Some(i.configuration) == self.active_configuration
                    && i.name.as_deref() == Some("IIO")
                    && number.is_none_or(|n| n == i.number)
            })
            .collect();
        let first = *candidates.first().ok_or(Error::IioInterfaceNotFound)?;
        if candidates.iter().any(|i| i.number != first.number) {
            return Err(Error::AmbiguousIioInterface);
        }
        if let Some(selected) = candidates
            .into_iter()
            .filter(|i| i.endpoint_pairs().is_ok())
            .min_by_key(|i| i.alternate_setting)
        {
            Ok(selected)
        } else {
            first.endpoint_pairs()?;
            Ok(first)
        }
    }
}

fn snapshot(device: &nusb::Device) -> UsbInspection {
    UsbInspection {
        active_configuration: device
            .active_configuration()
            .ok()
            .map(|c| c.configuration_value()),
        interfaces: device
            .configurations()
            .flat_map(|c| {
                c.interface_alt_settings().map(move |i| InterfaceInfo {
                    configuration: c.configuration_value(),
                    number: i.interface_number(),
                    alternate_setting: i.alternate_setting(),
                    class: i.class(),
                    subclass: i.subclass(),
                    protocol: i.protocol(),
                    name: None,
                    name_error: None,
                    string_index: i.string_index(),
                    endpoints: i
                        .endpoints()
                        .map(|e| EndpointInfo {
                            address: e.address(),
                            transfer_type: e.transfer_type(),
                            max_packet_size: e.max_packet_size(),
                        })
                        .collect(),
                })
            })
            .collect(),
    }
}
pub(crate) fn inspect_device(
    device: &nusb::Device,
) -> impl MaybeFuture<Output = Result<UsbInspection>> + '_ {
    dual!(
        device,
        |device: &nusb::Device| {
            let mut result = snapshot(device);
            for interface in &mut result.interfaces {
                if let Some(index) = interface.string_index {
                    record_name(
                        interface,
                        device
                            .get_string_descriptor(index, 0x0409, DESCRIPTOR_TIMEOUT)
                            .wait(),
                    );
                }
            }
            Ok(result)
        },
        |device: &nusb::Device| async move {
            let mut result = snapshot(device);
            for interface in &mut result.interfaces {
                if let Some(index) = interface.string_index {
                    record_name(
                        interface,
                        device
                            .get_string_descriptor(index, 0x0409, DESCRIPTOR_TIMEOUT)
                            .await,
                    );
                }
            }
            Ok(result)
        }
    )
}
fn record_name<E: std::fmt::Display>(
    interface: &mut InterfaceInfo,
    result: std::result::Result<String, E>,
) {
    match result {
        Ok(name) => interface.name = Some(name),
        Err(e) => interface.name_error = Some(e.to_string()),
    }
}
