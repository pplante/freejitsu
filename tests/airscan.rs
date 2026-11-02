mod support;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

use freejitsu::airscan::{AirScan, ScanLock, Session};
use freejitsu::config::{Config, Dpi, PageSize, Ports};
use freejitsu::identity::Identity;

fn test_addr(service: &AirScan) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, service.local_addr().port()))
}

fn config(dir: &Path) -> Config {
    Config {
        ip: Ipv4Addr::LOCALHOST,
        out_dir: dir.join("out"),
        dpi: Dpi::D300,
        page: PageSize::Auto,
        simplex: false,
        identity: dir.join("identity"),
        password: None,
        once: false,
        ports: Ports {
            discovery: 0,
            startup: 0,
        },
        airscan: true,
        airscan_port: 0,
    }
}

fn identity() -> Identity {
    Identity {
        host_id: [0; 8],
        mac: [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
        host_port: 55265,
        password: None,
    }
}

#[test]
fn capabilities_and_status_need_a_session() {
    let dir = support::temp_dir("airscan-no-session");
    let lock = Arc::new(ScanLock::new());
    let service = AirScan::start(config(&dir), Arc::clone(&lock)).unwrap();
    let addr = test_addr(&service);
    assert_eq!(
        support::http_get(addr, "/eSCL/ScannerCapabilities").status,
        503
    );
    assert_eq!(support::http_get(addr, "/eSCL/ScannerStatus").status, 503);
}

#[test]
fn capabilities_and_status_appear_after_registration() {
    let dir = support::temp_dir("airscan-session");
    let lock = Arc::new(ScanLock::new());
    let service = AirScan::start(config(&dir), Arc::clone(&lock)).unwrap();
    service.set_session(Some(Session {
        info: support::scanner_info(Ipv4Addr::LOCALHOST, 1, 1),
        identity: identity(),
    }));
    let addr = test_addr(&service);

    let caps = support::http_get(addr, "/eSCL/ScannerCapabilities");
    assert_eq!(caps.status, 200);
    let caps = String::from_utf8(caps.body).unwrap();
    assert!(caps.contains("<pwg:MakeAndModel>ScanSnap iX500</pwg:MakeAndModel>"));
    assert!(caps.contains("<scan:SerialNumber>A0XX012345</scan:SerialNumber>"));

    let status = String::from_utf8(support::http_get(addr, "/eSCL/ScannerStatus").body).unwrap();
    assert!(status.contains("<pwg:State>Idle</pwg:State>"));
    assert!(status.contains("<scan:AdfState>ScannerAdfLoaded</scan:AdfState>"));
}

#[test]
fn status_reports_processing_while_the_lock_is_held() {
    let dir = support::temp_dir("airscan-processing");
    let lock = Arc::new(ScanLock::new());
    let service = AirScan::start(config(&dir), Arc::clone(&lock)).unwrap();
    service.set_session(Some(Session {
        info: support::scanner_info(Ipv4Addr::LOCALHOST, 1, 1),
        identity: identity(),
    }));
    let guard = lock.try_acquire().expect("lock");
    let status =
        String::from_utf8(support::http_get(test_addr(&service), "/eSCL/ScannerStatus").body)
            .unwrap();
    assert!(status.contains("<pwg:State>Processing</pwg:State>"));
    drop(guard);
    let status =
        String::from_utf8(support::http_get(test_addr(&service), "/eSCL/ScannerStatus").body)
            .unwrap();
    assert!(status.contains("<pwg:State>Idle</pwg:State>"));
}

#[test]
fn the_scan_lock_is_exclusive() {
    let lock = Arc::new(ScanLock::new());
    let first = lock.try_acquire().expect("first");
    assert!(lock.busy());
    assert!(lock.try_acquire().is_none());
    drop(first);
    assert!(!lock.busy());
    assert!(lock.try_acquire().is_some());
}

#[test]
fn unknown_paths_are_not_found() {
    let dir = support::temp_dir("airscan-404");
    let lock = Arc::new(ScanLock::new());
    let service = AirScan::start(config(&dir), Arc::clone(&lock)).unwrap();
    assert_eq!(support::http_get(test_addr(&service), "/nope").status, 404);
}
