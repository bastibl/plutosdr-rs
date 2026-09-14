#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> plutosdr::Result<()> {
    use plutosdr::{Device, MaybeFuture, usb::discovery};
    let all = std::env::args().any(|a| a == "--all");
    let devices = if all {
        discovery::list_all().wait()?
    } else {
        Device::list().wait()?
    };
    if devices.is_empty() {
        println!("No PlutoSDR found. Use --all to inspect custom USB identities.");
    }
    for device in devices {
        println!(
            "{:04x}:{:04x} product={:?} serial={:?}{}",
            device.vid,
            device.pid,
            device.product_string,
            device.serial,
            if device.is_likely_pluto() {
                " (likely Pluto)"
            } else {
                ""
            }
        );
        match device.inspect().wait() {
            Err(e) => eprintln!("  cannot inspect: {e}"),
            Ok(inspection) => {
                for interface in &inspection.interfaces {
                    println!(
                        "  config={}{} interface={} alt={} class={:02x}/{:02x}/{:02x} name={:?}",
                        interface.configuration,
                        if Some(interface.configuration) == inspection.active_configuration {
                            " (active)"
                        } else {
                            ""
                        },
                        interface.number,
                        interface.alternate_setting,
                        interface.class,
                        interface.subclass,
                        interface.protocol,
                        interface.name
                    );
                    if let Some(e) = &interface.name_error {
                        println!("    string descriptor: {e}");
                    }
                    for endpoint in &interface.endpoints {
                        println!(
                            "    endpoint={:02x} {:?} max_packet={}",
                            endpoint.address, endpoint.transfer_type, endpoint.max_packet_size
                        );
                    }
                    if interface.name.as_deref() == Some("IIO") {
                        println!("    IIO pipes: {:?}", interface.endpoint_pairs());
                    }
                }
                match inspection.iio_interface(None) {
                    Ok(i) => println!(
                        "  selected IIO interface {} alt {}",
                        i.number, i.alternate_setting
                    ),
                    Err(e) => println!("  IIO selection: {e}"),
                }
            }
        }
    }
    Ok(())
}
