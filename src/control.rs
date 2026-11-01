use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use crate::discovery::ScannerInfo;
use crate::error::{Error, Result};
use crate::identity::Identity;
use crate::wire::Channel;

pub const SCANNER_TIMEOUT: u8 = 0x1e;
const PASSWORD_KEY: &[u8; 16] = b"pFusCANsNapFiPfu";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WallClock {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub min: u8,
    pub sec: u8,
    /// Seconds of -UTC offset.
    pub tz_bias_secs: i32,
}

pub fn local_now() -> WallClock {
    let mut now: libc::time_t = 0;
    // SAFETY: both pointers refer to live, correctly typed storage.
    unsafe {
        libc::time(&mut now);
    }
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: localtime_r writes into the tm that was just allocated for it.
    unsafe {
        libc::localtime_r(&now, &mut tm);
    }
    WallClock {
        year: (tm.tm_year + 1900) as u16,
        month: (tm.tm_mon + 1) as u8,
        day: tm.tm_mday as u8,
        hour: tm.tm_hour as u8,
        min: tm.tm_min as u8,
        sec: tm.tm_sec as u8,
        tz_bias_secs: -(tm.tm_gmtoff as i32),
    }
}

/// ScanSnap Home's encoding: three decimal digits per character.
pub fn encode_password(password: &str) -> Result<String> {
    if password.is_empty() || password.len() > PASSWORD_KEY.len() || !password.is_ascii() {
        return Err(Error::Config(format!(
            "password must be 1 to {} ASCII characters",
            PASSWORD_KEY.len()
        )));
    }
    Ok(password
        .bytes()
        .zip(PASSWORD_KEY)
        .map(|(byte, key)| format!("{:03}", u32::from(*key) + u32::from(byte) + 11))
        .collect())
}

/// Factory Wi-Fi password: the last four characters of the serial number.
pub fn default_password(serial: &str) -> String {
    let chars: Vec<char> = serial.chars().collect();
    chars[chars.len().saturating_sub(4)..].iter().collect()
}

pub fn register_body(
    identity: &Identity,
    host_ip: Ipv4Addr,
    if_version: u16,
    password: &str,
    now: WallClock,
) -> Result<[u8; 96]> {
    let code = if password.is_empty() {
        None
    } else {
        Some(encode_password(password)?)
    };
    let mut body = [0u8; 96];
    body[0..2].copy_from_slice(&if_version.to_be_bytes());
    body[2] = SCANNER_TIMEOUT;
    let flag = u32::from(code.is_some());
    body[8..12].copy_from_slice(&flag.to_be_bytes());
    body[12..16].copy_from_slice(&host_ip.octets());
    body[16..20].copy_from_slice(&u32::from(identity.host_port).to_be_bytes());
    if let Some(code) = code {
        body[20..20 + code.len()].copy_from_slice(code.as_bytes());
    }
    body[68..70].copy_from_slice(&now.year.to_be_bytes());
    body[70] = now.month;
    body[71] = now.day;
    body[72] = now.hour;
    body[73] = now.min;
    body[74] = now.sec;
    body[76..84].copy_from_slice(&identity.host_id);
    if if_version > 3 {
        body[84..88].copy_from_slice(&now.tz_bias_secs.to_be_bytes());
    }
    Ok(body)
}

/// 0x11 on the control port. Status is the reply word at offset 8, not the header status at 12.
pub fn register(
    info: &ScannerInfo,
    identity: &Identity,
    host_ip: Ipv4Addr,
    password: &str,
    now: WallClock,
) -> Result<()> {
    let body = register_body(identity, host_ip, info.if_version, password, now)?;
    let addr = SocketAddr::from((info.ip, info.control_port));
    let mut channel = Channel::connect(addr, identity.mac, Duration::from_secs(10))?;
    let reply = channel.call(0x11, &body)?;
    if reply.len() < 12 {
        return Err(Error::Protocol(format!(
            "short registration reply of {} bytes",
            reply.len()
        )));
    }
    let status = i32::from_be_bytes([reply[8], reply[9], reply[10], reply[11]]);
    if status != 0 {
        return Err(Error::Registration(status));
    }
    Ok(())
}
