#[cfg(not(target_arch = "wasm32"))]
fn main() -> plutosdr::Result<()> {
    futures_lite::future::block_on(async {
        use plutosdr::{Complex32, Device};
        let mut device = Device::open().await?;
        device.set_frequency_hz(2_450_000_000).await?;
        device.set_sample_rate_hz(2_500_000).await?;
        device.set_bandwidth_hz(2_000_000).await?;
        device.set_gain_db(30.0).await?;
        println!(
            "RX: {} Hz, {} samples/s, {} Hz bandwidth, {} dB",
            device.frequency_hz().await?,
            device.sample_rate_hz().await?,
            device.bandwidth_hz().await?,
            device.gain_db().await?
        );
        let mut rx = device.rx_stream()?;
        let mut samples = vec![Complex32::default(); rx.mtu()];
        rx.start().await?;
        for _ in 0..8 {
            let n = rx.read(&mut samples, None).await?;
            let power = samples[..n]
                .iter()
                .map(|s| s.norm_sqr() as f64)
                .sum::<f64>()
                / n as f64;
            println!("{n} samples, mean power {power:.8}, first {:?}", samples[0]);
        }
        rx.stop().await?;
        drop(rx);
        device.shutdown().await
    })
}
#[cfg(target_arch = "wasm32")]
fn main() {}
