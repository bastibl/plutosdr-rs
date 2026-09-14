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
    /// Deliver a length-delimited attribute payload as one complete write.
    fn write_data(&mut self, data: &[u8]) -> impl MaybeFuture<Output = Result<()>>;
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

    pub(crate) fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }
    pub(crate) fn usable(&self) -> bool {
        !self.closed && !self.poisoned
    }
    pub fn read_attr(
        &mut self,
        target: &super::AttributeTarget,
    ) -> impl MaybeFuture<Output = Result<String>> + '_ {
        dual!(
            (self, target.command("READ", None)),
            |(this, command): (&mut Self, Result<String>)| {
                let command = command?;
                this.begin()?;
                this.transport.write_command(command.as_bytes()).wait()?;
                let length = this.integer().wait()?;
                if length < 0 {
                    this.poisoned = false;
                    return Err(Error::Remote(length));
                }
                if length as usize > 65536 {
                    return Err(Error::Protocol("attribute exceeds size limit"));
                }
                let bytes = this.exact(length as usize + 1).wait()?;
                if bytes.last() != Some(&b'\n') {
                    return Err(Error::Protocol("missing attribute trailer"));
                }
                this.poisoned = false;
                String::from_utf8(bytes[..bytes.len() - 1].to_vec())
                    .map_err(|_| Error::Protocol("attribute is not UTF-8"))
            },
            |(this, command): (&mut Self, Result<String>)| async move {
                let command = command?;
                this.begin()?;
                this.transport.write_command(command.as_bytes()).await?;
                let length = this.integer().await?;
                if length < 0 {
                    this.poisoned = false;
                    return Err(Error::Remote(length));
                }
                if length as usize > 65536 {
                    return Err(Error::Protocol("attribute exceeds size limit"));
                }
                let bytes = this.exact(length as usize + 1).await?;
                if bytes.last() != Some(&b'\n') {
                    return Err(Error::Protocol("missing attribute trailer"));
                }
                this.poisoned = false;
                String::from_utf8(bytes[..bytes.len() - 1].to_vec())
                    .map_err(|_| Error::Protocol("attribute is not UTF-8"))
            }
        )
    }
    pub fn write_attr(
        &mut self,
        target: &super::AttributeTarget,
        value: &str,
    ) -> impl MaybeFuture<Output = Result<()>> + '_ {
        dual!(
            (
                self,
                target.command("WRITE", Some(value.len())),
                value.as_bytes().to_vec()
            ),
            |(this, command, value): (&mut Self, Result<String>, Vec<u8>)| {
                let command = command?;
                if value.is_empty() || value.len() > 65536 {
                    return Err(Error::InvalidConfig("invalid attribute length"));
                }
                this.begin()?;
                this.transport.write_command(command.as_bytes()).wait()?;
                this.transport.write_data(&value).wait()?;
                let status = this.integer().wait()?;
                this.poisoned = false;
                if status < 0 {
                    return Err(Error::Remote(status));
                }
                if status as usize != value.len() {
                    return Err(Error::Protocol("short attribute write"));
                }
                Ok(())
            },
            |(this, command, value): (&mut Self, Result<String>, Vec<u8>)| async move {
                let command = command?;
                if value.is_empty() || value.len() > 65536 {
                    return Err(Error::InvalidConfig("invalid attribute length"));
                }
                this.begin()?;
                this.transport.write_command(command.as_bytes()).await?;
                this.transport.write_data(&value).await?;
                let status = this.integer().await?;
                this.poisoned = false;
                if status < 0 {
                    return Err(Error::Remote(status));
                }
                if status as usize != value.len() {
                    return Err(Error::Protocol("short attribute write"));
                }
                Ok(())
            }
        )
    }
    pub(crate) fn status(&mut self, command: String) -> impl MaybeFuture<Output = Result<()>> + '_ {
        dual!(
            (self, command),
            |(this, command): (&mut Self, String)| {
                this.begin()?;
                this.transport.write_command(command.as_bytes()).wait()?;
                let status = this.integer().wait()?;
                this.poisoned = false;
                if status < 0 {
                    Err(Error::Remote(status))
                } else {
                    Ok(())
                }
            },
            |(this, command): (&mut Self, String)| async move {
                this.begin()?;
                this.transport.write_command(command.as_bytes()).await?;
                let status = this.integer().await?;
                this.poisoned = false;
                if status < 0 {
                    Err(Error::Remote(status))
                } else {
                    Ok(())
                }
            }
        )
    }
    fn integer(&mut self) -> impl MaybeFuture<Output = Result<i32>> + '_ {
        dual!(
            self,
            |this: &mut Self| {
                let mut line = Vec::new();
                loop {
                    let byte = this.exact(1).wait()?[0];
                    if byte == b'\n' {
                        break;
                    }
                    if line.len() >= MAX_LINE_BYTES {
                        return Err(Error::Protocol("response line too long"));
                    }
                    line.push(byte);
                }
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                let text =
                    std::str::from_utf8(&line).map_err(|_| Error::Protocol("invalid integer"))?;
                let digits = text.strip_prefix('-').unwrap_or(text);
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(Error::Protocol("invalid integer"));
                }
                text.parse()
                    .map_err(|_| Error::Protocol("integer overflow"))
            },
            |this: &mut Self| async move {
                let mut line = Vec::new();
                loop {
                    let byte = this.exact(1).await?[0];
                    if byte == b'\n' {
                        break;
                    }
                    if line.len() >= MAX_LINE_BYTES {
                        return Err(Error::Protocol("response line too long"));
                    }
                    line.push(byte);
                }
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                let text =
                    std::str::from_utf8(&line).map_err(|_| Error::Protocol("invalid integer"))?;
                let digits = text.strip_prefix('-').unwrap_or(text);
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(Error::Protocol("invalid integer"));
                }
                text.parse()
                    .map_err(|_| Error::Protocol("integer overflow"))
            }
        )
    }
    fn exact(&mut self, length: usize) -> impl MaybeFuture<Output = Result<Vec<u8>>> + '_ {
        dual!(
            (self, length),
            |(this, length): (&mut Self, usize)| {
                let mut bytes = Vec::with_capacity(length);
                while bytes.len() < length {
                    let count = this.pending.len().min(length - bytes.len());
                    bytes.extend(this.pending.drain(..count));
                    if bytes.len() == length {
                        break;
                    }
                    let max = (length - bytes.len()).min(256 * 1024);
                    let chunk = this.transport.read(max).wait()?;
                    if chunk.is_empty() || chunk.len() > max {
                        return Err(Error::Protocol("invalid transport read length"));
                    }
                    bytes.extend(chunk);
                }
                Ok(bytes)
            },
            |(this, length): (&mut Self, usize)| async move {
                let mut bytes = Vec::with_capacity(length);
                while bytes.len() < length {
                    let count = this.pending.len().min(length - bytes.len());
                    bytes.extend(this.pending.drain(..count));
                    if bytes.len() == length {
                        break;
                    }
                    let max = (length - bytes.len()).min(256 * 1024);
                    let chunk = this.transport.read(max).await?;
                    if chunk.is_empty() || chunk.len() > max {
                        return Err(Error::Protocol("invalid transport read length"));
                    }
                    bytes.extend(chunk);
                }
                Ok(bytes)
            }
        )
    }
    pub fn read_buffer(
        &mut self,
        device: &str,
        length: usize,
        mask: &str,
    ) -> impl MaybeFuture<Output = Result<Vec<u8>>> + '_ {
        dual!(
            (self, device.to_owned(), length, mask.to_owned()),
            |(this, device, length, mask): (&mut Self, String, usize, String)| {
                super::attribute::token(&device)?;
                if length == 0
                    || length > 16 * 1024 * 1024
                    || mask.is_empty()
                    || mask.len() > 128
                    || !mask.len().is_multiple_of(8)
                    || !mask.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err(Error::InvalidConfig("invalid RX buffer request"));
                }
                this.begin()?;
                this.transport
                    .write_command(format!("READBUF {device} {length}\r\n").as_bytes())
                    .wait()?;
                let mut data = Vec::with_capacity(length);
                let mut first = true;
                while data.len() < length {
                    let count = this.integer().wait()?;
                    if count < 0 {
                        this.poisoned = false;
                        return Err(Error::Remote(count));
                    }
                    if count == 0 {
                        break;
                    }
                    let count = count as usize;
                    if count > length - data.len() {
                        return Err(Error::Protocol("RX chunk exceeds requested length"));
                    }
                    if first {
                        let actual = this.exact(mask.len() + 1).wait()?;
                        if actual.last() != Some(&b'\n')
                            || !actual[..mask.len()].eq_ignore_ascii_case(mask.as_bytes())
                        {
                            return Err(Error::Protocol("RX channel mask changed"));
                        }
                        first = false;
                    }
                    data.extend(this.exact(count).wait()?);
                }
                this.poisoned = false;
                Ok(data)
            },
            |(this, device, length, mask): (&mut Self, String, usize, String)| async move {
                super::attribute::token(&device)?;
                if length == 0
                    || length > 16 * 1024 * 1024
                    || mask.is_empty()
                    || mask.len() > 128
                    || !mask.len().is_multiple_of(8)
                    || !mask.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err(Error::InvalidConfig("invalid RX buffer request"));
                }
                this.begin()?;
                this.transport
                    .write_command(format!("READBUF {device} {length}\r\n").as_bytes())
                    .await?;
                let mut data = Vec::with_capacity(length);
                let mut first = true;
                while data.len() < length {
                    let count = this.integer().await?;
                    if count < 0 {
                        this.poisoned = false;
                        return Err(Error::Remote(count));
                    }
                    if count == 0 {
                        break;
                    }
                    let count = count as usize;
                    if count > length - data.len() {
                        return Err(Error::Protocol("RX chunk exceeds requested length"));
                    }
                    if first {
                        let actual = this.exact(mask.len() + 1).await?;
                        if actual.last() != Some(&b'\n')
                            || !actual[..mask.len()].eq_ignore_ascii_case(mask.as_bytes())
                        {
                            return Err(Error::Protocol("RX channel mask changed"));
                        }
                        first = false;
                    }
                    data.extend(this.exact(count).await?);
                }
                this.poisoned = false;
                Ok(data)
            }
        )
    }
    pub(crate) fn begin(&mut self) -> Result<()> {
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
        fn write_data(&mut self, data: &[u8]) -> impl MaybeFuture<Output = Result<()>> {
            self.write_command(data)
        }
        fn read(&mut self, max: usize) -> impl MaybeFuture<Output = Result<Vec<u8>>> {
            dual!(
                (self, max),
                |(s, max): (&mut Self, usize)| match s.reads.pop_front() {
                    Some(Read::Data(mut d)) => {
                        if d.len() > max {
                            s.reads.push_front(Read::Data(d.split_off(max)));
                        }
                        Ok(d)
                    }
                    Some(Read::Error) => Err(Error::Timeout),
                    Some(Read::Pending) => panic!("pending script in blocking mode"),
                    None => Ok(vec![]),
                },
                |(s, max): (&mut Self, usize)| async move {
                    match s.reads.pop_front() {
                        Some(Read::Data(mut d)) => {
                            if d.len() > max {
                                s.reads.push_front(Read::Data(d.split_off(max)));
                            }
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
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn attribute_counts_nul_trailers_and_raw_writes() {
        let target = super::super::AttributeTarget {
            device: "iio:device4".into(),
            channel: Some((super::super::ChannelDirection::Input, "voltage0".into())),
            name: "hardwaregain".into(),
        };
        for asynchronous in [false, true] {
            let mut client = IiodClient::new(Script::new([b"3\n30\0\n2\n-22\n".to_vec()]));
            assert_eq!(
                run(client.read_attr(&target), asynchronous).unwrap(),
                "30\0"
            );
            run(client.write_attr(&target, "30"), asynchronous).unwrap();
            assert!(matches!(
                run(client.write_attr(&target, "99"), asynchronous),
                Err(Error::Remote(-22))
            ));
            assert!(client.usable());
            assert_eq!(
                client.transport.commands,
                [
                    b"READ iio:device4 INPUT voltage0 hardwaregain\r\n".to_vec(),
                    b"WRITE iio:device4 INPUT voltage0 hardwaregain 2\r\n".to_vec(),
                    b"30".to_vec(),
                    b"WRITE iio:device4 INPUT voltage0 hardwaregain 2\r\n".to_vec(),
                    b"99".to_vec()
                ]
            );
            let bad = super::super::AttributeTarget {
                name: "gain\r\nPRINT".into(),
                ..target.clone()
            };
            assert!(run(client.read_attr(&bad), asynchronous).is_err());
            assert!(client.usable());
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn rx_fragmentation_multiple_chunks_and_exact_length_has_no_trailer() {
        let bytes = b"4\n00000003\n\x00\x80\xff\x7f4\n\x0a\x0d\x00\x00";
        for asynchronous in [false, true] {
            for split in 1..bytes.len() {
                let mut client = IiodClient::new(Script::new([
                    bytes[..split].to_vec(),
                    bytes[split..].to_vec(),
                ]));
                assert_eq!(
                    run(
                        client.read_buffer("iio:device7", 8, "00000003"),
                        asynchronous
                    )
                    .unwrap(),
                    [0, 128, 255, 127, 10, 13, 0, 0]
                );
                assert!(client.usable());
                assert!(client.pending.is_empty());
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn rx_short_remote_errors_and_malformed_frames() {
        for asynchronous in [false, true] {
            for (wire, length) in [
                (b"0\n".as_slice(), 0),
                (b"4\n00000003\nABCD0\n".as_slice(), 4),
            ] {
                let mut client = IiodClient::new(Script::new([wire.to_vec()]));
                assert_eq!(
                    run(
                        client.read_buffer("iio:device7", 8, "00000003"),
                        asynchronous
                    )
                    .unwrap()
                    .len(),
                    length
                );
                assert!(client.usable());
            }
            let mut client = IiodClient::new(Script::new([b"4\n00000003\nABCD-110\n".to_vec()]));
            assert!(matches!(
                run(
                    client.read_buffer("iio:device7", 8, "00000003"),
                    asynchronous
                ),
                Err(Error::Remote(-110))
            ));
            assert!(client.usable());
            for wire in [
                b"9\n".as_slice(),
                b"4\n00000007\nABCD",
                b"4\n00000003!ABCD",
                b"4\n00000003\nAB",
                b"9999999999999999\n",
                b"junk\n",
            ] {
                let mut client = IiodClient::new(Script::new([wire.to_vec()]));
                assert!(
                    run(
                        client.read_buffer("iio:device7", 8, "00000003"),
                        asynchronous
                    )
                    .is_err()
                );
                assert!(!client.usable());
            }
        }
    }

    async fn cancel_rx_after_header() {
        let mut script = Script::new([b"4\n00000003\nAB".to_vec()]);
        script.reads.push_back(Read::Pending);
        let mut client = IiodClient::new(script);
        let mut read = Box::pin(
            client
                .read_buffer("iio:device7", 4, "00000003")
                .into_future(),
        );
        assert!(
            futures_lite::future::poll_once(read.as_mut())
                .await
                .is_none()
        );
        drop(read);
        assert!(matches!(
            client.read_buffer("iio:device7", 4, "00000003").await,
            Err(Error::SessionPoisoned)
        ));
        client.shutdown().await.unwrap();
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn cancelled_rx_cannot_reuse_protocol_session() {
        futures_lite::future::block_on(cancel_rx_after_header());
    }
    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn wasm_cancelled_rx_cannot_reuse_protocol_session() {
        cancel_rx_after_header().await;
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn noncyclic_buffer_lifecycle_and_rejected_open_leave_framed_session_usable() {
        for asynchronous in [false, true] {
            let mut client = IiodClient::new(Script::new([b"0\n0\n-16\n".to_vec()]));
            run(
                client.open_buffer("iio:device7", 4096, "00000003"),
                asynchronous,
            )
            .unwrap();
            run(client.close_buffer("iio:device7"), asynchronous).unwrap();
            assert!(matches!(
                run(
                    client.open_buffer("iio:device7", 4096, "00000003"),
                    asynchronous
                ),
                Err(Error::Remote(-16))
            ));
            assert!(client.usable());
            assert_eq!(
                client.transport.commands,
                [
                    b"OPEN iio:device7 4096 00000003\r\n".to_vec(),
                    b"CLOSE iio:device7\r\n".to_vec(),
                    b"OPEN iio:device7 4096 00000003\r\n".to_vec()
                ]
            );
            assert!(
                run(
                    client.open_buffer("iio:device7", 0, "00000003"),
                    asynchronous
                )
                .is_err()
            );
            assert!(run(client.open_buffer("iio:device7", 4096, "3"), asynchronous).is_err());
            assert!(client.usable());
            assert_eq!(client.transport.commands.len(), 3);
        }
    }
}
