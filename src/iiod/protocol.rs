use super::Context;
use crate::{
    Error, Result,
    maybe_future::{PlatformSend, dual},
};
use nusb::MaybeFuture;
use std::collections::VecDeque;

/// Upper bound for a context response before allocating its payload.
pub const MAX_XML_BYTES: usize = 8 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 64;

/// One ordered IIOD connection. Native implementations and their futures are Send.
///
/// `write_command` must deliver a complete short ASCII command as one write
/// (one bulk transfer for FunctionFS). A short write is an error, not retryable.
/// `read` returns at most `max_bytes`; short reads are legal, empty means EOF.
/// USB implementations must absorb ZLPs rather than report them as EOF.
/// Every method must be lazy. Only shutdown is safe after cancelled/failed I/O.
pub trait Transport: PlatformSend {
    fn write_command(&mut self, command: &[u8]) -> impl MaybeFuture<Output = Result<()>>;
    fn read(&mut self, max_bytes: usize) -> impl MaybeFuture<Output = Result<Vec<u8>>>;
    fn shutdown(&mut self) -> impl MaybeFuture<Output = Result<()>>;
}

/// Serialized ASCII client owning one transport, usable with native wait or await.
pub struct IiodClient<T: Transport> {
    transport: T,
    pending: VecDeque<u8>,
    poisoned: bool,
    closed: bool,
}

impl<T: Transport> IiodClient<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            pending: VecDeque::new(),
            poisoned: false,
            closed: false,
        }
    }

    /// Fetch and parse PRINT. Dropping an executing request poisons the session.
    pub fn context(&mut self) -> impl MaybeFuture<Output = Result<Context>> + '_ {
        self.context_xml().map(|xml| Context::from_xml(&xml?))
    }

    /// Fetch length-delimited UTF-8 XML, consuming its trailing newline.
    pub fn context_xml(&mut self) -> impl MaybeFuture<Output = Result<String>> + '_ {
        dual!(
            self,
            |this: &mut Self| {
                this.begin()?;
                this.transport.write_command(b"PRINT\r\n").wait()?;
                let mut decoder = PrintDecoder::default();
                loop {
                    if let Some(result) = decoder.consume(&mut this.pending)? {
                        return this.finish(result);
                    }
                    let bytes = this.transport.read(4096).wait()?;
                    this.receive(bytes)?;
                }
            },
            |this: &mut Self| async move {
                this.begin()?;
                this.transport.write_command(b"PRINT\r\n").await?;
                let mut decoder = PrintDecoder::default();
                loop {
                    if let Some(result) = decoder.consume(&mut this.pending)? {
                        return this.finish(result);
                    }
                    let bytes = this.transport.read(4096).await?;
                    this.receive(bytes)?;
                }
            }
        )
    }

    fn begin(&mut self) -> Result<()> {
        if self.closed {
            return Err(Error::DeviceClosed);
        }
        if self.poisoned {
            return Err(Error::SessionPoisoned);
        }
        self.poisoned = true;
        Ok(())
    }

    fn receive(&mut self, bytes: Vec<u8>) -> Result<()> {
        if bytes.is_empty() {
            return Err(Error::Protocol("unexpected end of response"));
        }
        if bytes.len() > 4096 {
            return Err(Error::Protocol("transport exceeded read limit"));
        }
        self.pending.extend(bytes);
        Ok(())
    }

    fn finish(&mut self, result: Result<Vec<u8>>) -> Result<String> {
        // Complete remote errors are framed replies and do not desynchronize.
        self.poisoned = false;
        String::from_utf8(result?).map_err(|_| Error::Protocol("XML is not UTF-8"))
    }

    /// Terminal, retryable shutdown. Repeating a successful shutdown is harmless.
    pub fn shutdown(&mut self) -> impl MaybeFuture<Output = Result<()>> + '_ {
        dual!(
            self,
            |this: &mut Self| {
                this.closed = true;
                this.transport.shutdown().wait()
            },
            |this: &mut Self| async move {
                this.closed = true;
                this.transport.shutdown().await
            }
        )
    }
}

#[derive(Default)]
struct PrintDecoder {
    line: Vec<u8>,
    length: Option<usize>,
    payload: Vec<u8>,
}

impl PrintDecoder {
    fn consume(&mut self, bytes: &mut VecDeque<u8>) -> Result<Option<Result<Vec<u8>>>> {
        while let Some(byte) = bytes.pop_front() {
            if let Some(length) = self.length {
                if self.payload.len() < length {
                    self.payload.push(byte);
                } else {
                    if byte != b'\n' {
                        return Err(Error::Protocol("missing XML newline trailer"));
                    }
                    return Ok(Some(Ok(std::mem::take(&mut self.payload))));
                }
            } else if byte == b'\n' {
                if self.line.last() == Some(&b'\r') {
                    self.line.pop();
                }
                let text = std::str::from_utf8(&self.line)
                    .map_err(|_| Error::Protocol("non-ASCII response length"))?;
                let digits = text.strip_prefix('-').unwrap_or(text);
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(Error::Protocol("invalid response length"));
                }
                let length = text
                    .parse::<i32>()
                    .map_err(|_| Error::Protocol("response length overflow"))?;
                if length < 0 {
                    return Ok(Some(Err(Error::Remote(length))));
                }
                let length = length as usize;
                if length > MAX_XML_BYTES {
                    return Err(Error::Protocol("XML exceeds size limit"));
                }
                self.length = Some(length);
                self.payload = Vec::with_capacity(length);
            } else {
                if self.line.len() == MAX_LINE_BYTES {
                    return Err(Error::Protocol("response line too long"));
                }
                self.line.push(byte);
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::IntoFuture;

    const XML: &str = include_str!("../../tests/fixtures/context.xml");

    enum Read {
        Data(Vec<u8>),
        Error,
        Pending,
    }
    struct Script {
        reads: VecDeque<Read>,
        commands: Vec<Vec<u8>>,
        shutdown_calls: usize,
        shutdown_failures: usize,
        closed: bool,
    }
    impl Script {
        fn new(chunks: impl IntoIterator<Item = Vec<u8>>) -> Self {
            Self {
                reads: chunks.into_iter().map(Read::Data).collect(),
                commands: vec![],
                shutdown_calls: 0,
                shutdown_failures: 0,
                closed: false,
            }
        }
        fn write(&mut self, bytes: &[u8]) -> Result<()> {
            self.commands.push(bytes.into());
            Ok(())
        }
        fn close(&mut self) -> Result<()> {
            if self.closed {
                return Ok(());
            }
            self.shutdown_calls += 1;
            if self.shutdown_failures > 0 {
                self.shutdown_failures -= 1;
                return Err(Error::Timeout);
            }
            self.closed = true;
            Ok(())
        }
    }
    impl Transport for Script {
        fn write_command(&mut self, command: &[u8]) -> impl MaybeFuture<Output = Result<()>> {
            dual!(
                (self, command),
                |(s, c): (&mut Self, &[u8])| s.write(c),
                |(s, c): (&mut Self, &[u8])| async move { s.write(c) }
            )
        }
        fn read(&mut self, max: usize) -> impl MaybeFuture<Output = Result<Vec<u8>>> {
            dual!(
                (self, max),
                |(s, max): (&mut Self, usize)| match s.reads.pop_front() {
                    Some(Read::Data(d)) => {
                        assert!(d.len() <= max);
                        Ok(d)
                    }
                    Some(Read::Error) => Err(Error::Timeout),
                    Some(Read::Pending) => panic!("pending script in blocking mode"),
                    None => Ok(vec![]),
                },
                |(s, max): (&mut Self, usize)| async move {
                    match s.reads.pop_front() {
                        Some(Read::Data(d)) => {
                            assert!(d.len() <= max);
                            Ok(d)
                        }
                        Some(Read::Error) => Err(Error::Timeout),
                        Some(Read::Pending) => std::future::pending().await,
                        None => Ok(vec![]),
                    }
                }
            )
        }
        fn shutdown(&mut self) -> impl MaybeFuture<Output = Result<()>> {
            dual!(self, |s: &mut Self| s.close(), |s: &mut Self| async move {
                s.close()
            })
        }
    }
    fn wire(xml: &str) -> Vec<u8> {
        format!("{}\n{xml}\n", xml.len()).into_bytes()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn run<M: MaybeFuture>(op: M, asynchronous: bool) -> M::Output {
        if asynchronous {
            futures_lite::future::block_on(op.into_future())
        } else {
            op.wait()
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn every_response_split_works_in_both_execution_modes() {
        let bytes = wire(XML);
        for asynchronous in [false, true] {
            for split in 1..bytes.len() {
                let mut client =
                    IiodClient::new(Script::new([bytes[..split].into(), bytes[split..].into()]));
                let context = run(client.context(), asynchronous).unwrap();
                assert_eq!(context.devices.len(), 3);
                assert_eq!(client.transport.commands, [b"PRINT\r\n".to_vec()]);
                assert!(client.pending.is_empty());
                assert!(!client.poisoned);
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn coalesced_data_and_surplus_are_preserved() {
        for asynchronous in [false, true] {
            let mut bytes = wire("<context name='first'/>");
            bytes.extend(wire("<context name='second'/>"));
            let mut client = IiodClient::new(Script::new([bytes]));
            assert_eq!(run(client.context(), asynchronous).unwrap().name, "first");
            assert_eq!(run(client.context(), asynchronous).unwrap().name, "second");
            assert_eq!(client.transport.commands.len(), 2);
            assert!(client.pending.is_empty());
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn remote_error_is_framed_and_session_can_continue() {
        for asynchronous in [false, true] {
            let mut client = IiodClient::new(Script::new([b"-22\n".to_vec(), wire(XML)]));
            assert!(matches!(
                run(client.context(), asynchronous),
                Err(Error::Remote(-22))
            ));
            assert!(run(client.context(), asynchronous).is_ok());
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn malformed_or_truncated_responses_poison_the_session() {
        let mut cases = vec![
            b"oops\n".to_vec(),
            b"-\n".to_vec(),
            b"+1\n".to_vec(),
            b"9999999999999999\n".to_vec(),
            format!("{}\n", MAX_XML_BYTES + 1).into_bytes(),
            b"2\na".to_vec(),
            b"1\na!".to_vec(),
            b"1\na".to_vec(),
            vec![b'1'; 65],
            vec![],
        ];
        cases.push(b"1.2\n".to_vec());
        for asynchronous in [false, true] {
            for bytes in &cases {
                let mut client = IiodClient::new(Script::new([bytes.clone()]));
                assert!(
                    run(client.context_xml(), asynchronous).is_err(),
                    "accepted {bytes:?}"
                );
                assert!(matches!(
                    run(client.context_xml(), asynchronous),
                    Err(Error::SessionPoisoned)
                ));
                assert_eq!(client.transport.commands.len(), 1);
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn transport_failure_is_not_retried() {
        for asynchronous in [false, true] {
            let mut script = Script::new([b"2\na".to_vec()]);
            script.reads.push_back(Read::Error);
            let mut client = IiodClient::new(script);
            assert!(matches!(
                run(client.context_xml(), asynchronous),
                Err(Error::Timeout)
            ));
            assert!(matches!(
                run(client.context_xml(), asynchronous),
                Err(Error::SessionPoisoned)
            ));
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn shutdown_is_lazy_terminal_retryable_and_idempotent() {
        for asynchronous in [false, true] {
            let mut script = Script::new([wire(XML)]);
            script.shutdown_failures = 1;
            let mut client = IiodClient::new(script);
            drop(client.shutdown().into_future());
            assert!(!client.closed);
            assert!(matches!(
                run(client.shutdown(), asynchronous),
                Err(Error::Timeout)
            ));
            assert!(matches!(
                run(client.context(), asynchronous),
                Err(Error::DeviceClosed)
            ));
            run(client.shutdown(), asynchronous).unwrap();
            run(client.shutdown(), asynchronous).unwrap();
            assert_eq!(client.transport.shutdown_calls, 2);
        }
    }

    async fn async_fragmentation_and_cancellation() {
        let bytes = wire(XML);
        let mut client = IiodClient::new(Script::new(bytes.chunks(1).map(Vec::from)));
        drop(client.context().into_future());
        assert!(client.transport.commands.is_empty());
        assert_eq!(client.context().await.unwrap().devices.len(), 3);

        let mut script = Script::new([b"10\nabc".to_vec()]);
        script.reads.push_back(Read::Pending);
        let mut client = IiodClient::new(script);
        let mut query = Box::pin(client.context().into_future());
        assert!(
            futures_lite::future::poll_once(query.as_mut())
                .await
                .is_none()
        );
        drop(query);
        assert!(matches!(
            client.context().await,
            Err(Error::SessionPoisoned)
        ));
        assert_eq!(client.transport.commands.len(), 1);
        client.shutdown().await.unwrap();

        let mut script = Script::new([]);
        script.reads.push_back(Read::Error);
        let mut client = IiodClient::new(script);
        assert!(matches!(client.context().await, Err(Error::Timeout)));
        assert!(matches!(
            client.context().await,
            Err(Error::SessionPoisoned)
        ));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn multi_transfer_unicode_xml_uses_byte_lengths() {
        let xml = format!("<context name='local' description='{}'/>", "é".repeat(8192));
        for asynchronous in [false, true] {
            let bytes = wire(&xml);
            let mut client = IiodClient::new(Script::new(bytes.chunks(4096).map(Vec::from)));
            let context = run(client.context(), asynchronous).unwrap();
            assert_eq!(context.description.unwrap(), "é".repeat(8192));
            assert!(client.pending.is_empty());
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn async_cancelled_request_cannot_be_reused() {
        futures_lite::future::block_on(async_fragmentation_and_cancellation());
    }
    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn wasm_fragmentation_and_cancelled_request() {
        async_fragmentation_and_cancellation().await;
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_operations_and_futures_are_send() {
        fn send<T: Send>(_: T) {}
        let mut client = IiodClient::new(Script::new([]));
        send(client.context());
        send(client.context().into_future());
        send(crate::Device::open());
        send(crate::Device::open().into_future());
    }
}
