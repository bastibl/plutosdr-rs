#![cfg(not(target_arch = "wasm32"))]
use plutosdr::{Complex32, Device, Error, GainMode, MaybeFuture, Result};
use std::time::{Duration, Instant};

fn check_samples(samples: &[Complex32]) {
    assert!(!samples.is_empty());
    assert!(samples.iter().all(|s| s.re.is_finite()
        && s.im.is_finite()
        && s.re >= -1.0
        && s.re < 1.0
        && s.im >= -1.0
        && s.im < 1.0));
    assert!(
        samples.windows(2).any(|pair| pair[0] != pair[1]),
        "samples must vary"
    );
}

#[test]
#[ignore = "requires Pluto; configures RX at 2.45 GHz, 2.5 MS/s, 2 MHz bandwidth"]
fn rx_configuration_streaming_and_recovery() -> Result<()> {
    let mut device = Device::open().wait()?;
    assert!(device.dc_offset_available());
    for enabled in [false, true] {
        device.set_dc_offset_enabled(enabled).wait()?;
        assert_eq!(device.dc_offset_enabled().wait()?, enabled);
    }
    // Cross every FIR profile boundary and return from a sub-2 MS/s rate.
    for rate in [
        3_200_000, 20_000_000, 32_000_000, 48_000_000, 61_440_000, 1_000_000, 3_200_000,
    ] {
        device.set_sample_rate_hz(rate).wait()?;
        assert!(device.sample_rate_hz().wait()?.abs_diff(rate) < 5);
        let available = device
            .read_rx_attribute(plutosdr::RxAttribute::SampleRate, true)
            .wait()?;
        let range = plutosdr::ValueRange::parse(&available)?;
        assert!(range.min < 1_100_000.0, "FIR must be enabled");
        assert_eq!(
            device.bandwidth_hz().wait()?,
            device.sample_rate_hz().wait()?.min(56_000_000)
        );
    }

    assert_eq!(
        device.bandwidth_hz().wait()?,
        device.sample_rate_hz().wait()?
    );
    device.set_frequency_hz(2_450_000_000).wait()?;
    device.set_sample_rate_hz(2_500_000).wait()?;
    device.set_bandwidth_hz(2_000_000).wait()?;
    device.set_gain_db(30.0).wait()?;
    assert_eq!(device.frequency_hz().wait()?, 2_450_000_000);
    assert!(device.sample_rate_hz().wait()?.abs_diff(2_500_000) < 5);
    assert_eq!(device.bandwidth_hz().wait()?, 2_000_000);
    assert_eq!(device.gain_db().wait()?, 30.0);
    assert_eq!(device.gain_mode().wait()?, GainMode::Manual);
    for mode in [
        GainMode::FastAttack,
        GainMode::SlowAttack,
        GainMode::Hybrid,
        GainMode::Manual,
    ] {
        device.set_gain_mode(mode).wait()?;
        assert_eq!(device.gain_mode().wait()?, mode);
    }
    let port = device.rf_port().wait()?;
    device.set_rf_port(port.clone()).wait()?;
    assert_eq!(device.rf_port().wait()?, port);
    assert!(device.set_gain_db(f64::NAN).wait().is_err());
    assert!(device.set_sample_rate_hz(1).wait().is_err());
    assert!(device.rx_stream_with_buffer(0).is_err());
    let start = Instant::now();
    let mut total = 0;
    for _ in 0..3 {
        let mut rx = device.rx_stream_with_buffer(16384)?;
        assert!(matches!(device.rx_stream(), Err(Error::Busy)));
        assert!(matches!(device.shutdown().wait(), Err(Error::Busy)));
        let mut samples = vec![Complex32::default(); 16384];
        assert!(matches!(
            rx.read(&mut samples, None).wait(),
            Err(Error::StreamInactive)
        ));
        rx.start().wait()?;
        rx.start().wait()?;
        assert!(matches!(
            rx.read(&mut samples, Some(Duration::ZERO)).wait(),
            Err(Error::Timeout)
        ));
        for _ in 0..16 {
            let n = rx.read(&mut samples, None).wait()?;
            check_samples(&samples[..n]);
            total += n;
        }
        // Smaller client buffers must consume a cached full DMA buffer, not
        // discard its tail and request new samples on each call.
        assert_eq!(rx.read(&mut samples[..17], None).wait()?, 17);
        assert_eq!(
            rx.read(&mut samples, Some(Duration::ZERO)).wait()?,
            16384 - 17
        );
        device.refresh_info().wait()?; // pipe 0 remains usable with RX active
        rx.stop().wait()?;
        rx.stop().wait()?;
        rx.start().wait()?;
        assert_eq!(rx.read(&mut samples, None).wait()?, samples.len());
        // Drop an active stream; the next cycle must reopen pipe 1.
    }
    eprintln!(
        "Native RX: {total} samples, {:.3}s including lifecycle checks",
        start.elapsed().as_secs_f64()
    );
    let mut rx = device.rx_stream_with_buffer(4096)?;
    drop(device); // owned stream keeps the claimed USB interface alive
    rx.start().wait()?;
    let mut samples = vec![Complex32::default(); 4096];
    assert_eq!(rx.read(&mut samples, None).wait()?, 4096);
    rx.stop().wait()?;
    drop(rx);
    #[cfg(feature = "smol")]
    futures_lite::future::block_on(async_checks())?;
    Ok(())
}

#[cfg(feature = "smol")]
async fn async_checks() -> Result<()> {
    use std::future::IntoFuture;
    let mut device = Device::open().await?;
    assert!(device.dc_offset_available());
    for enabled in [false, true] {
        device.set_dc_offset_enabled(enabled).await?;
        assert_eq!(device.dc_offset_enabled().await?, enabled);
    }
    for rate in [32_000_000, 1_000_000, 3_200_000] {
        device.set_sample_rate_hz(rate).await?;
        assert!(device.sample_rate_hz().await?.abs_diff(rate) < 5);
    }
    assert_eq!(device.bandwidth_hz().await?, device.sample_rate_hz().await?);
    device.set_gain_db(25.0).await?;
    assert_eq!(device.gain_db().await?, 25.0);
    let mut rx = device.rx_stream_with_buffer(65536)?;
    let mut opening = Box::pin(rx.start().into_future());
    assert!(
        futures_lite::future::poll_once(opening.as_mut())
            .await
            .is_none()
    );
    drop(opening);
    assert!(matches!(rx.start().await, Err(Error::SessionPoisoned)));
    rx.stop().await?;
    rx.start().await?;
    let mut samples = vec![Complex32::default(); 65536];
    for _ in 0..16 {
        let (n, frequency) = futures_lite::future::zip(
            rx.read(&mut samples, None).into_future(),
            device.frequency_hz().into_future(),
        )
        .await;
        check_samples(&samples[..n?]);
        assert_eq!(frequency?, 2_450_000_000);
    }
    let mut read = Box::pin(rx.read(&mut samples, None).into_future());
    assert!(
        futures_lite::future::poll_once(read.as_mut())
            .await
            .is_none()
    );
    drop(read);
    assert!(matches!(rx.start().await, Err(Error::SessionPoisoned)));
    assert!(matches!(
        rx.read(&mut samples, None).await,
        Err(Error::SessionPoisoned)
    ));
    rx.stop().await?;
    rx.start().await?;
    let n = rx.read(&mut samples, None).await?;
    check_samples(&samples[..n]);
    // A nonzero deadline failure also needs a new pipe session.
    assert!(matches!(
        rx.read(&mut samples, Some(Duration::from_nanos(1))).await,
        Err(Error::Timeout)
    ));
    rx.stop().await?;
    rx.start().await?;
    assert_eq!(rx.read(&mut samples, None).await?, samples.len());
    rx.stop().await?;
    drop(rx);
    device.shutdown().await?;
    eprintln!(
        "Async RX: 1048576 samples, concurrent control reads, cancelled read, stop/restart passed"
    );
    Ok(())
}
