//! AD936x FIR loading and safe sample-clock transitions.
use crate::{
    Error, Result, ValueRange,
    iiod::{AttributeTarget, ChannelDirection, Context, IiodClient, Transport},
    maybe_future::dual,
};
use nusb::MaybeFuture;
use std::fmt::Write;

mod coefficients;
use coefficients::*;

// Minimum ADC clock / (three halfband stages * FIR decimation).
// Round upward: the truncated firmware minimum is not always writable.
pub(crate) const SAMPLE_RATE_RANGE: ValueRange = ValueRange {
    min: 520_834.0,
    step: 1.0,
    max: 61_440_000.0,
};
struct Filter {
    decimation: u32,
    taps: &'static [i16],
}
impl Filter {
    fn for_rate(rate: u32) -> Result<Self> {
        if !SAMPLE_RATE_RANGE.contains(f64::from(rate)) {
            return Err(Error::InvalidConfig(
                "sample rate outside FIR-supported range",
            ));
        }
        // Keep the FIR input clock within 80 MHz at x4. Shorter x2
        // profiles fit the available multiply cycles at higher sample rates.
        let (decimation, taps): (_, &[_]) = match rate {
            ..=20_000_000 => (4, &FIR_128_4),
            20_000_001..=40_000_000 => (2, &FIR_128_2),
            40_000_001..=53_333_333 => (2, &FIR_96_2),
            _ => (2, &FIR_64_2),
        };
        Ok(Self { decimation, taps })
    }
    fn config(&self) -> String {
        let mut text = format!(
            "RX 3 GAIN -6 DEC {}\nTX 3 GAIN 0 INT {}\n",
            self.decimation, self.decimation
        );
        for tap in self.taps {
            writeln!(text, "{tap},{tap}").unwrap();
        }
        text.push('\n');
        text
    }
}

pub(crate) struct Targets {
    rate: AttributeTarget,
    rx_rate: AttributeTarget,
    fir_enable: AttributeTarget,
    fir_config: AttributeTarget,
    dma_rate: Option<AttributeTarget>,
}
impl Targets {
    pub(crate) fn from_context(context: &Context) -> Result<Self> {
        let phy = context
            .devices
            .iter()
            .find(|d| d.name.as_deref() == Some("ad9361-phy"))
            .ok_or(Error::InvalidConfig("missing ad9361-phy"))?;
        let device_attr = |name: &str| {
            phy.attributes
                .iter()
                .any(|a| a.name == name)
                .then(|| AttributeTarget {
                    device: phy.id.clone(),
                    channel: None,
                    name: name.into(),
                })
        };
        let channel_attr = |direction, id: &str, name: &str| {
            phy.channels
                .iter()
                .find(|c| c.direction == direction && c.id == id)
                .filter(|c| c.attributes.iter().any(|a| a.name == name))
                .map(|_| AttributeTarget {
                    device: phy.id.clone(),
                    channel: Some((direction, id.into())),
                    name: name.into(),
                })
        };
        let missing = || Error::InvalidConfig("firmware missing sample-rate/FIR controls");
        let fir_enable = device_attr("in_out_voltage_filter_fir_en")
            .or_else(|| channel_attr(ChannelDirection::Input, "out", "voltage_filter_fir_en"))
            .ok_or_else(missing)?;
        let dma_rate = context
            .devices
            .iter()
            .find(|d| d.name.as_deref() == Some("cf-ad9361-lpc"))
            .and_then(|d| {
                d.channels
                    .iter()
                    .find(|c| c.id == "voltage0" && c.direction == ChannelDirection::Input)
                    .filter(|c| c.attributes.iter().any(|a| a.name == "sampling_frequency"))
                    .map(|_| AttributeTarget {
                        device: d.id.clone(),
                        channel: Some((ChannelDirection::Input, "voltage0".into())),
                        name: "sampling_frequency".into(),
                    })
            });
        Ok(Self {
            rate: channel_attr(ChannelDirection::Output, "voltage0", "sampling_frequency")
                .ok_or_else(missing)?,
            rx_rate: channel_attr(ChannelDirection::Input, "voltage0", "sampling_frequency")
                .ok_or_else(missing)?,
            fir_enable,
            fir_config: device_attr("filter_fir_config").ok_or_else(missing)?,
            dma_rate,
        })
    }
}

fn number(text: &str) -> Result<u32> {
    text.trim_matches(|c: char| c.is_whitespace() || c == '\0')
        .parse()
        .map_err(|_| Error::Protocol("invalid sample-rate/FIR readback"))
}

/// Configure both FIRs because RX and TX share the AD936x clock chain.
/// Errors preserve their original cause; earlier successful writes stay applied.
pub(crate) fn configure<T: Transport>(
    client: &mut IiodClient<T>,
    targets: Targets,
    rate: u32,
) -> impl MaybeFuture<Output = Result<u32>> + '_ {
    // Use one intermediate clock rate for every transition. At 3 MS/s,
    // bypass and either FIR decimation are valid, with room for 128 taps.
    // This also avoids needing to inspect the previous filter's clock chain.
    dual!(
        (client, targets, rate),
        |(client, targets, rate): (&mut IiodClient<T>, Targets, u32)| {
            let filter = Filter::for_rate(rate)?;
            for (target, value) in [
                (&targets.rate, "3000000".to_owned()),
                (&targets.fir_enable, "0".to_owned()),
                (&targets.fir_config, filter.config()),
                (&targets.fir_enable, "1".to_owned()),
                (&targets.rate, rate.to_string()),
            ] {
                client.write_attr(target, &value).wait()?;
            }
            let actual = number(&client.read_attr(&targets.rx_rate).wait()?)?;
            if let Some(target) = targets.dma_rate {
                // Reset any FPGA x8 decimation left by another application.
                client.write_attr(&target, &actual.to_string()).wait()?;
            }
            Ok(actual)
        },
        |(client, targets, rate): (&mut IiodClient<T>, Targets, u32)| async move {
            let filter = Filter::for_rate(rate)?;
            for (target, value) in [
                (&targets.rate, "3000000".to_owned()),
                (&targets.fir_enable, "0".to_owned()),
                (&targets.fir_config, filter.config()),
                (&targets.fir_enable, "1".to_owned()),
                (&targets.rate, rate.to_string()),
            ] {
                client.write_attr(target, &value).await?;
            }
            let actual = number(&client.read_attr(&targets.rx_rate).await?)?;
            if let Some(target) = targets.dma_rate {
                client.write_attr(&target, &actual.to_string()).await?;
            }
            Ok(actual)
        }
    )
}

#[cfg(test)]
mod tests;
