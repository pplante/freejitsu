#![allow(dead_code)]

use std::fs;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub const MAGIC: [u8; 4] = *b"VENS";

pub fn server_frame(body: &[u8], status: i32) -> Vec<u8> {
    let mut frame = Vec::with_capacity(16 + body.len());
    frame.extend_from_slice(&((16 + body.len()) as u32).to_be_bytes());
    frame.extend_from_slice(&MAGIC);
    frame.extend_from_slice(&0u32.to_be_bytes());
    frame.extend_from_slice(&status.to_be_bytes());
    frame.extend_from_slice(body);
    frame
}

pub fn read_frame(stream: &mut TcpStream) -> io::Result<Option<Vec<u8>>> {
    let mut head = [0u8; 8];
    match stream.read_exact(&mut head) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) if err.kind() == io::ErrorKind::ConnectionReset => return Ok(None),
        Err(err) => return Err(err),
    }
    let length = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
    if head[4..8] != MAGIC || !(16..=0x40_0000).contains(&length) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "bad frame"));
    }
    let mut frame = Vec::with_capacity(length);
    frame.extend_from_slice(&head);
    frame.resize(length, 0);
    stream.read_exact(&mut frame[8..])?;
    Ok(Some(frame))
}

pub struct ClientFrame {
    pub command: u32,
    pub mac: [u8; 6],
    pub body: Vec<u8>,
}

pub fn parse_client_frame(frame: &[u8]) -> Option<ClientFrame> {
    if frame.len() < 32 || frame[4..8] != MAGIC {
        return None;
    }
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&frame[16..22]);
    Some(ClientFrame {
        command: u32::from_be_bytes([frame[8], frame[9], frame[10], frame[11]]),
        mac,
        body: frame[32..].to_vec(),
    })
}

pub fn temp_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "freejitsu-{tag}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).expect("temp dir");
    path
}

pub fn scanner_info(
    ip: Ipv4Addr,
    scan_port: u16,
    control_port: u16,
) -> freejitsu::discovery::ScannerInfo {
    freejitsu::discovery::ScannerInfo {
        ip,
        scan_port,
        control_port,
        mac: [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
        name: "iX500-A0XX012345".into(),
        model: "ScanSnap iX500".into(),
        if_version: 6,
        paired_ip: ip,
        paired_host_id: [0u8; 8],
        serial: "A0XX012345".into(),
    }
}

#[derive(Clone)]
pub struct DiscoveryConfig {
    pub ip: Ipv4Addr,
    pub scan_port: u16,
    pub control_port: u16,
    pub mac: [u8; 6],
    pub name: String,
    pub if_version: u16,
    pub paired_ip: Ipv4Addr,
    pub paired_host_id: [u8; 8],
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        DiscoveryConfig {
            ip: Ipv4Addr::LOCALHOST,
            scan_port: 53218,
            control_port: 53219,
            mac: [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
            name: "iX500-A0XX012345".into(),
            if_version: 6,
            paired_ip: Ipv4Addr::LOCALHOST,
            paired_host_id: [0; 8],
        }
    }
}

pub fn discovery_reply(config: &DiscoveryConfig) -> [u8; 132] {
    let mut data = [0u8; 132];
    data[0..4].copy_from_slice(&MAGIC);
    data[8..10].copy_from_slice(&config.if_version.to_be_bytes());
    data[10..12].copy_from_slice(&0x30u16.to_be_bytes());
    data[12..16].fill(0xff);
    data[16..20].copy_from_slice(&config.ip.octets());
    data[20..24].copy_from_slice(&u32::from(config.scan_port).to_be_bytes());
    data[24..28].copy_from_slice(&u32::from(config.control_port).to_be_bytes());
    data[28..34].copy_from_slice(&config.mac);
    data[36..40].copy_from_slice(&4u32.to_be_bytes());
    let name = config.name.as_bytes();
    let length = name.len().min(64);
    data[40..40 + length].copy_from_slice(&name[..length]);
    data[104..118].copy_from_slice(b"ScanSnap iX500");
    data[120..124].copy_from_slice(&config.paired_ip.octets());
    data[124..132].copy_from_slice(&config.paired_host_id);
    data
}

pub struct FakeDiscovery {
    pub port: u16,
    pub probes: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeDiscovery {
    pub fn start(config: DiscoveryConfig) -> Self {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind discovery");
        socket
            .set_read_timeout(Some(Duration::from_millis(50)))
            .expect("discovery timeout");
        let port = socket.local_addr().expect("discovery addr").port();
        let reply = discovery_reply(&config);
        let probes = Arc::new(Mutex::new(Vec::new()));
        let thread_probes = Arc::clone(&probes);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            let mut buffer = [0u8; 2048];
            while !thread_stop.load(Ordering::Relaxed) {
                match socket.recv_from(&mut buffer) {
                    Ok((size, sender)) => {
                        thread_probes.lock().unwrap().push(buffer[..size].to_vec());
                        let _ = socket.send_to(&reply, sender);
                    }
                    Err(err)
                        if matches!(
                            err.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) => {}
                    Err(_) => return,
                }
            }
        });
        FakeDiscovery {
            port,
            probes,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for FakeDiscovery {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Clone, Default)]
pub struct ControlLog {
    pub commands: Vec<u32>,
    pub macs: Vec<[u8; 6]>,
    pub bodies: Vec<Vec<u8>>,
}

pub struct FakeControl {
    pub port: u16,
    pub log: Arc<Mutex<ControlLog>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeControl {
    pub fn start(status: i32) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind control");
        listener.set_nonblocking(true).expect("control nonblocking");
        let port = listener.local_addr().expect("control addr").port();
        let log = Arc::new(Mutex::new(ControlLog::default()));
        let thread_log = Arc::clone(&log);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let log = Arc::clone(&thread_log);
                        thread::spawn(move || serve_control(stream, status, log));
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => return,
                }
            }
        });
        FakeControl {
            port,
            log,
            stop,
            thread: Some(thread),
        }
    }

    pub fn snapshot(&self) -> ControlLog {
        self.log.lock().unwrap().clone()
    }
}

impl Drop for FakeControl {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve_control(mut stream: TcpStream, status: i32, log: Arc<Mutex<ControlLog>>) {
    stream.set_nonblocking(false).expect("control blocking");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("control read timeout");
    if stream.write_all(&server_frame(&[], 0)).is_err() {
        return;
    }
    let Ok(Some(frame)) = read_frame(&mut stream) else {
        return;
    };
    let Some(client) = parse_client_frame(&frame) else {
        return;
    };
    {
        let mut log = log.lock().unwrap();
        log.commands.push(client.command);
        log.macs.push(client.mac);
        log.bodies.push(client.body);
    }
    // The iX500 puts the registration status at offset 8 of the reply.
    let mut reply = vec![0u8; 20];
    reply[0..4].copy_from_slice(&20u32.to_be_bytes());
    reply[4..8].copy_from_slice(&MAGIC);
    reply[8..12].copy_from_slice(&status.to_be_bytes());
    let _ = stream.write_all(&reply);
}

#[derive(Clone, Default)]
pub struct FakeSheet {
    pub front: Vec<Vec<u8>>,
    pub back: Vec<Vec<u8>>,
}

impl FakeSheet {
    pub fn new(front: Vec<u8>, back: Vec<u8>) -> Self {
        FakeSheet {
            front: vec![front],
            back: vec![back],
        }
    }

    pub fn chunked(front: Vec<Vec<u8>>, back: Vec<Vec<u8>>) -> Self {
        FakeSheet { front, back }
    }
}

#[derive(Clone)]
pub struct ScanConfig {
    pub sheets: Vec<FakeSheet>,
    pub jam_at: Vec<usize>,
    pub info: [u8; 20],
    pub delay: std::time::Duration,
}

impl Default for ScanConfig {
    fn default() -> Self {
        let mut info = [0u8; 20];
        info[0..4].copy_from_slice(&300u32.to_be_bytes());
        info[4..8].copy_from_slice(&400u32.to_be_bytes());
        info[8..12].copy_from_slice(&300u32.to_be_bytes());
        info[12..16].copy_from_slice(&400u32.to_be_bytes());
        info[16..18].copy_from_slice(&300u16.to_be_bytes());
        info[18..20].copy_from_slice(&300u16.to_be_bytes());
        ScanConfig {
            sheets: Vec::new(),
            jam_at: Vec::new(),
            info,
            delay: std::time::Duration::ZERO,
        }
    }
}

#[derive(Clone, Default)]
pub struct ScanLog {
    pub cdbs: Vec<Vec<u8>>,
    pub out: Vec<Vec<u8>>,
    pub macs: Vec<[u8; 6]>,
    pub connections: usize,
}

pub struct FakeScan {
    pub port: u16,
    pub log: Arc<Mutex<ScanLog>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeScan {
    pub fn start(config: ScanConfig) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind scan");
        listener.set_nonblocking(true).expect("scan nonblocking");
        let port = listener.local_addr().expect("scan addr").port();
        let log = Arc::new(Mutex::new(ScanLog::default()));
        let thread_log = Arc::clone(&log);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        thread_log.lock().unwrap().connections += 1;
                        let log = Arc::clone(&thread_log);
                        let config = config.clone();
                        thread::spawn(move || serve_scan(stream, &config, log));
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => return,
                }
            }
        });
        FakeScan {
            port,
            log,
            stop,
            thread: Some(thread),
        }
    }

    pub fn snapshot(&self) -> ScanLog {
        self.log.lock().unwrap().clone()
    }
}

impl Drop for FakeScan {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve_scan(mut stream: TcpStream, config: &ScanConfig, log: Arc<Mutex<ScanLog>>) {
    stream.set_nonblocking(false).expect("scan blocking");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("scan read timeout");
    if stream.write_all(&server_frame(&[], 0)).is_err() {
        return;
    }
    let mut next_sheet = 0usize;
    let mut current: Option<usize> = None;
    let mut front_done = false;
    let mut chunk = 0usize;
    let mut pending_sense = [0u8; 18];

    loop {
        let Ok(Some(frame)) = read_frame(&mut stream) else {
            return;
        };
        let Some(client) = parse_client_frame(&frame) else {
            return;
        };
        if client.command != 1 || client.body.len() < 32 {
            return;
        }
        let cdb_length = u32::from_be_bytes([
            client.body[0],
            client.body[1],
            client.body[2],
            client.body[3],
        ]) as usize;
        if !(6..=16).contains(&cdb_length) || 16 + cdb_length > client.body.len() {
            return;
        }
        let cdb = client.body[16..16 + cdb_length].to_vec();
        let mut data = Vec::new();
        let mut status = 0i32;

        match cdb[0] {
            0xc2 => {
                let mut hardware = [0u8; 32];
                if next_sheet >= config.sheets.len() {
                    hardware[3] = 0x80;
                }
                data = hardware.to_vec();
            }
            0xd5 => data = vec![0u8; 8],
            0xd6 | 0xd8 | 0xd4 | 0xe9 => {}
            0x03 => {
                data = pending_sense.to_vec();
                pending_sense = [0u8; 18];
            }
            0xe0 => {
                if next_sheet >= config.sheets.len() {
                    status = 2;
                    pending_sense = sense(3, 0x80, 0x03, false);
                } else if config.jam_at.contains(&next_sheet) {
                    status = 2;
                    pending_sense = sense(3, 0x80, 0x01, false);
                } else {
                    if !config.delay.is_zero() {
                        thread::sleep(config.delay);
                    }
                    current = Some(next_sheet);
                    next_sheet += 1;
                    front_done = false;
                    chunk = 0;
                }
            }
            0x28 if cdb[2] == 0x80 => data = config.info.to_vec(),
            0x28 => {
                let Some(sheet_index) = current else {
                    status = 2;
                    pending_sense = sense(3, 0x80, 0x01, false);
                    let mut body = vec![0u8; 24];
                    body.append(&mut data);
                    let _ = stream.write_all(&server_frame(&body, status));
                    continue;
                };
                let sheet = &config.sheets[sheet_index];
                let (chunks, is_front) = if !front_done {
                    (&sheet.front, true)
                } else {
                    (&sheet.back, false)
                };
                if chunks.is_empty() {
                    status = 2;
                    pending_sense = sense(3, 0x80, 0x01, false);
                } else {
                    let index = chunk.min(chunks.len() - 1);
                    data.push(cdb[10]);
                    data.push(index as u8);
                    data.extend_from_slice(&chunks[index]);
                    if index + 1 == chunks.len() {
                        status = 2;
                        pending_sense = sense(0, 0, 0, true);
                        if is_front {
                            front_done = true;
                        }
                        chunk = 0;
                    } else {
                        chunk = index + 1;
                    }
                }
            }
            _ => {}
        }

        {
            let mut log = log.lock().unwrap();
            log.cdbs.push(cdb);
            log.out.push(client.body[32..].to_vec());
            log.macs.push(client.mac);
        }
        let mut body = vec![0u8; 24];
        body.append(&mut data);
        if stream.write_all(&server_frame(&body, status)).is_err() {
            return;
        }
    }
}

fn sense(key: u8, asc: u8, ascq: u8, eom: bool) -> [u8; 18] {
    let mut data = [0u8; 18];
    data[2] = (key & 0x0f) | if eom { 0x40 } else { 0 };
    data[12] = asc;
    data[13] = ascq;
    data
}

pub struct FakeScanner {
    pub discovery: FakeDiscovery,
    pub control: FakeControl,
    pub scan: FakeScan,
    pub paired_host_id: [u8; 8],
    pub mac: [u8; 6],
}

impl FakeScanner {
    pub fn start(scan_config: ScanConfig, control_status: i32, if_version: u16) -> Self {
        let scan = FakeScan::start(scan_config);
        let control = FakeControl::start(control_status);
        let paired_host_id = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77];
        let mac = [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
        let discovery = FakeDiscovery::start(DiscoveryConfig {
            ip: Ipv4Addr::LOCALHOST,
            scan_port: scan.port,
            control_port: control.port,
            mac,
            name: "iX500-A0XX012345".into(),
            if_version,
            paired_ip: Ipv4Addr::LOCALHOST,
            paired_host_id,
        });
        FakeScanner {
            discovery,
            control,
            scan,
            paired_host_id,
            mac,
        }
    }
}

pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

pub fn http_request(addr: std::net::SocketAddr, request: &[u8]) -> HttpResponse {
    let mut stream = TcpStream::connect(addr).expect("connect http");
    stream.write_all(request).expect("write http");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read http");
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("http head");
    let head = std::str::from_utf8(&raw[..split]).expect("http head utf8");
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|word| word.parse().ok())
        .expect("http status");
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    HttpResponse {
        status,
        headers,
        body: raw[split + 4..].to_vec(),
    }
}

pub fn http_get(addr: std::net::SocketAddr, path: &str) -> HttpResponse {
    http_request(
        addr,
        format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
}

pub fn http_post(addr: std::net::SocketAddr, path: &str, body: &[u8]) -> HttpResponse {
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut request = head.into_bytes();
    request.extend_from_slice(body);
    http_request(addr, &request)
}

pub fn http_delete(addr: std::net::SocketAddr, path: &str) -> HttpResponse {
    http_request(
        addr,
        format!("DELETE {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
}
