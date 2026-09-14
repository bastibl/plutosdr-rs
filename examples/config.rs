#[cfg(not(target_arch = "wasm32"))]
fn main() -> plutosdr::Result<()> {
    use plutosdr::{Device, MaybeFuture, RxAttribute};
    let mut dev = Device::open().wait()?;
    for attr in [
        RxAttribute::Frequency,
        RxAttribute::SampleRate,
        RxAttribute::Bandwidth,
        RxAttribute::Gain,
        RxAttribute::GainMode,
        RxAttribute::Port,
    ] {
        println!(
            "{attr:?}: {:?}, available {:?}",
            dev.read_rx_attribute(attr, false).wait(),
            dev.read_rx_attribute(attr, true).wait()
        );
    }
    dev.shutdown().wait()
}
#[cfg(target_arch = "wasm32")]
fn main() {}
