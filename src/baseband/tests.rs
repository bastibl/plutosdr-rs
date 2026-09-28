use super::*;
use std::collections::VecDeque;

fn context(legacy: bool) -> Context {
    let enable = if legacy {
        r#"<channel id="out" type="input"><attribute name="voltage_filter_fir_en"/></channel>"#
    } else {
        r#"<attribute name="in_out_voltage_filter_fir_en"/>"#
    };
    Context::from_xml(&format!(
        r#"<context name="local">
        <device id="phy" name="ad9361-phy">
            {enable}
            <attribute name="filter_fir_config"/>
            <channel id="voltage0" type="input"><attribute name="sampling_frequency"/></channel>
            <channel id="voltage0" type="output"><attribute name="sampling_frequency"/></channel>
        </device>
        <device id="dma" name="cf-ad9361-lpc">
            <channel id="voltage0" type="input"><attribute name="sampling_frequency"/></channel>
        </device>
    </context>"#
    ))
    .unwrap()
}

struct Exchange {
    command: String,
    data: Option<String>,
    reply: Vec<u8>,
}
fn read(target: &str, value: &str) -> Exchange {
    Exchange {
        command: format!("READ {target}\r\n"),
        data: None,
        reply: format!("{}\n{value}\n", value.len()).into_bytes(),
    }
}
fn write(target: &str, value: &str) -> Exchange {
    Exchange {
        command: format!("WRITE {target} {}\r\n", value.len()),
        data: Some(value.into()),
        reply: format!("{}\n", value.len()).into_bytes(),
    }
}
struct Script {
    exchanges: VecDeque<Exchange>,
    data: Option<String>,
    reply: VecDeque<u8>,
}
impl Script {
    fn new(exchanges: Vec<Exchange>) -> Self {
        Self {
            exchanges: exchanges.into(),
            data: None,
            reply: VecDeque::new(),
        }
    }
    fn command(&mut self, bytes: &[u8]) -> Result<()> {
        assert!(self.data.is_none());
        assert!(self.reply.is_empty());
        let exchange = self.exchanges.pop_front().expect("unexpected USB command");
        assert_eq!(bytes, exchange.command.as_bytes());
        self.data = exchange.data;
        self.reply = exchange.reply.into();
        Ok(())
    }
    fn data(&mut self, bytes: &[u8]) -> Result<()> {
        assert_eq!(
            bytes,
            self.data.take().expect("unexpected payload").as_bytes()
        );
        Ok(())
    }
    fn read(&mut self, max: usize) -> Result<Vec<u8>> {
        assert!(self.data.is_none());
        Ok(self.reply.drain(..max.min(self.reply.len())).collect())
    }
    fn done(&self) {
        assert!(self.exchanges.is_empty());
        assert!(self.data.is_none());
        assert!(self.reply.is_empty());
    }
}
impl Transport for Script {
    fn write_command(&mut self, data: &[u8]) -> impl MaybeFuture<Output = Result<()>> {
        dual!(
            (self, data),
            |(s, d): (&mut Self, &[u8])| s.command(d),
            |(s, d): (&mut Self, &[u8])| async move { s.command(d) }
        )
    }
    fn write_data(&mut self, data: &[u8]) -> impl MaybeFuture<Output = Result<()>> {
        dual!(
            (self, data),
            |(s, d): (&mut Self, &[u8])| s.data(d),
            |(s, d): (&mut Self, &[u8])| async move { s.data(d) }
        )
    }
    fn read(&mut self, max: usize) -> impl MaybeFuture<Output = Result<Vec<u8>>> {
        dual!(
            (self, max),
            |(s, m): (&mut Self, usize)| s.read(m),
            |(s, m): (&mut Self, usize)| async move { s.read(m) }
        )
    }
    fn shutdown(&mut self) -> impl MaybeFuture<Output = Result<()>> {
        dual!((), |()| Ok(()), |()| async { Ok(()) })
    }
}
const RATE: &str = "phy OUTPUT voltage0 sampling_frequency";
const RX_RATE: &str = "phy INPUT voltage0 sampling_frequency";
const DMA_RATE: &str = "dma INPUT voltage0 sampling_frequency";
const ENABLE: &str = "phy in_out_voltage_filter_fir_en";
const CONFIG: &str = "phy filter_fir_config";

fn transitions() -> Vec<(u32, Vec<Exchange>)> {
    [
        520_834, 1_000_000, 3_200_000, 20_000_000, 32_000_000, 48_000_000, 61_440_000,
    ]
    .into_iter()
    .map(|rate| {
        let actual = rate - 1;
        (
            rate,
            vec![
                write(RATE, "3000000"),
                write(ENABLE, "0"),
                write(CONFIG, &Filter::for_rate(rate).unwrap().config()),
                write(ENABLE, "1"),
                write(RATE, &rate.to_string()),
                read(RX_RATE, &actual.to_string()),
                write(DMA_RATE, &actual.to_string()),
            ],
        )
    })
    .collect()
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn blocking_clock_transitions() {
    for (rate, exchanges) in transitions() {
        let mut client = IiodClient::new(Script::new(exchanges));
        let actual = configure(
            &mut client,
            Targets::from_context(&context(false)).unwrap(),
            rate,
        )
        .wait()
        .unwrap();
        assert!(actual.abs_diff(rate) < 5);
        client.transport_mut().done();
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn async_clock_transitions_native() {
    futures_lite::future::block_on(async_clock_transitions());
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
async fn async_clock_transitions() {
    for (rate, exchanges) in transitions() {
        let mut client = IiodClient::new(Script::new(exchanges));
        let actual = configure(
            &mut client,
            Targets::from_context(&context(false)).unwrap(),
            rate,
        )
        .await
        .unwrap();
        assert!(actual.abs_diff(rate) < 5);
        client.transport_mut().done();
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn profiles_and_legacy_attribute_layout() {
    for (rate, decimation, taps) in [
        (520_834, 4, 128),
        (20_000_000, 4, 128),
        (20_000_001, 2, 128),
        (40_000_000, 2, 128),
        (40_000_001, 2, 96),
        (53_333_333, 2, 96),
        (53_333_334, 2, 64),
        (61_440_000, 2, 64),
    ] {
        let filter = Filter::for_rate(rate).unwrap();
        assert_eq!(filter.decimation, decimation);
        assert_eq!(filter.taps.len(), taps);
        let config = filter.config();
        assert!(config.starts_with(&format!(
            "RX 3 GAIN -6 DEC {decimation}\nTX 3 GAIN 0 INT {decimation}\n"
        )));
        assert_eq!(config.lines().count(), taps + 3);
    }
    for rate in [0, 520_833, 61_440_001, u32::MAX] {
        assert!(Filter::for_rate(rate).is_err());
    }
    let targets = Targets::from_context(&context(true)).unwrap();
    assert_eq!(targets.fir_enable.name, "voltage_filter_fir_en");
    assert_eq!(
        targets.fir_enable.channel,
        Some((ChannelDirection::Input, "out".into()))
    );
    let mut missing = context(false);
    missing.devices[0]
        .attributes
        .retain(|a| a.name != "filter_fir_config");
    assert!(Targets::from_context(&missing).is_err());
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn failures_do_not_continue_clock_changes() {
    let mut failed = write(CONFIG, &Filter::for_rate(3_200_000).unwrap().config());
    failed.reply = b"-22\n".to_vec();
    let mut client = IiodClient::new(Script::new(vec![
        write(RATE, "3000000"),
        write(ENABLE, "0"),
        failed,
    ]));
    assert!(matches!(
        configure(
            &mut client,
            Targets::from_context(&context(false)).unwrap(),
            3_200_000
        )
        .wait(),
        Err(Error::Remote(-22))
    ));
    client.transport_mut().done();
    let mut client = IiodClient::new(Script::new(vec![]));
    assert!(matches!(
        configure(
            &mut client,
            Targets::from_context(&context(false)).unwrap(),
            1
        )
        .wait(),
        Err(Error::InvalidConfig(_))
    ));
    client.transport_mut().done();
}
