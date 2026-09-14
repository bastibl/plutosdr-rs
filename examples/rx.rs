#[cfg(not(target_arch = "wasm32"))]
fn main() -> plutosdr::Result<()> {
    use plutosdr::{Complex32, Device, MaybeFuture};
    let mut device = Device::open().wait()?;
    device.set_frequency_hz(2_450_000_000).wait()?;
    device.set_sample_rate_hz(2_500_000).wait()?;
    device.set_bandwidth_hz(2_000_000).wait()?;
    device.set_gain_db(30.0).wait()?;
    println!(
        "RX: {} Hz, {} samples/s, {} Hz bandwidth, {} dB",
        device.frequency_hz().wait()?,
        device.sample_rate_hz().wait()?,
        device.bandwidth_hz().wait()?,
        device.gain_db().wait()?
    );
    let mut rx = device.rx_stream()?;
    let mut samples = vec![Complex32::default(); rx.mtu()];
    rx.start().wait()?;
    for _ in 0..8 {
        let n = rx.read(&mut samples, None).wait()?;
        let power = samples[..n]
            .iter()
            .map(|s| s.norm_sqr() as f64)
            .sum::<f64>()
            / n as f64;
        println!("{n} samples, mean power {power:.8}, first {:?}", samples[0]);
    }
    rx.stop().wait()?;
    drop(rx);
    device.shutdown().wait()
}
#[cfg(target_arch = "wasm32")]
fn main() {}
