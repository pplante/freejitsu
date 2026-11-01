use crate::config::{Dpi, PageSize};
use crate::error::{Error, Result};
use crate::scsi::{CHECK_CONDITION, ScsiChannel, Sense};

pub const READ_LEN: usize = 0x30_0000;
pub const MAX_IMAGE: usize = 64 << 20;
const MAX_CHUNKS: usize = 256;

#[derive(Clone, Debug)]
pub struct ImageInfo {
    pub width: u32,
    pub max_height: u32,
    pub height: u32,
    pub dpi_x: u16,
    pub dpi_y: u16,
}

#[derive(Clone, Debug)]
pub struct Sheet {
    pub front: Vec<u8>,
    pub back: Vec<u8>,
    pub info: ImageInfo,
}

/// E9 data block.
pub fn window_block(dpi: Dpi, page: PageSize) -> [u8; 32] {
    let (width, height) = page.dims();
    let mut block = [0u8; 32];
    block[0..2].copy_from_slice(&dpi.as_u16().to_be_bytes());
    block[2..4].copy_from_slice(&dpi.as_u16().to_be_bytes());
    block[4..8].copy_from_slice(&width.to_be_bytes());
    block[8..12].copy_from_slice(&height.to_be_bytes());
    block[12] = 0x05;
    block
}

/// D4 data block: 32 parameter bytes plus the 48 byte window descriptor.
pub fn param_block(dpi: Dpi, page: PageSize) -> [u8; 80] {
    let (width, height) = page.dims();
    let dpi = dpi.as_u16();
    let mut block = [0u8; 80];
    block[1] = 3;
    let auto = u8::from(page.automatic());
    block[2] = auto;
    block[3] = auto;
    block[4] = 0xd0;
    block[6] = 0xc1;
    block[7] = 0x80;
    block[8] = 0x80;
    block[9] = 0x80;
    block[10] = if dpi == 300 { 0x90 } else { 0x80 };
    block[11] = 0x80;
    block[12] = 0x80;
    block[13] = if dpi == 600 { 0x80 } else { 0x00 };
    block[31] = 0x30;
    block[32..34].copy_from_slice(&[0x00, 0x10]);
    block[34..36].copy_from_slice(&dpi.to_be_bytes());
    block[36..38].copy_from_slice(&dpi.to_be_bytes());
    block[38] = 5;
    block[39] = 0x81;
    block[40] = match dpi {
        150 | 200 => 5,
        300 => 0,
        600 => 0x0a,
        _ => 0,
    };
    block[42..46].copy_from_slice(&width.to_be_bytes());
    block[46..50].copy_from_slice(&height.to_be_bytes());
    block[50] = 4;
    block
}

pub fn read_cdb(back: bool, index: u8) -> [u8; 12] {
    let mut cdb = [0u8; 12];
    cdb[0] = 0x28;
    cdb[3] = 0x02;
    cdb[5] = if back { 0x80 } else { 0x00 };
    cdb[6..9].copy_from_slice(&(READ_LEN as u32).to_be_bytes()[1..4]);
    cdb[10] = index;
    cdb
}

pub fn info_cdb(index: u8) -> [u8; 12] {
    let mut cdb = [0u8; 12];
    cdb[0] = 0x28;
    cdb[2] = 0x80;
    cdb[5] = 0x80;
    cdb[8] = 0x20;
    cdb[10] = index;
    cdb
}

pub fn parse_image_info(data: &[u8]) -> Result<ImageInfo> {
    if data.len() < 20 {
        return Err(Error::Protocol(format!(
            "short image info of {} bytes",
            data.len()
        )));
    }
    Ok(ImageInfo {
        width: u32::from_be_bytes([data[0], data[1], data[2], data[3]]),
        max_height: u32::from_be_bytes([data[4], data[5], data[6], data[7]]),
        height: u32::from_be_bytes([data[12], data[13], data[14], data[15]]),
        dpi_x: u16::from_be_bytes([data[16], data[17]]),
        dpi_y: u16::from_be_bytes([data[18], data[19]]),
    })
}

/// D5 through D6. Both sides are always read; simplex is a capture decision.
pub fn scan(
    channel: &mut ScsiChannel,
    dpi: Dpi,
    page: PageSize,
    on_sheet: &mut dyn FnMut(Sheet) -> Result<()>,
) -> Result<usize> {
    channel.hardware_status()?;
    channel.checked(&[0xd5, 0, 0, 0, 0x08, 0x08], &[0u8; 8], 8)?;
    channel.checked(&[0xd8, 0, 0, 0, 0, 0], &[], 0)?;
    channel.checked(
        &[0xe9, 0, 0, 0, 0, 0, 0, 0x20, 0, 0],
        &window_block(dpi, page),
        0,
    )?;
    channel.checked(&[0xd4, 0, 0, 0, 0x50, 0], &param_block(dpi, page), 0)?;
    channel.hardware_status()?;
    channel.sense()?;

    let mut index: u32 = 0;
    let mut sheets = 0usize;
    loop {
        let (status, _) = channel.raw(&[0xe0, 0, 0, 0, 0, 0], &[], 0)?;
        if status == CHECK_CONDITION {
            let sense = channel.sense()?;
            if sense.no_paper() {
                if sheets == 0 {
                    return Err(Error::NoPaper);
                }
                return Ok(sheets);
            }
            return Err(feed_error(0xe0, status, sense));
        }
        if status != 0 {
            return Err(Error::Scsi {
                cd: 0xe0,
                status,
                sense: None,
            });
        }
        let front = read_image(channel, false, index as u8)?;
        let back = read_image(channel, true, (index + 1) as u8)?;
        let info_data = channel.checked(&info_cdb((index + 1) as u8), &[], 0x20)?;
        let info = parse_image_info(&info_data)?;
        channel.hardware_status()?;
        index += 2;
        sheets += 1;
        on_sheet(Sheet { front, back, info })?;
    }
}

fn read_image(channel: &mut ScsiChannel, back: bool, index: u8) -> Result<Vec<u8>> {
    let mut parts: Vec<Vec<u8>> = Vec::new();
    let mut total = 0usize;
    for chunk in 0..MAX_CHUNKS {
        let (status, data) = channel.raw(&read_cdb(back, index), &[], READ_LEN)?;
        let sense = if status == CHECK_CONDITION {
            Some(channel.sense()?)
        } else {
            None
        };
        if let Some(sense) = sense
            && !sense.eom
        {
            return Err(feed_error(0x28, status, sense));
        }
        if status != 0 && status != CHECK_CONDITION {
            return Err(Error::Scsi {
                cd: 0x28,
                status,
                sense,
            });
        }
        if data.len() < 2 || data[0] != index || data[1] != chunk as u8 {
            let header = data.get(..2).map(hex).unwrap_or_default();
            return Err(Error::Protocol(format!(
                "unexpected image header {header} for image {index}"
            )));
        }
        total += data.len() - 2;
        if total > MAX_IMAGE {
            return Err(Error::Protocol(format!(
                "image {index} larger than {} MB without an end",
                MAX_IMAGE >> 20
            )));
        }
        parts.push(data[2..].to_vec());
        if sense.is_some() {
            return Ok(parts.concat());
        }
    }
    Err(Error::Protocol(format!(
        "image {index} has more than {MAX_CHUNKS} chunks"
    )))
}

fn feed_error(cd: u8, status: u8, sense: Sense) -> Error {
    match sense.medium_error() {
        Some(reason) => Error::Feed(reason),
        None => Error::Scsi {
            cd,
            status,
            sense: Some(sense),
        },
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
