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

const DUPLEX_A4_JPEG: &str = r#"<?xml version="1.0"?>
<scan:ScanSettings xmlns:scan="http://schemas.hp.com/imaging/escl/2011/05/03"
                   xmlns:pwg="http://www.pwg.org/schemas/2010/12/sm">
  <scan:InputSource>Feeder</scan:InputSource>
  <scan:ColorMode>RGB24</scan:ColorMode>
  <scan:XResolution>300</scan:XResolution>
  <scan:Duplex>true</scan:Duplex>
  <pwg:ScanRegions><pwg:ScanRegion>
    <pwg:Width>2480</pwg:Width><pwg:Height>3508</pwg:Height>
  </pwg:ScanRegion></pwg:ScanRegions>
  <pwg:DocumentFormat>image/jpeg</pwg:DocumentFormat>
</scan:ScanSettings>"#;

const SIMPLEX_JPEG: &str = r#"<ScanSettings>
  <InputSource>Feeder</InputSource>
  <ColorMode>RGB24</ColorMode>
  <XResolution>300</XResolution>
  <Duplex>false</Duplex>
  <DocumentFormat>image/jpeg</DocumentFormat>
</ScanSettings>"#;

const SIMPLEX_PDF: &str = r#"<ScanSettings>
  <InputSource>Feeder</InputSource>
  <ColorMode>RGB24</ColorMode>
  <XResolution>300</XResolution>
  <Duplex>false</Duplex>
  <DocumentFormatExt>application/pdf</DocumentFormatExt>
</ScanSettings>"#;

fn job_uri(response: &support::HttpResponse) -> String {
    let location = response.header("location").expect("location");
    let path = location.find("/eSCL/").expect("job path");
    location[path..].to_string()
}

fn pdfs(dir: &Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.extension().is_some_and(|ext| ext == "pdf"))
                .collect()
        })
        .unwrap_or_default()
}

fn start_job_service(scanner: &support::FakeScanner, dir: &Path) -> (Arc<AirScan>, Arc<ScanLock>) {
    let lock = Arc::new(ScanLock::new());
    let service = AirScan::start(config(dir), Arc::clone(&lock)).unwrap();
    service.set_session(Some(Session {
        info: support::scanner_info(Ipv4Addr::LOCALHOST, scanner.scan.port, scanner.control.port),
        identity: Identity {
            host_id: scanner.paired_host_id,
            mac: scanner.mac,
            host_port: 55265,
            password: None,
        },
    }));
    (service, lock)
}

#[test]
fn jpeg_job_delivers_each_side_and_writes_a_pdf() {
    let jpeg = std::fs::read("tests/fixtures/page.jpg").unwrap();
    let scanner = support::FakeScanner::start(
        support::ScanConfig {
            sheets: vec![support::FakeSheet::new(jpeg.clone(), jpeg.clone())],
            ..support::ScanConfig::default()
        },
        0,
        6,
    );
    let dir = support::temp_dir("airscan-jpeg");
    let (service, _lock) = start_job_service(&scanner, &dir);
    let addr = test_addr(&service);

    let created = support::http_post(addr, "/eSCL/ScanJobs", DUPLEX_A4_JPEG.as_bytes());
    assert_eq!(created.status, 201);
    let uri = job_uri(&created);

    let first = support::http_get(addr, &format!("{uri}/NextDocument"));
    assert_eq!(first.status, 200);
    assert_eq!(first.header("content-type"), Some("image/jpeg"));
    assert!(first.body.starts_with(&[0xff, 0xd8]));
    assert_eq!(&first.body[2..4], &[0xff, 0xe1]);

    let second = support::http_get(addr, &format!("{uri}/NextDocument"));
    assert_eq!(second.status, 200);
    assert!(second.body.starts_with(&[0xff, 0xd8]));
    assert_eq!(
        support::http_get(addr, &format!("{uri}/NextDocument")).status,
        404
    );

    assert_eq!(pdfs(&dir.join("out")).len(), 1);

    let log = scanner.scan.snapshot();
    assert_eq!(
        log.out[3],
        freejitsu::scan::window_block(
            Dpi::D300,
            freejitsu::scan::Area {
                width: 9920,
                height: 14032,
                automatic: false
            },
        )
    );
    assert_eq!(
        log.out[4],
        freejitsu::scan::param_block(
            Dpi::D300,
            freejitsu::scan::Area {
                width: 9920,
                height: 14032,
                automatic: false
            },
        )
    );
}

#[test]
fn pdf_job_delivers_one_document() {
    let jpeg = std::fs::read("tests/fixtures/page.jpg").unwrap();
    let scanner = support::FakeScanner::start(
        support::ScanConfig {
            sheets: vec![support::FakeSheet::new(jpeg.clone(), jpeg.clone())],
            ..support::ScanConfig::default()
        },
        0,
        6,
    );
    let dir = support::temp_dir("airscan-pdf");
    let (service, _lock) = start_job_service(&scanner, &dir);
    let addr = test_addr(&service);

    let created = support::http_post(addr, "/eSCL/ScanJobs", SIMPLEX_PDF.as_bytes());
    assert_eq!(created.status, 201);
    let uri = job_uri(&created);
    let doc = support::http_get(addr, &format!("{uri}/NextDocument"));
    assert_eq!(doc.status, 200);
    assert_eq!(doc.header("content-type"), Some("application/pdf"));
    assert!(doc.body.starts_with(b"%PDF-1.4"));
    assert!(doc.body.windows(8).any(|window| window == b"/Count 1"));
    assert!(doc.body.windows(11).any(|window| window == b"/Rotate 180"));
    assert_eq!(
        support::http_get(addr, &format!("{uri}/NextDocument")).status,
        404
    );
    assert_eq!(
        support::http_delete(addr, "/eSCL/ScanJobs/missing").status,
        404
    );
    assert_eq!(support::http_delete(addr, &uri).status, 200);
}

#[test]
fn empty_feeder_is_conflict() {
    let scanner = support::FakeScanner::start(support::ScanConfig::default(), 0, 6);
    let dir = support::temp_dir("airscan-empty");
    let (service, _lock) = start_job_service(&scanner, &dir);
    let created = support::http_post(
        test_addr(&service),
        "/eSCL/ScanJobs",
        SIMPLEX_PDF.as_bytes(),
    );
    assert_eq!(created.status, 409);
    assert!(pdfs(&dir.join("out")).is_empty());
}

#[test]
fn busy_scanner_is_unavailable() {
    let scanner = support::FakeScanner::start(support::ScanConfig::default(), 0, 6);
    let dir = support::temp_dir("airscan-busy");
    let (service, lock) = start_job_service(&scanner, &dir);
    let guard = lock.try_acquire().expect("hold");
    let created = support::http_post(
        test_addr(&service),
        "/eSCL/ScanJobs",
        SIMPLEX_PDF.as_bytes(),
    );
    assert_eq!(created.status, 503);
    drop(guard);
}

#[test]
fn no_session_is_unavailable() {
    let dir = support::temp_dir("airscan-offline");
    let lock = Arc::new(ScanLock::new());
    let service = AirScan::start(config(&dir), Arc::clone(&lock)).unwrap();
    let created = support::http_post(
        test_addr(&service),
        "/eSCL/ScanJobs",
        SIMPLEX_PDF.as_bytes(),
    );
    assert_eq!(created.status, 503);
}

#[test]
fn unsupported_settings_are_conflict() {
    let scanner = support::FakeScanner::start(support::ScanConfig::default(), 0, 6);
    let dir = support::temp_dir("airscan-unsupported");
    let (service, _lock) = start_job_service(&scanner, &dir);
    let created = support::http_post(
        test_addr(&service),
        "/eSCL/ScanJobs",
        b"<ScanSettings><InputSource>Platen</InputSource></ScanSettings>",
    );
    assert_eq!(created.status, 409);
}

#[test]
fn jam_after_one_sheet_reports_aborted_and_keeps_the_pdf() {
    let jpeg = std::fs::read("tests/fixtures/page.jpg").unwrap();
    let scanner = support::FakeScanner::start(
        support::ScanConfig {
            sheets: vec![
                support::FakeSheet::new(jpeg.clone(), jpeg.clone()),
                support::FakeSheet::new(jpeg.clone(), jpeg.clone()),
            ],
            jam_at: vec![1],
            ..support::ScanConfig::default()
        },
        0,
        6,
    );
    let dir = support::temp_dir("airscan-jam");
    let (service, _lock) = start_job_service(&scanner, &dir);
    let addr = test_addr(&service);

    let created = support::http_post(addr, "/eSCL/ScanJobs", SIMPLEX_JPEG.as_bytes());
    assert_eq!(created.status, 201);
    let uri = job_uri(&created);
    assert_eq!(
        support::http_get(addr, &format!("{uri}/NextDocument")).status,
        200
    );
    assert_eq!(
        support::http_get(addr, &format!("{uri}/NextDocument")).status,
        404
    );

    let mut aborted = false;
    for _ in 0..60 {
        let status =
            String::from_utf8(support::http_get(addr, "/eSCL/ScannerStatus").body).unwrap();
        if status.contains("MediaJam") {
            aborted = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(aborted, "job did not report a jam");
    assert_eq!(pdfs(&dir.join("out")).len(), 1);
}

#[test]
fn cancel_between_sheets_leaves_no_pdf() {
    let jpeg = std::fs::read("tests/fixtures/page.jpg").unwrap();
    let scanner = support::FakeScanner::start(
        support::ScanConfig {
            sheets: vec![
                support::FakeSheet::new(jpeg.clone(), jpeg.clone()),
                support::FakeSheet::new(jpeg.clone(), jpeg.clone()),
            ],
            delay: std::time::Duration::from_millis(800),
            ..support::ScanConfig::default()
        },
        0,
        6,
    );
    let dir = support::temp_dir("airscan-cancel");
    let (service, _lock) = start_job_service(&scanner, &dir);
    let addr = test_addr(&service);

    let created = support::http_post(addr, "/eSCL/ScanJobs", SIMPLEX_JPEG.as_bytes());
    assert_eq!(created.status, 201);
    let uri = job_uri(&created);
    assert_eq!(
        support::http_get(addr, &format!("{uri}/NextDocument")).status,
        200
    );
    assert_eq!(support::http_delete(addr, &uri).status, 200);

    let mut canceled = false;
    for _ in 0..60 {
        let status =
            String::from_utf8(support::http_get(addr, "/eSCL/ScannerStatus").body).unwrap();
        if status.contains("JobCanceledByUser") {
            canceled = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(canceled, "job did not report cancellation");
    assert!(pdfs(&dir.join("out")).is_empty());
}
