//! Run the production nusb WebUSB endpoint queue against delayed JS transfers.
use super::*;
use crate::usb::discovery::EndpointInfo;
use std::future::IntoFuture;
use wasm_bindgen::{JsCast, JsValue, prelude::wasm_bindgen};

#[wasm_bindgen(inline_js = r#"
export function usbMock() {
    const config = [9,2,32,0,1,1,0,128,50, 9,4,0,0,2,255,0,0,0,
                    7,5,129,2,0,2,0, 7,5,1,2,0,2,0];
    const descriptor = [18,1,0,2,0,0,0,64,86,4,115,182,0,1,0,0,0,1];
    const d = {
        configuration: { configurationValue: 1, interfaces: [{interfaceNumber: 0, claimed: false}] }, configurations: [{}],
        pending: [], peak: 0, calls: 0, closes: 0, held: false,
        bytes: new Uint8Array(), offset: 0,
        open: async () => {}, claimInterface: async () => {},
        releaseInterface: async () => {},
        controlTransferIn: async (setup, len) => ({ status: 'ok',
            data: new DataView(new Uint8Array((setup.value >> 8) === 1 ? descriptor : config).slice(0,len).buffer) }),
        controlTransferOut: async () => { d.closes++; return { status: 'ok', bytesWritten: 0 }; },
        transferIn: (ep, len) => new Promise(resolve => {
            d.pending.push({resolve, len, call: d.calls++});
            d.peak = Math.max(d.peak, d.pending.length);
            setTimeout(() => flush(d), 0);
        }),
    };
    return d;
}
function flush(d) {
    while (!d.held && d.pending.length && d.offset < d.bytes.length) {
        const p = d.pending.shift();
        // Short data and a ZLP between full transfers exercise queue budgeting.
        const n = Math.min(p.len, d.bytes.length - d.offset,
                           p.call === 0 ? 31 : p.call === 1 ? 0 : Infinity);
        const data = d.bytes.slice(d.offset, d.offset + n);
        d.offset += n;
        p.resolve({status:'ok', data:new DataView(data.buffer)});
    }
}
export function payload(d, n, hold) {
    d.bytes = Uint8Array.from({length:n + 2}, (_, i) => i < n ? i % 251 : (i === n ? 48 : 10));
    d.offset = 0; d.held = hold;
}
export function release(d) { d.held = false; flush(d); }
export function peak(d) { return d.peak; }
export function pending(d) { return d.pending.length; }
export function closes(d) { return d.closes; }
"#)]
extern "C" {
    fn usbMock() -> JsValue;
    fn payload(device: &JsValue, length: usize, hold: bool);
    fn release(device: &JsValue);
    fn peak(device: &JsValue) -> usize;
    fn pending(device: &JsValue) -> usize;
    fn closes(device: &JsValue) -> usize;
}

async fn transport() -> (NusbTransport, JsValue) {
    let js = usbMock();
    let device = nusb::Device::from_js(js.clone().unchecked_into())
        .await
        .unwrap();
    let interface = device.claim_interface(0).await.unwrap();
    let info = InterfaceInfo {
        configuration: 1,
        number: 0,
        alternate_setting: 0,
        class: 255,
        subclass: 0,
        protocol: 0,
        name: Some("IIO".into()),
        name_error: None,
        string_index: None,
        endpoints: [0x81, 0x01]
            .into_iter()
            .map(|address| EndpointInfo {
                address,
                transfer_type: nusb::descriptors::TransferType::Bulk,
                max_packet_size: 512,
            })
            .collect(),
    };
    (
        NusbTransport::from_claim(device, interface, info, Duration::from_secs(1), 0).unwrap(),
        js,
    )
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn webusb_pipelines_real_endpoint_calls_without_losing_bytes() {
    let (mut transport, js) = transport().await;
    let mut bytes = vec![0; 1024 * 1024 + 3];
    payload(&js, bytes.len(), false);
    transport.read_exact(&mut bytes).await.unwrap();
    assert!(bytes.iter().enumerate().all(|(i, b)| *b == (i % 251) as u8));
    assert_eq!(peak(&js), TRANSFER_COUNT);
    assert_eq!(pending(&js), 0);
    assert_eq!(transport.input.as_ref().unwrap().pending(), 0);
    assert_eq!(transport.spare_reads.len(), TRANSFER_COUNT);
    assert_eq!(transport.read(2).await.unwrap(), b"0\n");
    transport.shutdown().await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn webusb_consumes_borrowed_completions_and_recycles_buffers() {
    let (mut transport, js) = transport().await;
    let length = 1024 * 1024 + 3;
    payload(&js, length, false);
    let mut count = 0;
    let mut allocations = std::collections::HashSet::new();
    transport
        .consume_exact(length, |bytes| {
            if !bytes.is_empty() {
                allocations.insert(bytes.as_ptr() as usize);
            }
            assert!(
                bytes
                    .iter()
                    .enumerate()
                    .all(|(i, b)| *b == ((count + i) % 251) as u8)
            );
            count += bytes.len();
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(count, length);
    assert_eq!(allocations.len(), TRANSFER_COUNT);
    assert_eq!(peak(&js), TRANSFER_COUNT);
    assert_eq!(pending(&js), 0);
    assert_eq!(transport.read(2).await.unwrap(), b"0\n");
    transport.shutdown().await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn webusb_callback_failure_can_drain_and_close() {
    let (mut transport, js) = transport().await;
    payload(&js, 1024 * 1024, false);
    assert!(matches!(
        transport
            .consume_exact(1024 * 1024, |_| {
                Err(Error::Protocol("rejected samples"))
            })
            .await,
        Err(Error::Protocol("rejected samples"))
    ));
    transport.shutdown().await.unwrap();
    assert_eq!(pending(&js), 0);
    assert_eq!(closes(&js), 1);
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn webusb_shutdown_drains_cancelled_payload_before_closing() {
    let (mut transport, js) = transport().await;
    let mut bytes = vec![0; 1024 * 1024];
    payload(&js, bytes.len(), true);
    let mut read = Box::pin(transport.read_exact(&mut bytes).into_future());
    assert!(
        futures_lite::future::poll_once(read.as_mut())
            .await
            .is_none()
    );
    assert_eq!(pending(&js), TRANSFER_COUNT);
    drop(read);
    let mut close = Box::pin(transport.shutdown().into_future());
    assert!(
        futures_lite::future::poll_once(close.as_mut())
            .await
            .is_none()
    );
    assert_eq!(closes(&js), 0);
    release(&js);
    close.await.unwrap();
    assert_eq!(pending(&js), 0);
    assert_eq!(closes(&js), 1);
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn webusb_drop_keeps_endpoint_claims_until_abandoned_reads_settle() {
    let (mut transport, js) = transport().await;
    let interface = transport.interface.clone();
    let mut bytes = vec![0; 1024 * 1024];
    payload(&js, bytes.len(), true);
    let mut read = Box::pin(transport.read_exact(&mut bytes).into_future());
    assert!(
        futures_lite::future::poll_once(read.as_mut())
            .await
            .is_none()
    );
    drop(read);
    drop(transport);
    assert!(interface.endpoint::<Bulk, In>(0x81).is_err());
    release(&js);
    for _ in 0..100 {
        if let Ok(endpoint) = interface.endpoint::<Bulk, In>(0x81) {
            assert_eq!(pending(&js), 0);
            assert_eq!(closes(&js), 1);
            drop(endpoint);
            return;
        }
        futures_lite::future::yield_now().await;
    }
    panic!("background endpoint cleanup did not finish");
}
