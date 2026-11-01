mod support;

use std::net::{Ipv4Addr, UdpSocket};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use freejitsu::notify::{Dedupe, Event, listen, parse_notification};

fn notification(event: u32, counter: u8) -> [u8; 48] {
    let mut data = [0u8; 48];
    data[0..4].copy_from_slice(&48u32.to_be_bytes());
    data[4..8].copy_from_slice(b"VENS");
    data[8..12].copy_from_slice(&event.to_be_bytes());
    data[16] = counter;
    data[20..24].copy_from_slice(&Ipv4Addr::LOCALHOST.octets());
    data
}

#[test]
fn parses_the_three_events() {
    assert_eq!(
        parse_notification(&notification(0x01, 7), Ipv4Addr::LOCALHOST),
        Some((Event::Button, 7))
    );
    assert_eq!(
        parse_notification(&notification(0x10, 7), Ipv4Addr::LOCALHOST),
        Some((Event::PowerOff, 7))
    );
    assert_eq!(
        parse_notification(&notification(0x21, 7), Ipv4Addr::LOCALHOST),
        Some((Event::PowerOn, 7))
    );
    assert_eq!(
        parse_notification(&notification(0x99, 7), Ipv4Addr::LOCALHOST),
        None
    );
    assert_eq!(
        parse_notification(&notification(0x01, 7)[..47], Ipv4Addr::LOCALHOST),
        None
    );
    assert_eq!(
        parse_notification(&notification(0x01, 7), Ipv4Addr::new(10, 0, 0, 1)),
        None
    );
}

#[test]
fn triple_notice_yields_one_event_and_a_new_counter_yields_another() {
    let now = Instant::now();
    let mut dedupe = Dedupe::default();
    assert!(dedupe.accept_at(Event::Button, 1, now));
    assert!(!dedupe.accept_at(Event::Button, 1, now + Duration::from_millis(500)));
    assert!(!dedupe.accept_at(Event::Button, 1, now + Duration::from_millis(1000)));
    assert!(dedupe.accept_at(Event::Button, 2, now + Duration::from_millis(1000)));
    assert!(dedupe.accept_at(Event::Button, 2, now + Duration::from_secs(5)));
}

#[test]
fn listener_delivers_one_event_for_a_triple_send() {
    let probe = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("probe socket");
    let port = probe.local_addr().expect("probe addr").port();
    drop(probe);

    let (tx, rx) = mpsc::channel();
    let _listener = listen(Ipv4Addr::LOCALHOST, port, tx).expect("listener");

    let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("sender");
    let packet = notification(0x01, 1);
    for _ in 0..3 {
        sender
            .send_to(&packet, (Ipv4Addr::LOCALHOST, port))
            .expect("send");
    }
    assert_eq!(rx.recv_timeout(Duration::from_secs(1)), Ok(Event::Button));
    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());

    sender
        .send_to(&notification(0x01, 2), (Ipv4Addr::LOCALHOST, port))
        .expect("send");
    assert_eq!(rx.recv_timeout(Duration::from_secs(1)), Ok(Event::Button));
}
