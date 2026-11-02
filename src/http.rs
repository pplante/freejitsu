use std::io::{self, Read, Write};
use std::net::TcpStream;

pub const MAX_HEAD: usize = 16 * 1024;
pub const MAX_BODY: usize = 1024 * 1024;

pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug)]
pub enum RequestError {
    Eof,
    Io(io::Error),
    Malformed(&'static str),
    TooLarge,
}

pub struct Response {
    pub status: u16,
    pub content_type: Option<&'static str>,
    pub body: Vec<u8>,
    pub headers: Vec<(String, String)>,
}

impl Response {
    pub fn empty(status: u16) -> Self {
        Response {
            status,
            content_type: None,
            body: Vec::new(),
            headers: Vec::new(),
        }
    }

    pub fn xml(body: String) -> Self {
        Response::bytes(200, "text/xml", body.into_bytes())
    }

    pub fn bytes(status: u16, content_type: &'static str, body: Vec<u8>) -> Self {
        Response {
            status,
            content_type: Some(content_type),
            body,
            headers: Vec::new(),
        }
    }

    pub fn header(mut self, name: &str, value: String) -> Self {
        self.headers.push((name.to_string(), value));
        self
    }
}

pub fn read_request(stream: &mut TcpStream) -> Result<Request, RequestError> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) if head.is_empty() => return Err(RequestError::Eof),
            Ok(0) => return Err(RequestError::Malformed("incomplete head")),
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(RequestError::Io(err)),
        }
        if byte[0] == b'\n' && head.last() != Some(&b'\r') {
            return Err(RequestError::Malformed("bare line feed"));
        }
        head.push(byte[0]);
        if head.len() > MAX_HEAD {
            return Err(RequestError::TooLarge);
        }
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
    }

    let text =
        std::str::from_utf8(&head).map_err(|_| RequestError::Malformed("head is not utf-8"))?;
    let mut lines = text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or(RequestError::Malformed("missing request line"))?;
    let mut words = request_line.split_whitespace();
    let method = words
        .next()
        .ok_or(RequestError::Malformed("missing method"))?
        .to_string();
    let target = words
        .next()
        .ok_or(RequestError::Malformed("missing target"))?;
    let version = words
        .next()
        .ok_or(RequestError::Malformed("missing version"))?;
    if !version.starts_with("HTTP/1.") {
        return Err(RequestError::Malformed("unsupported version"));
    }

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(RequestError::Malformed("header without a colon"));
        };
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }

    let request = Request {
        method,
        path: target.split('?').next().unwrap_or_default().to_string(),
        headers,
        body: Vec::new(),
    };

    if let Some(encoding) = request.header("transfer-encoding")
        && !encoding.eq_ignore_ascii_case("identity")
    {
        return Err(RequestError::Malformed("transfer encoding"));
    }

    let length = match request.header("content-length") {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| RequestError::Malformed("bad content length"))?,
        None => 0,
    };
    if length > MAX_BODY {
        return Err(RequestError::TooLarge);
    }

    if request
        .header("expect")
        .is_some_and(|value| value.eq_ignore_ascii_case("100-continue"))
    {
        stream
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .map_err(RequestError::Io)?;
    }

    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).map_err(|err| {
        if err.kind() == io::ErrorKind::UnexpectedEof {
            RequestError::Malformed("incomplete body")
        } else {
            RequestError::Io(err)
        }
    })?;

    Ok(Request { body, ..request })
}

pub fn write_response(
    stream: &mut TcpStream,
    head_only: bool,
    response: &Response,
) -> io::Result<()> {
    let mut out = Vec::new();
    out.extend_from_slice(
        format!(
            "HTTP/1.1 {} {}\r\n",
            response.status,
            reason(response.status)
        )
        .as_bytes(),
    );
    out.extend_from_slice(b"Server: freejitsu\r\n");
    if let Some(content_type) = response.content_type {
        out.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
    }
    for (name, value) in &response.headers {
        out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    out.extend_from_slice(format!("Content-Length: {}\r\n", response.body.len()).as_bytes());
    out.extend_from_slice(b"Connection: close\r\n\r\n");
    if !head_only {
        out.extend_from_slice(&response.body);
    }
    stream.write_all(&out)
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}
