//! Explicit smoke test; no hardware access in default test runs.
#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "requires a Pluto connected over USB with access permissions"]
fn context_refresh_shutdown_and_reopen() -> plutosdr::Result<()> {
    use plutosdr::{Device, MaybeFuture};
    let mut device = Device::open().wait()?;
    let first = device.info().clone();
    assert!(!first.devices.is_empty());
    device.refresh_info().wait()?;
    assert_eq!(first, *device.info());
    let serial = device.descriptor().serial.clone().expect("Pluto serial");
    device.shutdown().wait()?;
    device.shutdown().wait()?;
    assert!(matches!(
        device.refresh_info().wait(),
        Err(plutosdr::Error::DeviceClosed)
    ));
    let mut reopened = Device::open_serial(&serial).wait()?;
    assert_eq!(first, *reopened.info());
    reopened.shutdown().wait()?;

    // Repeat on the same device identity to catch leftover response bytes or
    // endpoint ownership that survives shutdown/drop.
    for cycle in 0..10 {
        let mut device = Device::open_serial(&serial).wait()?;
        for _ in 0..10 {
            device.refresh_info().wait()?;
            assert_eq!(first, *device.info());
        }
        if cycle % 2 == 0 {
            device.shutdown().wait()?;
        }
        // Odd cycles deliberately exercise best-effort Drop cleanup.
    }
    eprintln!("Blocking: 100 context refreshes across 10 open/close cycles passed");

    #[cfg(feature = "smol")]
    futures_lite::future::block_on(check_async_lifecycle(&serial, &first))?;

    Ok(())
}

#[cfg(all(not(target_arch = "wasm32"), feature = "smol"))]
async fn check_async_lifecycle(
    serial: &str,
    expected: &plutosdr::iiod::Context,
) -> plutosdr::Result<()> {
    use plutosdr::{Device, Error};
    for cycle in 0..10 {
        let mut device = Device::open_serial(serial).await?;
        assert_eq!(expected, device.info());
        for _ in 0..10 {
            device.refresh_info().await?;
            assert_eq!(expected, device.info());
        }
        if cycle % 2 == 0 {
            device.shutdown().await?;
            device.shutdown().await?;
            assert!(matches!(
                device.refresh_info().await,
                Err(Error::DeviceClosed)
            ));
        }
    }
    eprintln!("Async: 100 context refreshes across 10 open/close cycles passed");
    Ok(())
}
