use super::ChannelDirection;
use crate::{Error, Result};

/// An individual device or channel attribute (not the bulk attribute namespace).
#[derive(Clone, Debug)]
pub struct AttributeTarget {
    pub device: String,
    pub channel: Option<(ChannelDirection, String)>,
    pub name: String,
}

impl AttributeTarget {
    pub(crate) fn command(&self, verb: &str, length: Option<usize>) -> Result<String> {
        token(&self.device)?;
        token(&self.name)?;
        let mut command = format!("{verb} {}", self.device);
        if let Some((direction, channel)) = &self.channel {
            token(channel)?;
            let direction = match direction {
                ChannelDirection::Input => "INPUT",
                ChannelDirection::Output => "OUTPUT",
            };
            command.push_str(&format!(" {direction} {channel}"));
        }
        command.push_str(&format!(" {}", self.name));
        if let Some(length) = length {
            command.push_str(&format!(" {length}"));
        }
        command.push_str("\r\n");
        if command.len() > 1024 {
            return Err(Error::InvalidConfig("attribute command too long"));
        }
        Ok(command)
    }
}

pub(crate) fn token(value: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_:-.".contains(&b))
    {
        return Err(Error::InvalidConfig("invalid IIOD identifier"));
    }
    Ok(())
}
