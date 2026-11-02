use std::fmt;

use crate::config::{Dpi, PageSize};
use crate::error::{Error, Result};
use crate::scan::Area;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Jpeg,
    Pdf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanSettings {
    pub dpi: Dpi,
    pub area: Area,
    pub duplex: bool,
    pub format: Format,
}

#[derive(Debug)]
pub enum SettingsError {
    BadXml(String),
    Unsupported(String),
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SettingsError::BadXml(msg) => write!(f, "bad xml: {msg}"),
            SettingsError::Unsupported(msg) => write!(f, "unsupported: {msg}"),
        }
    }
}

struct Element {
    name: String,
    text: String,
    children: Vec<Element>,
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    elements: usize,
}

impl<'a> Parser<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Parser {
            bytes,
            pos: 0,
            elements: 0,
        }
    }

    fn malformed(message: &str) -> SettingsError {
        SettingsError::BadXml(message.to_string())
    }

    fn starts_with(&self, needle: &[u8]) -> bool {
        self.bytes[self.pos..].starts_with(needle)
    }

    fn skip_past(&mut self, needle: &[u8]) -> std::result::Result<(), SettingsError> {
        let rest = &self.bytes[self.pos..];
        let found = rest
            .windows(needle.len())
            .position(|window| window == needle)
            .ok_or_else(|| Parser::malformed("unterminated markup"))?;
        self.pos += found + needle.len();
        Ok(())
    }

    fn skip_whitespace(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn skip_misc(&mut self) -> std::result::Result<(), SettingsError> {
        loop {
            self.skip_whitespace();
            if self.starts_with(b"<?") {
                self.skip_past(b"?>")?;
            } else if self.starts_with(b"<!--") {
                self.skip_past(b"-->")?;
            } else if self.starts_with(b"<!") {
                self.skip_past(b">")?;
            } else {
                return Ok(());
            }
        }
    }

    fn parse_name(&mut self) -> std::result::Result<String, SettingsError> {
        let start = self.pos;
        while self.pos < self.bytes.len()
            && !matches!(
                self.bytes[self.pos],
                b' ' | b'\t' | b'\r' | b'\n' | b'/' | b'>'
            )
        {
            self.pos += 1;
        }
        if start == self.pos {
            return Err(Parser::malformed("missing element name"));
        }
        let full = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| Parser::malformed("element name is not utf-8"))?;
        Ok(full.rsplit(':').next().unwrap_or(full).to_string())
    }

    fn parse_document(&mut self) -> std::result::Result<Element, SettingsError> {
        self.skip_misc()?;
        let root = self.parse_element(1)?;
        self.skip_misc()?;
        if self.pos != self.bytes.len() {
            return Err(Parser::malformed("content after the root element"));
        }
        Ok(root)
    }

    fn parse_element(&mut self, depth: usize) -> std::result::Result<Element, SettingsError> {
        if depth > 8 {
            return Err(Parser::malformed("xml is nested too deeply"));
        }
        self.elements += 1;
        if self.elements > 256 {
            return Err(Parser::malformed("too many elements"));
        }
        if !self.starts_with(b"<") || self.starts_with(b"</") {
            return Err(Parser::malformed("expected an element"));
        }
        self.pos += 1;
        let name = self.parse_name()?;

        loop {
            self.skip_whitespace();
            if self.pos >= self.bytes.len() {
                return Err(Parser::malformed("unterminated open tag"));
            }
            match self.bytes[self.pos] {
                b'/' => {
                    self.pos += 1;
                    if !self.starts_with(b">") {
                        return Err(Parser::malformed("bad empty element tag"));
                    }
                    self.pos += 1;
                    return Ok(Element {
                        name,
                        text: String::new(),
                        children: Vec::new(),
                    });
                }
                b'>' => {
                    self.pos += 1;
                    break;
                }
                _ => {
                    let start = self.pos;
                    while self.pos < self.bytes.len()
                        && !matches!(
                            self.bytes[self.pos],
                            b'=' | b'/' | b'>' | b' ' | b'\t' | b'\r' | b'\n'
                        )
                    {
                        self.pos += 1;
                    }
                    if start == self.pos {
                        return Err(Parser::malformed("bad attribute"));
                    }
                    self.skip_whitespace();
                    if self.pos < self.bytes.len() && self.bytes[self.pos] == b'=' {
                        self.pos += 1;
                        self.skip_whitespace();
                        if self.pos >= self.bytes.len() {
                            return Err(Parser::malformed("unterminated attribute"));
                        }
                        let quote = self.bytes[self.pos];
                        if quote == b'"' || quote == b'\'' {
                            self.pos += 1;
                            while self.pos < self.bytes.len() && self.bytes[self.pos] != quote {
                                self.pos += 1;
                            }
                            if self.pos >= self.bytes.len() {
                                return Err(Parser::malformed("unterminated attribute"));
                            }
                            self.pos += 1;
                        } else {
                            while self.pos < self.bytes.len()
                                && !matches!(
                                    self.bytes[self.pos],
                                    b'/' | b'>' | b' ' | b'\t' | b'\r' | b'\n'
                                )
                            {
                                self.pos += 1;
                            }
                        }
                    }
                }
            }
        }

        let mut element = Element {
            name,
            text: String::new(),
            children: Vec::new(),
        };
        loop {
            if self.pos >= self.bytes.len() {
                return Err(Parser::malformed("missing end tag"));
            }
            if self.bytes[self.pos] != b'<' {
                let start = self.pos;
                while self.pos < self.bytes.len() && self.bytes[self.pos] != b'<' {
                    self.pos += 1;
                }
                let decoded = decode_text(&self.bytes[start..self.pos])?;
                element.text.push_str(&decoded);
                if element.text.len() > 4096 {
                    return Err(Parser::malformed("text node is too long"));
                }
                continue;
            }
            if self.starts_with(b"</") {
                self.pos += 2;
                let close = self.parse_name()?;
                self.skip_whitespace();
                if !self.starts_with(b">") {
                    return Err(Parser::malformed("bad end tag"));
                }
                self.pos += 1;
                if close != element.name {
                    return Err(Parser::malformed("mismatched end tag"));
                }
                return Ok(element);
            }
            if self.starts_with(b"<?") {
                self.skip_past(b"?>")?;
            } else if self.starts_with(b"<!--") {
                self.skip_past(b"-->")?;
            } else if self.starts_with(b"<![CDATA[") {
                let start = self.pos + 9;
                self.skip_past(b"]]>")?;
                let end = self.pos - 3;
                element
                    .text
                    .push_str(&String::from_utf8_lossy(&self.bytes[start..end]));
                if element.text.len() > 4096 {
                    return Err(Parser::malformed("text node is too long"));
                }
            } else if self.starts_with(b"<!") {
                self.skip_past(b">")?;
            } else {
                let child = self.parse_element(depth + 1)?;
                element.children.push(child);
            }
        }
    }
}

fn decode_text(bytes: &[u8]) -> std::result::Result<String, SettingsError> {
    let mut out = String::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'&' {
            let start = index;
            while index < bytes.len() && bytes[index] != b'&' {
                index += 1;
            }
            out.push_str(&String::from_utf8_lossy(&bytes[start..index]));
            continue;
        }
        let end = bytes[index..]
            .iter()
            .position(|byte| *byte == b';')
            .ok_or_else(|| Parser::malformed("unterminated entity"))?;
        let entity = std::str::from_utf8(&bytes[index + 1..index + end])
            .map_err(|_| Parser::malformed("bad entity"))?;
        let decoded = match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            other => {
                let code = if let Some(hex) = other.strip_prefix("#x") {
                    u32::from_str_radix(hex, 16).ok()
                } else if let Some(decimal) = other.strip_prefix('#') {
                    decimal.parse().ok()
                } else {
                    None
                };
                code.and_then(char::from_u32)
                    .ok_or_else(|| Parser::malformed("unknown entity"))?
            }
        };
        out.push(decoded);
        index += end + 1;
    }
    Ok(out)
}

fn find<'a>(element: &'a Element, name: &str) -> Option<&'a Element> {
    for child in &element.children {
        if child.name == name {
            return Some(child);
        }
        if let Some(found) = find(child, name) {
            return Some(found);
        }
    }
    None
}

fn number(element: &Element) -> std::result::Result<u32, SettingsError> {
    element
        .text
        .trim()
        .parse()
        .map_err(|_| SettingsError::BadXml(format!("{} is not a number", element.name)))
}

pub fn parse_scan_settings(body: &[u8]) -> std::result::Result<ScanSettings, SettingsError> {
    let root = Parser::new(body).parse_document()?;

    let source = find(&root, "InputSource").map(|element| element.text.trim().to_ascii_lowercase());
    match source.as_deref() {
        None | Some("feeder") | Some("adf") => {}
        Some(other) => {
            return Err(SettingsError::Unsupported(format!("input source {other}")));
        }
    }

    if let Some(color) = find(&root, "ColorMode")
        && !color.text.trim().eq_ignore_ascii_case("RGB24")
    {
        return Err(SettingsError::Unsupported(format!(
            "color mode {}",
            color.text.trim()
        )));
    }

    let value = match find(&root, "XResolution").or_else(|| find(&root, "YResolution")) {
        Some(element) => number(element)?,
        None => 300,
    };
    let dpi = [Dpi::D150, Dpi::D200, Dpi::D300, Dpi::D600]
        .into_iter()
        .min_by_key(|dpi| (u32::from(dpi.as_u16()).abs_diff(value), dpi.as_u16()))
        .expect("a non-empty resolution list");

    let duplex = find(&root, "Duplex").is_some_and(|element| {
        let value = element.text.trim();
        value.eq_ignore_ascii_case("true") || value == "1"
    });

    let format = match find(&root, "DocumentFormatExt")
        .or_else(|| find(&root, "DocumentFormat"))
        .map(|element| element.text.trim().to_string())
    {
        None => Format::Jpeg,
        Some(value) if value.eq_ignore_ascii_case("image/jpeg") => Format::Jpeg,
        Some(value) if value.eq_ignore_ascii_case("application/pdf") => Format::Pdf,
        Some(value) => return Err(SettingsError::Unsupported(format!("format {value}"))),
    };

    let area = match find(&root, "ScanRegion") {
        None => PageSize::Auto.into(),
        Some(region) => match (find(region, "Width"), find(region, "Height")) {
            (None, None) => PageSize::Auto.into(),
            (Some(width), Some(height)) => Area {
                width: number(width)?.clamp(300, 2612) * 4,
                height: number(height)?.clamp(300, 4407) * 4,
                automatic: false,
            },
            _ => {
                return Err(SettingsError::BadXml(
                    "scan region needs both Width and Height".into(),
                ));
            }
        },
    };

    Ok(ScanSettings {
        dpi,
        area,
        duplex,
        format,
    })
}

pub struct Capabilities<'a> {
    pub model: &'a str,
    pub serial: &'a str,
    pub uuid: &'a str,
}

pub fn capabilities_xml(cap: &Capabilities) -> String {
    let model = escape(cap.model);
    let serial = escape(cap.serial);
    let uuid = escape(cap.uuid);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<scan:ScannerCapabilities xmlns:scan="http://schemas.hp.com/imaging/escl/2011/05/03" xmlns:pwg="http://www.pwg.org/schemas/2010/12/sm">
  <pwg:Version>2.63</pwg:Version>
  <pwg:MakeAndModel>{model}</pwg:MakeAndModel>
  <scan:SerialNumber>{serial}</scan:SerialNumber>
  <scan:UUID>{uuid}</scan:UUID>
  <scan:Adf>
    <scan:FeederCapacity>50</scan:FeederCapacity>
    <scan:AdfOptions>
      <scan:AdfOption>Duplex</scan:AdfOption>
      <scan:AdfOption>DetectPaperLoaded</scan:AdfOption>
    </scan:AdfOptions>
{simplex}{duplex}  </scan:Adf>
</scan:ScannerCapabilities>
"#,
        simplex = input_caps("AdfSimplexInputCaps"),
        duplex = input_caps("AdfDuplexInputCaps"),
    )
}

fn input_caps(tag: &str) -> String {
    format!(
        r#"    <scan:{tag}>
      <scan:MinWidth>300</scan:MinWidth>
      <scan:MaxWidth>2612</scan:MaxWidth>
      <scan:MinHeight>300</scan:MinHeight>
      <scan:MaxHeight>4407</scan:MaxHeight>
      <scan:MaxScanRegions>1</scan:MaxScanRegions>
      <scan:SettingProfiles>
        <scan:SettingProfile>
          <scan:ColorModes>
            <scan:ColorMode>RGB24</scan:ColorMode>
          </scan:ColorModes>
          <scan:DocumentFormats>
            <pwg:DocumentFormat>image/jpeg</pwg:DocumentFormat>
            <scan:DocumentFormatExt>image/jpeg</scan:DocumentFormatExt>
            <pwg:DocumentFormat>application/pdf</pwg:DocumentFormat>
            <scan:DocumentFormatExt>application/pdf</scan:DocumentFormatExt>
          </scan:DocumentFormats>
          <scan:SupportedResolutions>
            <scan:DiscreteResolutions>
              <scan:DiscreteResolution><scan:XResolution>150</scan:XResolution><scan:YResolution>150</scan:YResolution></scan:DiscreteResolution>
              <scan:DiscreteResolution><scan:XResolution>200</scan:XResolution><scan:YResolution>200</scan:YResolution></scan:DiscreteResolution>
              <scan:DiscreteResolution><scan:XResolution>300</scan:XResolution><scan:YResolution>300</scan:YResolution></scan:DiscreteResolution>
              <scan:DiscreteResolution><scan:XResolution>600</scan:XResolution><scan:YResolution>600</scan:YResolution></scan:DiscreteResolution>
            </scan:DiscreteResolutions>
          </scan:SupportedResolutions>
          <scan:ColorSpaces><scan:ColorSpace>sRGB</scan:ColorSpace></scan:ColorSpaces>
        </scan:SettingProfile>
      </scan:SettingProfiles>
      <scan:SupportedIntents>
        <scan:Intent>Document</scan:Intent>
        <scan:Intent>TextAndGraphic</scan:Intent>
        <scan:Intent>Photo</scan:Intent>
      </scan:SupportedIntents>
      <scan:MaxOpticalXResolution>600</scan:MaxOpticalXResolution>
      <scan:MaxOpticalYResolution>600</scan:MaxOpticalYResolution>
    </scan:{tag}>
"#
    )
}

pub struct JobView {
    pub id: String,
    pub age_secs: u64,
    pub images: u32,
    pub to_transfer: u32,
    pub state: &'static str,
    pub reason: &'static str,
}

pub fn status_xml(state: &str, adf: &str, jobs: &[JobView]) -> String {
    let mut body = String::new();
    for job in jobs {
        body.push_str(&format!(
            r#"    <scan:JobInfo>
      <pwg:JobUri>/eSCL/ScanJobs/{id}</pwg:JobUri>
      <pwg:JobUuid>{id}</pwg:JobUuid>
      <scan:Age>{age}</scan:Age>
      <pwg:ImagesCompleted>{images}</pwg:ImagesCompleted>
      <pwg:ImagesToTransfer>{to_transfer}</pwg:ImagesToTransfer>
      <pwg:JobState>{state}</pwg:JobState>
      <pwg:JobStateReasons><pwg:JobStateReason>{reason}</pwg:JobStateReason></pwg:JobStateReasons>
    </scan:JobInfo>
"#,
            id = escape(&job.id),
            age = job.age_secs,
            images = job.images,
            to_transfer = job.to_transfer,
            state = job.state,
            reason = job.reason,
        ));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<scan:ScannerStatus xmlns:scan="http://schemas.hp.com/imaging/escl/2011/05/03" xmlns:pwg="http://www.pwg.org/schemas/2010/12/sm">
  <pwg:Version>2.63</pwg:Version>
  <pwg:State>{state}</pwg:State>
  <scan:AdfState>{adf}</scan:AdfState>
  <scan:Jobs>
{body}  </scan:Jobs>
</scan:ScannerStatus>
"#
    )
}

/// Inserts an EXIF APP1 segment after SOI; the JPEG itself is untouched.
pub fn with_orientation_180(jpeg: &[u8]) -> Result<Vec<u8>> {
    const SEGMENT: [u8; 36] = [
        0xff, 0xe1, 0x00, 0x22, b'E', b'x', b'i', b'f', 0x00, 0x00, b'M', b'M', 0x00, 0x2a, 0x00,
        0x00, 0x00, 0x08, 0x00, 0x01, 0x01, 0x12, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00, 0x03,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    if jpeg.len() < 2 || jpeg[0] != 0xff || jpeg[1] != 0xd8 {
        return Err(Error::Protocol("not a JPEG".into()));
    }
    let mut out = Vec::with_capacity(jpeg.len() + SEGMENT.len());
    out.extend_from_slice(&jpeg[..2]);
    out.extend_from_slice(&SEGMENT);
    out.extend_from_slice(&jpeg[2..]);
    Ok(out)
}

pub fn device_uuid(model: &str, serial: &str) -> String {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    let mut hash = OFFSET;
    for byte in model.bytes().chain(*b"/").chain(serial.bytes()) {
        hash ^= u128::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    let mut bytes = hash.to_be_bytes();
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

pub fn instance_name(model: &str, serial: &str) -> String {
    format!("{model} ({serial})")
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}
