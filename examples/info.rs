#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> plutosdr::Result<()> {
    use plutosdr::{Device, MaybeFuture};
    let mut builder = Device::builder();
    if let Some(serial) = std::env::args().nth(1) {
        builder = builder.serial(serial);
    }
    let mut device = builder.open().wait()?;
    println!(
        "USB serial={:?}, IIO interface={}",
        device.descriptor().serial,
        device.interface_info().number
    );
    println!(
        "Context {}: {:?}",
        device.info().name,
        device.info().description
    );
    for iio in &device.info().devices {
        println!("{} name={:?} label={:?}", iio.id, iio.name, iio.label);
        for attr in &iio.attributes {
            println!("  attribute {}", attr.name);
        }
        for ch in &iio.channels {
            println!(
                "  {:?} {} name={:?} scan={:?}",
                ch.direction, ch.id, ch.name, ch.scan_element
            );
            for attr in &ch.attributes {
                println!("    attribute {} file={:?}", attr.name, attr.filename);
            }
        }
    }
    device.shutdown().wait()
}
