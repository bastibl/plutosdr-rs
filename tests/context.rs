use plutosdr::iiod::{ChannelDirection, Context};

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn legacy_dtd_context_preserves_identifiers_attributes_and_formats() {
    let context = Context::from_xml(include_str!("fixtures/context.xml")).unwrap();
    assert_eq!(context.name, "local");
    assert_eq!(
        context.description.as_deref(),
        Some("Synthetic Pluto & IIOD fixture")
    );
    assert_eq!(context.attributes[0].value.as_deref(), Some("ADALM-PLUTO"));
    assert_eq!(context.devices.len(), 3);
    let phy = &context.devices[0];
    assert_eq!(phy.id, "iio:device3");
    assert_eq!(phy.channels[0].direction, ChannelDirection::Output);
    assert_eq!(
        phy.channels[0].attributes[0].filename.as_deref(),
        Some("out_altvoltage0_RX_LO_frequency")
    );
    assert!(phy.channels[0].attributes[0].value.is_none());
    let rx = &context.devices[1];
    assert_eq!(rx.buffer_attributes[0].name, "length");
    assert_eq!(
        rx.channels[0].scan_element.as_ref().unwrap().format,
        "le:S12/16>>0"
    );
    assert_eq!(rx.channels[1].scan_element.as_ref().unwrap().index, 1);
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn newer_buffers_and_unknown_extensions_are_supported() {
    let context = Context::from_xml(r#"<context name="local"><device id="iio:device0">
      <buffer index="2" direction="in"><attribute name="length" value="1024"/>
        <channel id="voltage0" type="input"><scan-element index="0" format="le:u16/16&gt;&gt;0"/></channel>
      </buffer><future-extension><attribute name="not-a-device-attribute"/></future-extension>
    </device></context>"#).unwrap();
    assert!(context.devices[0].attributes.is_empty());
    assert_eq!(context.devices[0].buffers[0].index, 2);
    assert_eq!(context.devices[0].buffers[0].channels[0].id, "voltage0");
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn malformed_contexts_fail_without_panicking() {
    for xml in [
        "",
        "<other/>",
        "<context/>",
        "<context name='x'><device/></context>",
        "<context name='x'><device id='x'><channel id='v' type='sideways'/></device></context>",
        "<context name='x'><device id='x'><channel id='v' type='input'><scan-element index='NaN' format='x'/></channel></device></context>",
        "<!DOCTYPE context [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><context name='&x;'/>",
    ] {
        assert!(Context::from_xml(xml).is_err(), "accepted {xml}");
    }
}
