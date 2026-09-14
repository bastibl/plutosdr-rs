#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> plutosdr::Result<()> {
    futures_lite::future::block_on(async {
        let mut device = plutosdr::Device::open().await?;
        println!("{:#?}", device.info());
        device.shutdown().await
    })
}
