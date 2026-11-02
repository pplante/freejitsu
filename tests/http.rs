use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use freejitsu::http::{Request, RequestError, Response, read_request, write_response};

fn parse_on_loopback(input: &[u8]) -> Result<Request, RequestError> {
    let input = input.to_vec();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let writer = thread::spawn(move || {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.write_all(&input).unwrap();
        thread::sleep(Duration::from_millis(100));
    });
    let (mut server, _) = listener.accept().unwrap();
    let request = read_request(&mut server);
    writer.join().unwrap();
    request
}

#[test]
fn reads_a_get_request() {
    let request =
        parse_on_loopback(b"GET /eSCL/ScannerStatus?x=1 HTTP/1.1\r\nHost: 127.0.0.1:8090\r\n\r\n")
            .unwrap();
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/eSCL/ScannerStatus");
    assert_eq!(request.header("host"), Some("127.0.0.1:8090"));
    assert!(request.body.is_empty());
}

#[test]
fn reads_a_post_body() {
    let request =
        parse_on_loopback(b"POST /eSCL/ScanJobs HTTP/1.1\r\nContent-Length: 5\r\n\r\nhello")
            .unwrap();
    assert_eq!(request.method, "POST");
    assert_eq!(request.body, b"hello");
}

#[test]
fn rejects_a_short_terminator() {
    assert!(matches!(
        parse_on_loopback(b"GET / HTTP/1.1\n\n"),
        Err(RequestError::Malformed(_))
    ));
}

#[test]
fn rejects_a_body_over_one_megabyte() {
    assert!(matches!(
        parse_on_loopback(b"POST /eSCL/ScanJobs HTTP/1.1\r\nContent-Length: 1048577\r\n\r\n"),
        Err(RequestError::TooLarge)
    ));
}

#[test]
fn rejects_a_head_over_sixteen_kib() {
    let mut input = b"GET / HTTP/1.1\r\nX-Fill: ".to_vec();
    input.extend_from_slice(&[b'a'; 17 * 1024]);
    input.extend_from_slice(b"\r\n\r\n");
    assert!(matches!(
        parse_on_loopback(&input),
        Err(RequestError::TooLarge)
    ));
}

#[test]
fn answers_expect_continue_before_the_body() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let client = thread::spawn(move || {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .write_all(b"POST /eSCL/ScanJobs HTTP/1.1\r\nContent-Length: 5\r\nExpect: 100-continue\r\n\r\n")
            .unwrap();
        let mut line = [0u8; 25];
        stream.read_exact(&mut line).unwrap();
        assert_eq!(&line, b"HTTP/1.1 100 Continue\r\n\r\n");
        stream.write_all(b"hello").unwrap();
    });
    let (mut server, _) = listener.accept().unwrap();
    let request = read_request(&mut server).unwrap();
    assert_eq!(request.body, b"hello");
    client.join().unwrap();
}

fn response_bytes(head_only: bool) -> Vec<u8> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        write_response(&mut stream, head_only, &Response::xml("<x/>".into())).unwrap();
    });
    let mut stream = TcpStream::connect(addr).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    server.join().unwrap();
    raw
}

#[test]
fn writes_a_response_with_a_body() {
    let raw = response_bytes(false);
    let text = String::from_utf8_lossy(&raw);
    assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(text.contains("Content-Length: 4\r\n"));
    assert!(text.contains("Connection: close\r\n"));
    assert!(text.contains("Server: freejitsu\r\n"));
    assert!(raw.ends_with(b"\r\n\r\n<x/>"));
}

#[test]
fn writes_a_head_response_without_a_body() {
    let raw = response_bytes(true);
    let text = String::from_utf8_lossy(&raw);
    assert!(text.contains("Content-Length: 4\r\n"));
    assert!(raw.ends_with(b"\r\n\r\n"));
}
