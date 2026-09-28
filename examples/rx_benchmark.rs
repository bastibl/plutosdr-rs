//! RX throughput without DSP. Run with --release --features smol.
#[cfg(not(target_arch = "wasm32"))]
fn main() -> plutosdr::Result<()> {
    use plutosdr::{Complex32, Device, MaybeFuture};
    use std::time::{Duration, Instant};
    let args: Vec<_> = std::env::args().collect();
    let asynchronous = args.get(1).is_some_and(|s| s == "async");
    let buffer_samples = args.get(2).map(|s| s.parse().unwrap()).unwrap_or(65536);
    let duration = Duration::from_secs(5);
    let report = |count: usize, elapsed: Duration| {
        println!(
            "{}: buffer={} samples, {:.3} MS/s, {:.3} MB/s USB payload, {} samples in {:.3}s",
            if asynchronous { "async" } else { "blocking" },
            buffer_samples,
            count as f64 / elapsed.as_secs_f64() / 1e6,
            count as f64 * 4.0 / elapsed.as_secs_f64() / 1e6,
            count,
            elapsed.as_secs_f64()
        );
    };
    if asynchronous {
        futures_lite::future::block_on(async {
            let mut device = Device::open().await?;
            device.set_frequency_hz(2_462_000_000).await?;
            device.set_sample_rate_hz(20_000_000).await?;
            device.set_bandwidth_hz(20_000_000).await?;
            device.set_gain_db(50.0).await?;
            let mut rx = device.rx_stream_with_buffer(buffer_samples)?;
            let mut output = vec![Complex32::default(); buffer_samples];
            rx.start().await?;
            rx.read(&mut output, None).await?;
            let start = Instant::now();
            let mut count = 0;
            while start.elapsed() < duration {
                count += rx.read(std::hint::black_box(&mut output), None).await?;
            }
            let elapsed = start.elapsed();
            rx.stop().await?;
            drop(rx);
            device.shutdown().await?;
            report(count, elapsed);
            Ok(())
        })
    } else {
        let mut device = Device::open().wait()?;
        device.set_frequency_hz(2_462_000_000).wait()?;
        device.set_sample_rate_hz(20_000_000).wait()?;
        device.set_bandwidth_hz(20_000_000).wait()?;
        device.set_gain_db(50.0).wait()?;
        let mut rx = device.rx_stream_with_buffer(buffer_samples)?;
        let mut output = vec![Complex32::default(); buffer_samples];
        rx.start().wait()?;
        rx.read(&mut output, None).wait()?;
        let start = Instant::now();
        let mut count = 0;
        while start.elapsed() < duration {
            count += rx.read(std::hint::black_box(&mut output), None).wait()?;
        }
        let elapsed = start.elapsed();
        rx.stop().wait()?;
        drop(rx);
        device.shutdown().wait()?;
        report(count, elapsed);
        Ok(())
    }
}
#[cfg(target_arch = "wasm32")]
fn main() {}
