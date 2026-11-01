use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::iface;
use crate::wire::MAGIC;

pub const PROBE_FLAG: u16 = 0x0010;
pub const HEARTBEAT_FLAG: u16 = 0x1000;
pub const HEARTBEAT_PORT: u16 = 55264;
const DISCOVERY_ATTEMPTS: u32 = 3;
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone, Debug)]
pub struct ScannerInfo {
    pub ip: Ipv4Addr,
    pub scan_port: u16,
    pub control_port: u16,
    pub mac: [u8; 6],
    pub name: String,
    pub model: String,
    pub if_version: u16,
    pub paired_ip: Ipv4Addr,
    pub paired_host_id: [u8; 8],
    pub serial: String,
}

/// 32 byte probe. `host_port` is the UDP port that answers, not the destination.
pub fn probe(
    host_ip: Ipv4Addr,
    host_mac: [u8; 6],
    host_port: u16,
    seq: u32,
    flag: u16,
) -> [u8; 32] {
    let mut packet = [0u8; 32];
    packet[0..4].copy_from_slice(&MAGIC);
    packet[4..8].copy_from_slice(&seq.to_be_bytes());
    packet[8..12].copy_from_slice(&host_ip.octets());
    packet[12..18].copy_from_slice(&host_mac);
    packet[22..24].copy_from_slice(&host_port.to_be_bytes());
    packet[24..26].copy_from_slice(&flag.to_be_bytes());
    packet
}

/// Parses a 132 byte reply. A reply whose embedded IP differs from the sender is rejected.
pub fn parse_reply(data: &[u8], sender: Ipv4Addr) -> Option<ScannerInfo> {
    if data.len() < 132 || data[0..4] != MAGIC {
        return None;
    }
    let ip = Ipv4Addr::new(data[16], data[17], data[18], data[19]);
    if ip != sender {
        return None;
    }
    let scan_port =
        u16::try_from(u32::from_be_bytes([data[20], data[21], data[22], data[23]])).ok()?;
    let control_port =
        u16::try_from(u32::from_be_bytes([data[24], data[25], data[26], data[27]])).ok()?;
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&data[28..34]);
    let name = cstr(&data[40..104]);
    let serial = name.rsplit('-').next().unwrap_or(&name).to_string();
    Some(ScannerInfo {
        ip,
        scan_port,
        control_port,
        mac,
        name,
        model: cstr(&data[104..120]),
        if_version: u16::from_be_bytes([data[8], data[9]]),
        paired_ip: Ipv4Addr::new(data[120], data[121], data[122], data[123]),
        paired_host_id: data[124..132].try_into().ok()?,
        serial,
    })
}

fn cstr(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

/// Three probes with a one second timeout each.
pub fn discover(ip: Ipv4Addr, host_mac: [u8; 6], port: u16) -> Result<ScannerInfo> {
    let host_ip = iface::local_ip_towards(ip)?;
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    let host_port = socket.local_addr()?.port();
    let packet = probe(host_ip, host_mac, host_port, 0, PROBE_FLAG);
    let mut buffer = [0u8; 2048];
    for _ in 0..DISCOVERY_ATTEMPTS {
        socket.send_to(&packet, (ip, port))?;
        let deadline = Instant::now() + DISCOVERY_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            socket.set_read_timeout(Some(remaining))?;
            match socket.recv_from(&mut buffer) {
                Ok((size, sender)) => {
                    if let SocketAddr::V4(addr) = sender
                        && let Some(info) = parse_reply(&buffer[..size], *addr.ip())
                    {
                        return Ok(info);
                    }
                }
                Err(err) if timeout(&err) => break,
                Err(err) => return Err(Error::Io(err)),
            }
        }
    }
    Err(Error::Discovery(format!("no iX500 answered at {ip}")))
}

fn timeout(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

/// Keep-alive: three probes 0.5 seconds apart every 30 seconds from UDP 55264.
pub struct Heartbeat {
    stop: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Heartbeat {
    pub fn start(info: &ScannerInfo, mac: [u8; 6], discovery_port: u16) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let scanner_ip = info.ip;
        let thread_stop = Arc::clone(&stop);
        let thread_alive = Arc::clone(&alive);
        let thread = thread::spawn(move || {
            let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, HEARTBEAT_PORT))
                .or_else(|_| UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)));
            let socket = match socket {
                Ok(socket) => socket,
                Err(_) => {
                    thread_alive.store(false, Ordering::Relaxed);
                    return;
                }
            };
            let host_ip = iface::local_ip_towards(scanner_ip).unwrap_or(Ipv4Addr::UNSPECIFIED);
            let host_port = socket
                .local_addr()
                .map(|addr| addr.port())
                .unwrap_or(HEARTBEAT_PORT);
            let packet = probe(host_ip, mac, host_port, 1, HEARTBEAT_FLAG);
            let mut misses = 0u32;
            while !thread_stop.load(Ordering::Relaxed) {
                let mut replies = 0;
                for index in 0..3 {
                    if index > 0 && !pause(&thread_stop, Duration::from_millis(500)) {
                        break;
                    }
                    if socket
                        .send_to(&packet, (scanner_ip, discovery_port))
                        .is_err()
                    {
                        continue;
                    }
                    let _ = socket.set_read_timeout(Some(Duration::from_millis(300)));
                    let mut buffer = [0u8; 64];
                    if let Ok((size, sender)) = socket.recv_from(&mut buffer)
                        && sender.ip() == scanner_ip
                        && size >= 4
                        && buffer[..4] == MAGIC
                    {
                        replies += 1;
                    }
                }
                if thread_stop.load(Ordering::Relaxed) {
                    break;
                }
                if replies > 0 {
                    misses = 0;
                    thread_alive.store(true, Ordering::Relaxed);
                } else {
                    misses += 1;
                    if misses >= 2 {
                        thread_alive.store(false, Ordering::Relaxed);
                        return;
                    }
                }
                if !pause(&thread_stop, Duration::from_secs(30)) {
                    break;
                }
            }
            thread_alive.store(false, Ordering::Relaxed);
        });
        Heartbeat {
            stop,
            alive,
            thread: Some(thread),
        }
    }

    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Sleeps in small steps and returns false when stopped.
fn pause(stop: &AtomicBool, duration: Duration) -> bool {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(remaining.min(Duration::from_millis(50)));
    }
    true
}
