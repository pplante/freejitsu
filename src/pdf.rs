use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, ErrorKind, Write};
use std::path::PathBuf;

use crate::error::{Error, Result};

const SOF_MARKERS: [u8; 13] = [
    0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf,
];

/// Width, height and component count from the JPEG SOF marker.
pub fn jpeg_size(jpeg: &[u8]) -> Result<(u32, u32, u8)> {
    if jpeg.len() < 4 || jpeg[0] != 0xff || jpeg[1] != 0xd8 {
        return Err(Error::Pdf("not a JPEG".into()));
    }
    let mut index = 2;
    while index + 1 < jpeg.len() {
        if jpeg[index] != 0xff {
            index += 1;
            continue;
        }
        let mut marker_at = index + 1;
        while marker_at < jpeg.len() && jpeg[marker_at] == 0xff {
            marker_at += 1;
        }
        if marker_at >= jpeg.len() {
            break;
        }
        let marker = jpeg[marker_at];
        index = marker_at + 1;
        if marker == 0xd8 || marker == 0xd9 || marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        if index + 2 > jpeg.len() {
            break;
        }
        let length = usize::from(u16::from_be_bytes([jpeg[index], jpeg[index + 1]]));
        if length < 2 {
            break;
        }
        if SOF_MARKERS.contains(&marker) {
            if index + 8 > jpeg.len() {
                break;
            }
            let height = u32::from(u16::from_be_bytes([jpeg[index + 3], jpeg[index + 4]]));
            let width = u32::from(u16::from_be_bytes([jpeg[index + 5], jpeg[index + 6]]));
            let components = jpeg[index + 7];
            if width == 0 || height == 0 {
                break;
            }
            return Ok((width, height, components));
        }
        index += length;
    }
    Err(Error::Pdf("no JPEG SOF marker found".into()))
}

/// Streams pages into a temp file in the output directory. The destination appears only on finish.
pub struct PdfWriter {
    writer: Option<BufWriter<File>>,
    temp: PathBuf,
    dest: PathBuf,
    pos: u64,
    offsets: Vec<Option<u64>>,
    pages: Vec<u32>,
    next: u32,
    committed: bool,
}

impl PdfWriter {
    pub fn create(dest: PathBuf) -> Result<Self> {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let name = dest
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "scan.pdf".into());
        let mut opened = None;
        for attempt in 0..100 {
            let temp = dest.with_file_name(format!(".{name}.tmp-{}-{attempt}", std::process::id()));
            match OpenOptions::new().write(true).create_new(true).open(&temp) {
                Ok(file) => {
                    opened = Some((file, temp));
                    break;
                }
                Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(Error::Io(err)),
            }
        }
        let (file, temp) =
            opened.ok_or_else(|| Error::Pdf("could not create a temp PDF file".into()))?;
        let mut writer = PdfWriter {
            writer: Some(BufWriter::new(file)),
            temp,
            dest,
            pos: 0,
            offsets: Vec::new(),
            pages: Vec::new(),
            next: 3,
            committed: false,
        };
        writer.put(b"%PDF-1.4\n")?;
        Ok(writer)
    }

    pub fn pages(&self) -> usize {
        self.pages.len()
    }

    pub fn add_page(&mut self, jpeg: &[u8], dpi: u16) -> Result<()> {
        if dpi == 0 {
            return Err(Error::Pdf("DPI must not be zero".into()));
        }
        let (width, height, components) = jpeg_size(jpeg)?;
        let color_space = match components {
            1 => "DeviceGray",
            3 => "DeviceRGB",
            other => {
                return Err(Error::Pdf(format!(
                    "{other} JPEG color components are not supported"
                )));
            }
        };
        let page_width = f64::from(width) * 72.0 / f64::from(dpi);
        let page_height = f64::from(height) * 72.0 / f64::from(dpi);

        let image = self.next;
        let content = self.next + 1;
        let page = self.next + 2;
        self.next += 3;

        self.start_object(image)?;
        self.put(
            format!(
                "<< /Type /XObject /Subtype /Image /Width {width} /Height {height} \
                 /ColorSpace /{color_space} /BitsPerComponent 8 /Filter /DCTDecode \
                 /Length {} >>\nstream\n",
                jpeg.len()
            )
            .as_bytes(),
        )?;
        self.put(jpeg)?;
        self.put(b"\nendstream\nendobj\n")?;

        let content_bytes = format!(
            "q {} 0 0 {} 0 0 cm /Im0 Do Q\n",
            num(page_width),
            num(page_height)
        );
        self.start_object(content)?;
        self.put(format!("<< /Length {} >>\nstream\n", content_bytes.len()).as_bytes())?;
        self.put(content_bytes.as_bytes())?;
        self.put(b"endstream\nendobj\n")?;

        self.start_object(page)?;
        self.put(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Rotate 180 \
                 /Resources << /XObject << /Im0 {image} 0 R >> >> /Contents {content} 0 R >>\nendobj\n",
                num(page_width),
                num(page_height)
            )
            .as_bytes(),
        )?;
        self.pages.push(page);
        Ok(())
    }

    /// Writes the catalog, page tree and xref, fsyncs and renames into place.
    pub fn finish(mut self) -> Result<PathBuf> {
        if self.pages.is_empty() {
            return Err(Error::Pdf("no pages".into()));
        }
        let kids = self
            .pages
            .iter()
            .map(|page| format!("{page} 0 R"))
            .collect::<Vec<_>>()
            .join(" ");

        self.start_object(1)?;
        self.put(b"<< /Type /Catalog /Pages 2 0 R >>\nendobj\n")?;
        self.start_object(2)?;
        self.put(
            format!(
                "<< /Type /Pages /Kids [{kids}] /Count {} >>\nendobj\n",
                self.pages.len()
            )
            .as_bytes(),
        )?;

        let xref = self.pos;
        let size = u64::from(self.next);
        self.put(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes())?;
        for number in 1..self.next {
            let offset = self.offsets[(number - 1) as usize]
                .ok_or_else(|| Error::Pdf(format!("object {number} was never written")))?;
            self.put(format!("{offset:010} 00000 n \n").as_bytes())?;
        }
        self.put(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n")
                .as_bytes(),
        )?;

        let mut writer = self
            .writer
            .take()
            .ok_or_else(|| Error::Pdf("writer already closed".into()))?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);
        fs::rename(&self.temp, &self.dest)?;
        self.committed = true;
        Ok(self.dest.clone())
    }

    fn start_object(&mut self, number: u32) -> Result<()> {
        if self.offsets.len() < number as usize {
            self.offsets.resize(number as usize, None);
        }
        self.offsets[number as usize - 1] = Some(self.pos);
        self.put(format!("{number} 0 obj\n").as_bytes())
    }

    fn put(&mut self, bytes: &[u8]) -> Result<()> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| Error::Pdf("writer already closed".into()))?;
        writer.write_all(bytes)?;
        self.pos += bytes.len() as u64;
        Ok(())
    }
}

impl Drop for PdfWriter {
    fn drop(&mut self) {
        if !self.committed {
            self.writer.take();
            let _ = fs::remove_file(&self.temp);
        }
    }
}

fn num(value: f64) -> String {
    let text = format!("{value:.4}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() || text == "-0" {
        "0".into()
    } else {
        text.to_string()
    }
}
