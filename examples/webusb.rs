//! Build with `cargo build --target wasm32-unknown-unknown --example webusb`.
//! Bind the exported function to a click handler using wasm-bindgen tooling.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub async fn inspect_pluto() -> Result<String, wasm_bindgen::JsValue> {
    async {
        plutosdr::Device::request_permission().await?;
        let mut device = plutosdr::Device::open().await?;
        let text = format!("{:#?}", device.info());
        device.shutdown().await?;
        Ok::<_, plutosdr::Error>(text)
    }
    .await
    .map_err(|e| wasm_bindgen::JsValue::from_str(&e.to_string()))
}
fn main() {}
