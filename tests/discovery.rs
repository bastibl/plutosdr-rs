use nusb::descriptors::TransferType;
use plutosdr::{
    Error,
    usb::discovery::{EndpointInfo, InterfaceInfo, UsbInspection},
};

fn interface() -> InterfaceInfo {
    InterfaceInfo {
        configuration: 3,
        number: 7,
        alternate_setting: 2,
        class: 2,
        subclass: 0,
        protocol: 0,
        name: Some("IIO".into()),
        name_error: None,
        string_index: None,
        endpoints: [0x85, 0x03, 0x82, 0x07]
            .into_iter()
            .map(|address| EndpointInfo {
                address,
                transfer_type: TransferType::Bulk,
                max_packet_size: 512,
            })
            .collect(),
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn relocated_endpoints_are_paired_in_descriptor_order() {
    let i = interface();
    let pairs = i.endpoint_pairs().unwrap();
    assert_eq!(
        (pairs[0].pipe_id, pairs[0].in_address, pairs[0].out_address),
        (0, 0x85, 0x03)
    );
    assert_eq!(
        (pairs[1].pipe_id, pairs[1].in_address, pairs[1].out_address),
        (1, 0x82, 0x07)
    );
    let inspection = UsbInspection {
        active_configuration: Some(3),
        interfaces: vec![i],
    };
    assert_eq!(inspection.iio_interface(None).unwrap().number, 7);
    assert_eq!(inspection.iio_interface(None).unwrap().alternate_setting, 2);
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn identity_requires_the_name_and_active_configuration() {
    let mut i = interface();
    i.name = Some("RNDIS".into());
    let mut inspection = UsbInspection {
        active_configuration: Some(3),
        interfaces: vec![i],
    };
    assert!(matches!(
        inspection.iio_interface(None),
        Err(Error::IioInterfaceNotFound)
    ));
    inspection.interfaces[0].name = Some("IIO".into());
    inspection.active_configuration = Some(1);
    assert!(inspection.iio_interface(None).is_err());
    inspection.active_configuration = Some(3);
    let mut other = interface();
    other.number = 9;
    inspection.interfaces.push(other);
    assert!(matches!(
        inspection.iio_interface(None),
        Err(Error::AmbiguousIioInterface)
    ));
    assert_eq!(inspection.iio_interface(Some(9)).unwrap().number, 9);
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn invalid_endpoint_layouts_are_rejected() {
    let i = interface();
    let mut cases = vec![i.clone(); 6];
    cases[0].endpoints.clear();
    cases[1].endpoints.pop();
    cases[2].endpoints.swap(0, 1);
    cases[3].endpoints[0].transfer_type = TransferType::Interrupt;
    cases[4].endpoints[2].address = cases[4].endpoints[0].address;
    cases[5].endpoints[0].address = 0x80;
    for bad in cases {
        assert!(bad.endpoint_pairs().is_err());
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn alternate_settings_do_not_look_like_multiple_interfaces() {
    let valid = interface();
    let mut empty = valid.clone();
    empty.alternate_setting = 0;
    empty.endpoints.clear();
    let inspection = UsbInspection {
        active_configuration: Some(3),
        interfaces: vec![empty, valid],
    };
    assert_eq!(inspection.iio_interface(None).unwrap().alternate_setting, 2);
}
