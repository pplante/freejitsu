use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::error::{Error, Result};

pub const MAGIC: [u8; 4] = *b"VENS";
pub const CLIENT_HEADER_LEN: usize = 32;
pub const SERVER_HEADER_LEN: usize = 16;
/// One image read plus headers, with headroom.
pub const MAX_FRAME: usize = 0x40_0000;

/// Client frame: `u32 length | VENS | u32 command | u32 0 | 6 B MAC | u16 0 | u32 0 | u32 0`.
pub fn client_frame(command: u32, mac: [u8; 6], body: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(CLIENT_HEADER_LEN + body.len());
    frame.extend_from_slice(&((CLIENT_HEADER_LEN + body.len()) as u32).to_be_bytes());
    frame.extend_from_slice(&MAGIC);
    frame.extend_from_slice(&command.to_be_bytes());
    frame.extend_from_slice(&0u32.to_be_bytes());
    frame.extend_from_slice(&mac);
    frame.extend_from_slice(&0u16.to_be_bytes());
    frame.extend_from_slice(&0u32.to_be_bytes());
    frame.extend_from_slice(&0u32.to_be_bytes());
    frame.extend_from_slice(body);
    frame
}

/// Server frame: `u32 length | VENS | u32 0 | i32 status`.
pub fn server_frame(body: &[u8], status: i32) -> Vec<u8> {
    let mut frame = Vec::with_capacity(SERVER_HEADER_LEN + body.len());
    frame.extend_from_slice(&((SERVER_HEADER_LEN + body.len()) as u32).to_be_bytes());
    frame.extend_from_slice(&MAGIC);
    frame.extend_from_slice(&0u32.to_be_bytes());
    frame.extend_from_slice(&status.to_be_bytes());
    frame.extend_from_slice(body);
    frame
}

/// Status word in the server header.
pub fn frame_status(frame: &[u8]) -> i32 {
    if frame.len() < SERVER_HEADER_LEN {
        return -1;
    }
    let mut word = [0u8; 4];
    word.copy_from_slice(&frame[12..16]);
    i32::from_be_bytes(word)
}

pub struct Channel {
    stream: TcpStream,
    mac: [u8; 6],
}

impl Channel {
    /// Connects and consumes the 16 byte greeting.
    pub fn connect(addr: SocketAddr, mac: [u8; 6], timeout: Duration) -> Result<Self> {
        let stream = TcpStream::connect_timeout(&addr, timeout)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let mut channel = Channel { stream, mac };
        let greeting = channel.read_frame()?;
        if greeting.len() != SERVER_HEADER_LEN {
            return Err(Error::Protocol(format!(
                "unexpected greeting of {} bytes",
                greeting.len()
            )));
        }
        Ok(channel)
    }

    /// Sends one command and returns the complete server frame, header included.
    pub fn call(&mut self, command: u32, body: &[u8]) -> Result<Vec<u8>> {
        let frame = client_frame(command, self.mac, body);
        self.stream.write_all(&frame)?;
        self.read_frame()
    }

    fn read_frame(&mut self) -> Result<Vec<u8>> {
        let mut head = [0u8; 8];
        self.stream.read_exact(&mut head)?;
        let length = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        if head[4..8] != MAGIC {
            return Err(Error::Protocol(format!("bad magic {:02x?}", &head[4..8])));
        }
        if !(SERVER_HEADER_LEN..=MAX_FRAME).contains(&length) {
            return Err(Error::Protocol(format!("bad frame length {length}")));
        }
        let mut frame = Vec::with_capacity(length);
        frame.extend_from_slice(&head);
        frame.resize(length, 0);
        self.stream.read_exact(&mut frame[head.len()..])?;
        debug_assert_eq!(frame.len(), length);
        Ok(frame)
    }
}
