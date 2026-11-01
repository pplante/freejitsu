use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::error::Result;
use crate::wire::MAGIC;

pub const NOTIFY_LEN: usize = 48;
pub const REPEAT_WINDOW: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Button,
    PowerOff,
    PowerOn,
}

/// Parses a notification; `sender` must be the scanner.
pub fn parse_notification(data: &[u8], sender: Ipv4Addr) -> Option<(Event, u8)> {
    if data.len() < NOTIFY_LEN
        || data[4..8] != MAGIC
        || u32::from_be_bytes([data[0], data[1], data[2], data[3]]) != NOTIFY_LEN as u32
    {
        return None;
    }
    let event = match u32::from_be_bytes([data[8], data[9], data[10], data[11]]) {
        0x01 => Event::Button,
        0x10 => Event::PowerOff,
        0x21 => Event::PowerOn,
        _ => return None,
    };
    let embedded = Ipv4Addr::new(data[20], data[21], data[22], data[23]);
    if embedded != Ipv4Addr::UNSPECIFIED && embedded != sender {
        return None;
    }
    Some((event, data[16]))
}

/// Drops repeats of the same event and counter inside the repeat window.
#[derive(Default)]
pub struct Dedupe {
    last: Option<(Event, u8, Instant)>,
}

impl Dedupe {
    pub fn accept(&mut self, event: Event, counter: u8) -> bool {
        self.accept_at(event, counter, Instant::now())
    }

    pub fn accept_at(&mut self, event: Event, counter: u8, now: Instant) -> bool {
        if let Some((last_event, last_counter, at)) = self.last
            && last_event == event
            && last_counter == counter
            && now.saturating_duration_since(at) < REPEAT_WINDOW
        {
            return false;
        }
        self.last = Some((event, counter, now));
        true
    }
}

pub struct Listener {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// One UDP listener on `port`, filtered to `scanner`.
pub fn listen(scanner: Ipv4Addr, port: u16, tx: Sender<Event>) -> Result<Listener> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port))?;
    socket.set_read_timeout(Some(Duration::from_millis(200)))?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread = thread::spawn(move || {
        let mut dedupe = Dedupe::default();
        let mut buffer = [0u8; 2048];
        while !thread_stop.load(Ordering::Relaxed) {
            match socket.recv_from(&mut buffer) {
                Ok((size, sender)) => {
                    let sender_ip = match sender.ip() {
                        IpAddr::V4(ip) => ip,
                        IpAddr::V6(_) => continue,
                    };
                    if sender_ip != scanner {
                        continue;
                    }
                    if let Some((event, counter)) = parse_notification(&buffer[..size], sender_ip)
                        && dedupe.accept(event, counter)
                        && tx.send(event).is_err()
                    {
                        return;
                    }
                }
                Err(err)
                    if matches!(
                        err.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => return,
            }
        }
    });
    Ok(Listener {
        stop,
        thread: Some(thread),
    })
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
