//! RX controls backed by the AD936x Linux IIO attributes.
use crate::{
    Device, Error, Result, baseband,
    iiod::{AttributeTarget, ChannelDirection},
    maybe_future::dual,
};
use nusb::MaybeFuture;

/// AD936x receiver gain algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GainMode {
    Manual,
    SlowAttack,
    FastAttack,
    Hybrid,
}
impl GainMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::SlowAttack => "slow_attack",
            Self::FastAttack => "fast_attack",
            Self::Hybrid => "hybrid",
        }
    }
}
impl std::str::FromStr for GainMode {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.trim() {
            "manual" => Ok(Self::Manual),
            "slow_attack" => Ok(Self::SlowAttack),
            "fast_attack" => Ok(Self::FastAttack),
            "hybrid" => Ok(Self::Hybrid),
            _ => Err(Error::Protocol("unknown gain mode")),
        }
    }
}

/// Inclusive firmware range. The limits can depend on the current LO/FIR state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ValueRange {
    pub min: f64,
    pub step: f64,
    pub max: f64,
}
impl ValueRange {
    pub fn contains(self, value: f64) -> bool {
        value.is_finite()
            && value >= self.min
            && value <= self.max
            && ((value - self.min) / self.step - ((value - self.min) / self.step).round()).abs()
                < 1e-6
    }
    pub fn parse(text: &str) -> Result<Self> {
        let values = text
            .trim()
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .ok_or(Error::Protocol("expected firmware range"))?
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| Error::Protocol("invalid firmware range"))?;
        if values.len() != 3
            || values.iter().any(|v| !v.is_finite())
            || values[1] <= 0.0
            || values[0] > values[2]
        {
            return Err(Error::Protocol("invalid firmware range"));
        }
        Ok(Self {
            min: values[0],
            step: values[1],
            max: values[2],
        })
    }
}

/// A receiver setting. All getters query hardware, so AGC gain is live.
#[derive(Debug, Clone, Copy)]
pub enum RxAttribute {
    Frequency,
    SampleRate,
    Bandwidth,
    Gain,
    GainMode,
    Port,
    /// Baseband DC offset tracking. Boolean; no `_available` attribute.
    BbDcOffsetTracking,
    /// RF DC offset tracking. Boolean; no `_available` attribute.
    RfDcOffsetTracking,
}
impl RxAttribute {
    fn name(self) -> &'static str {
        match self {
            Self::Frequency => "frequency",
            Self::SampleRate => "sampling_frequency",
            Self::Bandwidth => "rf_bandwidth",
            Self::Gain => "hardwaregain",
            Self::GainMode => "gain_control_mode",
            Self::Port => "rf_port_select",
            Self::BbDcOffsetTracking => "bb_dc_offset_tracking_en",
            Self::RfDcOffsetTracking => "rf_dc_offset_tracking_en",
        }
    }
    pub(crate) fn target(self, device: &Device, available: bool) -> Result<AttributeTarget> {
        let phy = device
            .info()
            .devices
            .iter()
            .find(|d| d.name.as_deref() == Some("ad9361-phy"))
            .ok_or(Error::InvalidConfig("missing ad9361-phy"))?;
        let (direction, id) = if matches!(self, Self::Frequency) {
            (ChannelDirection::Output, "altvoltage0")
        } else {
            (ChannelDirection::Input, "voltage0")
        };
        let channel = phy
            .channels
            .iter()
            .find(|c| c.id == id && c.direction == direction)
            .ok_or(Error::InvalidConfig("missing RX PHY channel"))?;
        let name = format!(
            "{}{}",
            self.name(),
            if available { "_available" } else { "" }
        );
        if !channel.attributes.iter().any(|a| a.name == name) {
            return Err(Error::InvalidConfig(
                "RX attribute not advertised by firmware",
            ));
        }
        Ok(AttributeTarget {
            device: phy.id.clone(),
            channel: Some((direction, id.into())),
            name,
        })
    }
}

impl Device {
    /// Read an RX setting or its firmware-advertised available values.
    pub fn read_rx_attribute(
        &mut self,
        attr: RxAttribute,
        available: bool,
    ) -> impl MaybeFuture<Output = Result<String>> + '_ {
        dual!(
            (self, attr, available),
            |(this, attr, available): (&mut Self, RxAttribute, bool)| {
                let target = attr.target(this, available)?;
                this.client_mut()?.read_attr(&target).wait().map(|s| {
                    s.trim_matches(|c: char| c.is_whitespace() || c == '\0')
                        .to_owned()
                })
            },
            |(this, attr, available): (&mut Self, RxAttribute, bool)| async move {
                let target = attr.target(this, available)?;
                this.client_mut()?.read_attr(&target).await.map(|s| {
                    s.trim_matches(|c: char| c.is_whitespace() || c == '\0')
                        .to_owned()
                })
            }
        )
    }

    /// Read the supported setting range. Sample rate covers the FIR profiles
    /// managed by this driver; other ranges come from firmware.
    pub fn rx_range(
        &mut self,
        attr: RxAttribute,
    ) -> impl MaybeFuture<Output = Result<ValueRange>> + '_ {
        self.read_rx_attribute(attr, true).map(move |s| {
            if matches!(attr, RxAttribute::SampleRate) {
                // Firmware only reports limits for the currently loaded FIR.
                // Our setter switches profiles, including when leaving a low rate.
                s?;
                Ok(baseband::SAMPLE_RATE_RANGE)
            } else {
                ValueRange::parse(&s?)
            }
        })
    }

    /// Validated RX attribute write. Setting gain selects manual mode first.
    /// Setting sample rate loads/enables the matching RX/TX FIR profile and
    /// sets analog bandwidth to the closest supported
    /// value to the actual rate. Set bandwidth afterwards to override it.
    /// A remote failure can leave earlier settings applied; query readback after errors.
    pub fn set_rx_attribute<'a>(
        &'a mut self,
        attr: RxAttribute,
        value: &str,
    ) -> impl MaybeFuture<Output = Result<()>> + use<'a> {
        let value = value.to_owned();
        dual!(
            (self, attr, value),
            |(this, attr, value): (&mut Self, RxAttribute, String)| {
                if matches!(attr, RxAttribute::SampleRate) {
                    let rate = value
                        .parse::<u32>()
                        .map_err(|_| Error::InvalidConfig("expected integer sample rate"))?;
                    let targets = baseband::Targets::from_context(this.info())?;
                    let actual = baseband::configure(this.client_mut()?, targets, rate).wait()?;
                    let range = this.rx_range(RxAttribute::Bandwidth).wait()?;
                    let target = RxAttribute::Bandwidth.target(this, false)?;
                    return this
                        .client_mut()?
                        .write_attr(&target, &bandwidth_for_rate(actual, range).to_string())
                        .wait();
                }
                let target = attr.target(this, false)?;
                let available = if matches!(
                    attr,
                    RxAttribute::BbDcOffsetTracking | RxAttribute::RfDcOffsetTracking
                ) {
                    "0 1".to_owned()
                } else {
                    this.read_rx_attribute(attr, true).wait()?
                };
                validate_value(attr, &value, &available)?;
                if matches!(attr, RxAttribute::Gain) {
                    let mode = RxAttribute::GainMode.target(this, false)?;
                    this.client_mut()?.write_attr(&mode, "manual").wait()?;
                }
                this.client_mut()?.write_attr(&target, &value).wait()?;
                Ok(())
            },
            |(this, attr, value): (&mut Self, RxAttribute, String)| async move {
                if matches!(attr, RxAttribute::SampleRate) {
                    let rate = value
                        .parse::<u32>()
                        .map_err(|_| Error::InvalidConfig("expected integer sample rate"))?;
                    let targets = baseband::Targets::from_context(this.info())?;
                    let actual = baseband::configure(this.client_mut()?, targets, rate).await?;
                    let range = this.rx_range(RxAttribute::Bandwidth).await?;
                    let target = RxAttribute::Bandwidth.target(this, false)?;
                    return this
                        .client_mut()?
                        .write_attr(&target, &bandwidth_for_rate(actual, range).to_string())
                        .await;
                }
                let target = attr.target(this, false)?;
                let available = if matches!(
                    attr,
                    RxAttribute::BbDcOffsetTracking | RxAttribute::RfDcOffsetTracking
                ) {
                    "0 1".to_owned()
                } else {
                    this.read_rx_attribute(attr, true).await?
                };
                validate_value(attr, &value, &available)?;
                if matches!(attr, RxAttribute::Gain) {
                    let mode = RxAttribute::GainMode.target(this, false)?;
                    this.client_mut()?.write_attr(&mode, "manual").await?;
                }
                this.client_mut()?.write_attr(&target, &value).await?;
                Ok(())
            }
        )
    }
}

fn parse_bool(value: &str) -> Result<bool> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(Error::Protocol("invalid DC tracking value")),
    }
}

fn bandwidth_for_rate(rate: u32, range: ValueRange) -> f64 {
    let steps = ((f64::from(rate) - range.min) / range.step)
        .round()
        .clamp(0.0, ((range.max - range.min) / range.step).floor());
    range.min + steps * range.step
}

fn validate_value(attr: RxAttribute, value: &str, available: &str) -> Result<()> {
    match attr {
        RxAttribute::Port
        | RxAttribute::GainMode
        | RxAttribute::BbDcOffsetTracking
        | RxAttribute::RfDcOffsetTracking => {
            if !available.split_whitespace().any(|v| v == value) {
                return Err(Error::InvalidConfig("unsupported RX selection"));
            }
        }
        _ => {
            let value = value
                .parse::<f64>()
                .map_err(|_| Error::InvalidConfig("expected numeric RX setting"))?;
            if !ValueRange::parse(available)?.contains(value) {
                return Err(Error::InvalidConfig("RX setting outside firmware range"));
            }
        }
    }
    Ok(())
}

impl Device {
    /// Whether firmware advertises both RF and baseband DC tracking controls.
    pub fn dc_offset_available(&self) -> bool {
        RxAttribute::BbDcOffsetTracking.target(self, false).is_ok()
            && RxAttribute::RfDcOffsetTracking.target(self, false).is_ok()
    }

    /// Read whether both RF and baseband DC offset tracking are enabled.
    pub fn dc_offset_enabled(&mut self) -> impl MaybeFuture<Output = Result<bool>> + '_ {
        dual!(
            self,
            |this: &mut Self| {
                let bb = this
                    .read_rx_attribute(RxAttribute::BbDcOffsetTracking, false)
                    .wait()?;
                let rf = this
                    .read_rx_attribute(RxAttribute::RfDcOffsetTracking, false)
                    .wait()?;
                Ok(parse_bool(&bb)? && parse_bool(&rf)?)
            },
            |this: &mut Self| async move {
                let bb = this
                    .read_rx_attribute(RxAttribute::BbDcOffsetTracking, false)
                    .await?;
                let rf = this
                    .read_rx_attribute(RxAttribute::RfDcOffsetTracking, false)
                    .await?;
                Ok(parse_bool(&bb)? && parse_bool(&rf)?)
            }
        )
    }

    /// Enable or disable both RF and baseband hardware DC offset tracking.
    /// A remote failure may leave only one setting applied; query readback after errors.
    pub fn set_dc_offset_enabled(
        &mut self,
        enabled: bool,
    ) -> impl MaybeFuture<Output = Result<()>> + '_ {
        dual!(
            (self, enabled),
            |(this, enabled): (&mut Self, bool)| {
                let bb = RxAttribute::BbDcOffsetTracking.target(this, false)?;
                let rf = RxAttribute::RfDcOffsetTracking.target(this, false)?;
                let value = if enabled { "1" } else { "0" };
                this.client_mut()?.write_attr(&bb, value).wait()?;
                this.client_mut()?.write_attr(&rf, value).wait()
            },
            |(this, enabled): (&mut Self, bool)| async move {
                let bb = RxAttribute::BbDcOffsetTracking.target(this, false)?;
                let rf = RxAttribute::RfDcOffsetTracking.target(this, false)?;
                let value = if enabled { "1" } else { "0" };
                this.client_mut()?.write_attr(&bb, value).await?;
                this.client_mut()?.write_attr(&rf, value).await
            }
        )
    }

    /// Read the actual RX LO frequency in Hz.
    pub fn frequency_hz(&mut self) -> impl MaybeFuture<Output = Result<u64>> + '_ {
        self.read_rx_attribute(RxAttribute::Frequency, false)
            .map(|s| {
                s?.split_whitespace()
                    .next()
                    .ok_or(Error::Protocol("empty RX value"))?
                    .parse()
                    .map_err(|_| Error::Protocol("invalid RX value"))
            })
    }
    /// Tune the RX LO within the firmware-advertised range.
    pub fn set_frequency_hz(&mut self, value: u64) -> impl MaybeFuture<Output = Result<()>> + '_ {
        self.set_rx_attribute(RxAttribute::Frequency, &value.to_string())
    }
    /// Read the actual sample rate; clock rounding can differ from the requested rate.
    pub fn sample_rate_hz(&mut self) -> impl MaybeFuture<Output = Result<u32>> + '_ {
        self.read_rx_attribute(RxAttribute::SampleRate, false)
            .map(|s| {
                s?.split_whitespace()
                    .next()
                    .ok_or(Error::Protocol("empty RX value"))?
                    .parse()
                    .map_err(|_| Error::Protocol("invalid RX value"))
            })
    }
    /// Load and enable the RX/TX FIR profile for this sample rate, then match
    /// analog bandwidth to the actual readback within firmware limits. Changes
    /// the shared RX/TX clock chain; no host resampling is performed.
    pub fn set_sample_rate_hz(&mut self, value: u32) -> impl MaybeFuture<Output = Result<()>> + '_ {
        self.set_rx_attribute(RxAttribute::SampleRate, &value.to_string())
    }
    /// Read RX analog filter bandwidth in Hz.
    pub fn bandwidth_hz(&mut self) -> impl MaybeFuture<Output = Result<u32>> + '_ {
        self.read_rx_attribute(RxAttribute::Bandwidth, false)
            .map(|s| {
                s?.split_whitespace()
                    .next()
                    .ok_or(Error::Protocol("empty RX value"))?
                    .parse()
                    .map_err(|_| Error::Protocol("invalid RX value"))
            })
    }
    /// Set RX analog filter bandwidth in Hz.
    pub fn set_bandwidth_hz(&mut self, value: u32) -> impl MaybeFuture<Output = Result<()>> + '_ {
        self.set_rx_attribute(RxAttribute::Bandwidth, &value.to_string())
    }
    /// Read live RX hardware gain in dB, including while AGC is active.
    pub fn gain_db(&mut self) -> impl MaybeFuture<Output = Result<f64>> + '_ {
        self.read_rx_attribute(RxAttribute::Gain, false).map(|s| {
            s?.split_whitespace()
                .next()
                .ok_or(Error::Protocol("empty RX value"))?
                .parse()
                .map_err(|_| Error::Protocol("invalid RX value"))
        })
    }
    /// Select manual gain control, then set gain in dB.
    pub fn set_gain_db(&mut self, value: f64) -> impl MaybeFuture<Output = Result<()>> + '_ {
        self.set_rx_attribute(RxAttribute::Gain, &value.to_string())
    }
    /// Read the current receiver gain algorithm.
    pub fn gain_mode(&mut self) -> impl MaybeFuture<Output = Result<GainMode>> + '_ {
        self.read_rx_attribute(RxAttribute::GainMode, false)
            .map(|s| {
                s?.split_whitespace()
                    .next()
                    .ok_or(Error::Protocol("empty RX value"))?
                    .parse()
                    .map_err(|_| Error::Protocol("invalid RX value"))
            })
    }
    /// Select manual, slow-attack, fast-attack, or hybrid gain control.
    pub fn set_gain_mode(&mut self, value: GainMode) -> impl MaybeFuture<Output = Result<()>> + '_ {
        self.set_rx_attribute(RxAttribute::GainMode, value.as_str())
    }
    /// Read the selected internal AD936x RX port.
    pub fn rf_port(&mut self) -> impl MaybeFuture<Output = Result<String>> + '_ {
        self.read_rx_attribute(RxAttribute::Port, false)
    }
    /// Select a firmware-advertised internal RX port.
    pub fn set_rf_port(&mut self, value: String) -> impl MaybeFuture<Output = Result<()>> + '_ {
        self.set_rx_attribute(RxAttribute::Port, value.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn bandwidth_tracks_rate_within_firmware_limits() {
        let range = ValueRange::parse("[200000 1 56000000]").unwrap();
        for (rate, expected) in [
            (100_000, 200_000.0),
            (3_200_001, 3_200_001.0),
            (61_440_000, 56_000_000.0),
        ] {
            let bandwidth = bandwidth_for_rate(rate, range);
            assert_eq!(bandwidth, expected);
            assert!(range.contains(bandwidth));
        }
        let stepped = ValueRange::parse("[200000 100000 550000]").unwrap();
        assert_eq!(bandwidth_for_rate(360_000, stepped), 400_000.0);
        assert_eq!(bandwidth_for_rate(600_000, stepped), 500_000.0);
        assert!(!parse_bool("0").unwrap());
        assert!(parse_bool("1").unwrap());
        assert!(parse_bool("2").is_err());
        assert!(validate_value(RxAttribute::BbDcOffsetTracking, "true", "0 1").is_err());
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn ranges_and_settings_reject_invalid_values() {
        let range = ValueRange::parse("[-3 1 71]").unwrap();
        for value in [-3.0, 0.0, 71.0] {
            assert!(range.contains(value));
        }
        for value in [-4.0, 71.5, f64::NAN, f64::INFINITY] {
            assert!(!range.contains(value));
        }
        for bad in ["[1 0 2]", "[2 1 1]", "[NaN 1 2]", "[1 2]", "1 1 2"] {
            assert!(ValueRange::parse(bad).is_err());
        }
        assert!(
            validate_value(
                RxAttribute::Frequency,
                "2400000000.5",
                "[70000000 1 6000000000]"
            )
            .is_err()
        );
        assert!(validate_value(RxAttribute::Gain, "NaN", "[-3 1 71]").is_err());
        assert!(
            validate_value(
                RxAttribute::GainMode,
                "fast_attack",
                "manual slow_attack fast_attack"
            )
            .is_ok()
        );
        assert!(
            validate_value(RxAttribute::Port, "A_BALANCED\r\n", "A_BALANCED B_BALANCED").is_err()
        );
    }
}
