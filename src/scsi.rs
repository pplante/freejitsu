use std::net::SocketAddr;
use std::time::Duration;

use crate::discovery::ScannerInfo;
use crate::error::{Error, Result};
use crate::wire::{Channel, frame_status};

/// Server header plus 24 reserved bytes, then the data.
pub const RESPONSE_HEADER: usize = 40;
pub const CHECK_CONDITION: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sense {
    pub key: u8,
    pub asc: u8,
    pub ascq: u8,
    pub eom: bool,
    pub info: u32,
}

impl Sense {
    pub fn parse(data: &[u8]) -> Result<Sense> {
        if data.len() < 14 {
            return Err(Error::Protocol(format!("short sense data {}", hex(data))));
        }
        Ok(Sense {
            key: data[2] & 0x0f,
            asc: data[12],
            ascq: data[13],
            eom: data[2] & 0x40 != 0,
            info: u32::from_be_bytes([data[3], data[4], data[5], data[6]]),
        })
    }

    pub fn no_paper(self) -> bool {
        (self.key, self.asc, self.ascq) == (3, 0x80, 0x03)
    }

    pub fn medium_error(self) -> Option<&'static str> {
        if self.key == 3 && self.asc == 0x80 {
            Some(match self.ascq {
                0x01 => "paper jam",
                0x02 => "cover open",
                0x03 => "no paper",
                0x07 => "double feed",
                _ => "medium error",
            })
        } else {
            None
        }
    }
}

impl std::fmt::Display for Sense {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "sense key={:#x} asc={:#04x} ascq={:#04x} eom={}",
            self.key, self.asc, self.ascq, self.eom
        )
    }
}

pub struct ScsiChannel {
    channel: Channel,
}

impl ScsiChannel {
    pub fn open(info: &ScannerInfo, mac: [u8; 6]) -> Result<Self> {
        Self::open_at(info.ip, info.scan_port, mac, Duration::from_secs(60))
    }

    pub fn open_at(
        ip: std::net::Ipv4Addr,
        port: u16,
        mac: [u8; 6],
        timeout: Duration,
    ) -> Result<Self> {
        let addr = SocketAddr::from((ip, port));
        Ok(ScsiChannel {
            channel: Channel::connect(addr, mac, timeout)?,
        })
    }

    /// Sends a CDB and returns the SCSI status plus the data that follows the 40 byte reply header.
    pub fn raw(&mut self, cdb: &[u8], out: &[u8], in_len: usize) -> Result<(u8, Vec<u8>)> {
        if !(6..=16).contains(&cdb.len()) {
            return Err(Error::Protocol(format!("CDB is {} bytes", cdb.len())));
        }
        let mut body = vec![0u8; 32];
        body[0..4].copy_from_slice(&(cdb.len() as u32).to_be_bytes());
        body[4..8].copy_from_slice(&(in_len as u32).to_be_bytes());
        body[8..12].copy_from_slice(&(out.len() as u32).to_be_bytes());
        body[16..16 + cdb.len()].copy_from_slice(cdb);
        body.extend_from_slice(out);

        let reply = self.channel.call(1, &body)?;
        if reply.len() < RESPONSE_HEADER {
            return Err(Error::Protocol(format!(
                "short SCSI reply of {} bytes",
                reply.len()
            )));
        }
        let status = frame_status(&reply);
        let status = u8::try_from(status)
            .map_err(|_| Error::Protocol(format!("bad SCSI status {status}")))?;
        let data = reply[RESPONSE_HEADER..].to_vec();
        if data.len() > in_len {
            return Err(Error::Protocol(format!(
                "{} bytes for a read of {in_len}",
                data.len()
            )));
        }
        Ok((status, data))
    }

    pub fn checked(&mut self, cdb: &[u8], out: &[u8], in_len: usize) -> Result<Vec<u8>> {
        let (status, data) = self.raw(cdb, out, in_len)?;
        if status == CHECK_CONDITION {
            let sense = self.sense()?;
            return Err(Error::Scsi {
                cd: cdb[0],
                status,
                sense: Some(sense),
            });
        }
        if status != 0 {
            return Err(Error::Scsi {
                cd: cdb[0],
                status,
                sense: None,
            });
        }
        Ok(data)
    }

    pub fn sense(&mut self) -> Result<Sense> {
        let (_, data) = self.raw(&[0x03, 0, 0, 0, 0x12, 0], &[], 0x12)?;
        Sense::parse(&data)
    }

    /// C2 hardware status. Byte 3 bit 7 is set when the feeder is empty.
    pub fn hardware_status(&mut self) -> Result<[u8; 0x20]> {
        let data = self.checked(&[0xc2, 0, 0, 0, 0, 0, 0, 0, 0x20, 0], &[], 0x20)?;
        if data.len() < 0x20 {
            return Err(Error::Protocol(format!(
                "short C2 status of {} bytes",
                data.len()
            )));
        }
        let mut status = [0u8; 0x20];
        status.copy_from_slice(&data[..0x20]);
        Ok(status)
    }
}

/// D6 on a connection of its own, as ScanSnap Home ends every scan.
pub fn release(info: &ScannerInfo, mac: [u8; 6]) -> Result<()> {
    let mut channel = ScsiChannel::open_at(info.ip, info.scan_port, mac, Duration::from_secs(10))?;
    channel.checked(&[0xd6, 0, 0, 0, 0, 0], &[], 0)?;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
