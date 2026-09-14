use super::MAX_XML_BYTES;
use crate::{Error, Result};
use roxmltree::{Document, Node, ParsingOptions};

/// Discovered context metadata and IIO devices. Attribute values may be absent.
#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    pub name: String,
    pub description: Option<String>,
    /// All XML properties, including optional firmware version fields.
    pub properties: Vec<(String, String)>,
    pub attributes: Vec<Attribute>,
    pub devices: Vec<IioDevice>,
}

/// Attribute metadata advertised in XML; no implicit READ is performed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    pub name: String,
    pub filename: Option<String>,
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IioDevice {
    pub id: String,
    pub name: Option<String>,
    pub label: Option<String>,
    pub attributes: Vec<Attribute>,
    pub debug_attributes: Vec<Attribute>,
    pub buffer_attributes: Vec<Attribute>,
    pub channels: Vec<Channel>,
    /// Explicit buffers in newer XML; empty in older firmware contexts.
    pub buffers: Vec<BufferInfo>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BufferInfo {
    pub index: u32,
    pub direction: Option<String>,
    pub attributes: Vec<Attribute>,
    pub channels: Vec<Channel>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelDirection {
    Input,
    Output,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Channel {
    pub id: String,
    pub name: Option<String>,
    pub label: Option<String>,
    pub direction: ChannelDirection,
    pub attributes: Vec<Attribute>,
    pub scan_element: Option<ScanElement>,
}

/// Advertised sample layout. RX validates this format before converting samples.
#[derive(Debug, Clone, PartialEq)]
pub struct ScanElement {
    pub index: i64,
    /// Decoded XML text, for example `le:S12/16>>0`.
    pub format: String,
    pub scale: Option<f64>,
}

impl Context {
    /// Parse legacy and current context XML, tolerating unknown optional elements.
    /// Internal DTDs are allowed; roxmltree does not fetch external resources.
    pub fn from_xml(xml: &str) -> Result<Self> {
        if xml.len() > MAX_XML_BYTES {
            return Err(Error::Xml("context exceeds size limit".into()));
        }
        let doc = Document::parse_with_options(
            xml,
            ParsingOptions {
                allow_dtd: true,
                nodes_limit: 100_000,
                ..Default::default()
            },
        )
        .map_err(|e| Error::Xml(e.to_string()))?;
        let root = doc.root_element();
        if !root.has_tag_name("context") {
            return Err(Error::Xml("expected context root".into()));
        }
        Ok(Self {
            name: required(root, "name")?,
            description: optional(root, "description"),
            properties: root
                .attributes()
                .map(|a| (a.name().into(), a.value().into()))
                .collect(),
            attributes: attributes(root, "context-attribute")?,
            devices: root
                .children()
                .filter(|n| n.has_tag_name("device"))
                .map(parse_device)
                .collect::<Result<_>>()?,
        })
    }
}

fn optional(node: Node<'_, '_>, key: &str) -> Option<String> {
    node.attribute(key).map(str::to_owned)
}
fn required(node: Node<'_, '_>, key: &str) -> Result<String> {
    optional(node, key)
        .ok_or_else(|| Error::Xml(format!("{} missing {key}", node.tag_name().name())))
}
fn attributes(node: Node<'_, '_>, tag: &str) -> Result<Vec<Attribute>> {
    node.children()
        .filter(|n| n.has_tag_name(tag))
        .map(|n| {
            Ok(Attribute {
                name: required(n, "name")?,
                filename: optional(n, "filename"),
                value: optional(n, "value"),
            })
        })
        .collect()
}
fn channels(node: Node<'_, '_>) -> Result<Vec<Channel>> {
    node.children()
        .filter(|n| n.has_tag_name("channel"))
        .map(parse_channel)
        .collect()
}
fn parse_device(n: Node<'_, '_>) -> Result<IioDevice> {
    Ok(IioDevice {
        id: required(n, "id")?,
        name: optional(n, "name"),
        label: optional(n, "label"),
        attributes: attributes(n, "attribute")?,
        debug_attributes: attributes(n, "debug-attribute")?,
        buffer_attributes: attributes(n, "buffer-attribute")?,
        channels: channels(n)?,
        buffers: n
            .children()
            .filter(|n| n.has_tag_name("buffer"))
            .map(|b| {
                Ok(BufferInfo {
                    index: required(b, "index")?
                        .parse()
                        .map_err(|_| Error::Xml("invalid buffer index".into()))?,
                    direction: optional(b, "direction"),
                    attributes: attributes(b, "attribute")?,
                    channels: channels(b)?,
                })
            })
            .collect::<Result<_>>()?,
    })
}
fn parse_channel(n: Node<'_, '_>) -> Result<Channel> {
    let direction = match n.attribute("type") {
        Some("input") => ChannelDirection::Input,
        Some("output") => ChannelDirection::Output,
        _ => return Err(Error::Xml("channel type must be input or output".into())),
    };
    let scan_element = n
        .children()
        .find(|n| n.has_tag_name("scan-element"))
        .map(|s| {
            Ok::<_, Error>(ScanElement {
                index: required(s, "index")?
                    .parse()
                    .map_err(|_| Error::Xml("invalid scan index".into()))?,
                format: required(s, "format")?,
                scale: s
                    .attribute("scale")
                    .map(|s| {
                        s.parse()
                            .map_err(|_| Error::Xml("invalid scan scale".into()))
                    })
                    .transpose()?,
            })
        })
        .transpose()?;
    Ok(Channel {
        id: required(n, "id")?,
        name: optional(n, "name"),
        label: optional(n, "label"),
        direction,
        attributes: attributes(n, "attribute")?,
        scan_element,
    })
}
