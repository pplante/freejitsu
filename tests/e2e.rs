mod support;

use std::fs;
use std::net::Ipv4Addr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use freejitsu::config::{Config, Dpi, PageSize, Ports};
use freejitsu::control::encode_password;
use freejitsu::daemon;
use freejitsu::error::Error;
use freejitsu::identity;
use freejitsu::scan::Area;

fn fixture() -> Vec<u8> {
    fs::read("tests/fixtures/page.jpg").expect("page.jpg")
}

fn pdfs(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.extension().is_some_and(|ext| ext == "pdf"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

fn config(scanner: &support::FakeScanner, dir: &Path, simplex: bool) -> Config {
    Config {
        ip: std::net::Ipv4Addr::LOCALHOST,
        out_dir: dir.join("out"),
        dpi: Dpi::D300,
        page: PageSize::Auto,
        simplex,
        identity: dir.join("identity"),
        password: Some("2345".into()),
        once: true,
        ports: Ports {
            discovery: scanner.discovery.port,
            startup: 0,
        },
    }
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

#[test]
fn once_registers_scans_and_writes_a_pdf() {
    let jpeg = fixture();
    let scanner = support::FakeScanner::start(
        support::ScanConfig {
            sheets: vec![support::FakeSheet::new(jpeg.clone(), jpeg.clone())],
            ..support::ScanConfig::default()
        },
        0,
        6,
    );
    let dir = support::temp_dir("e2e-happy");
    let cfg = config(&scanner, &dir, false);
    daemon::run(cfg).expect("once run");

    let files = pdfs(&dir.join("out"));
    assert_eq!(files.len(), 1, "one PDF expected, got {files:?}");
    let bytes = fs::read(&files[0]).expect("read PDF");
    assert!(bytes.starts_with(b"%PDF-1.4"));
    assert!(bytes.windows(8).any(|window| window == b"/Count 2"));
    assert_eq!(count(&bytes, b"/Rotate 180"), 2);
    assert!(
        bytes
            .windows(21)
            .any(|window| window == b"/MediaBox [0 0 72 96]")
    );
    assert_eq!(count(&bytes, &jpeg), 2);

    let control = scanner.control.snapshot();
    assert_eq!(control.commands, vec![0x11], "one registration");
    let body = &control.bodies[0];
    assert_eq!(&body[76..84], &scanner.paired_host_id);
    assert_eq!(&body[12..16], &[127, 0, 0, 1]);
    assert_eq!(&body[16..20], &55265u32.to_be_bytes());
    let password = encode_password("2345").expect("encode");
    assert_eq!(
        std::str::from_utf8(&body[20..20 + password.len()]).expect("ascii"),
        password
    );

    let stored = identity::load(&dir.join("identity"))
        .expect("load identity")
        .expect("identity exists");
    assert_eq!(stored.host_id, scanner.paired_host_id);
    assert_eq!(stored.password.as_deref(), Some("2345"));
    assert_eq!(&body[16..20], &u32::from(stored.host_port).to_be_bytes());
    assert_eq!(control.macs, vec![stored.mac]);

    let mode = fs::metadata(dir.join("identity"))
        .expect("identity metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);

    let log = scanner.scan.snapshot();
    let order: Vec<u8> = log.cdbs.iter().map(|cdb| cdb[0]).collect();
    assert_eq!(
        order,
        vec![
            0xc2, 0xd5, 0xd8, 0xe9, 0xd4, 0xc2, 0x03, 0xe0, 0x28, 0x03, 0x28, 0x03, 0x28, 0xc2,
            0xe0, 0x03, 0xd6,
        ]
    );
    assert_eq!(log.connections, 2);
    assert!(log.macs.iter().all(|mac| *mac == stored.mac));
}

#[test]
fn jam_after_one_sheet_leaves_a_one_page_pdf() {
    let jpeg = fixture();
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
    let dir = support::temp_dir("e2e-jam");
    let cfg = config(&scanner, &dir, true);
    daemon::run(cfg).expect("jam run");

    let files = pdfs(&dir.join("out"));
    assert_eq!(files.len(), 1, "one PDF expected, got {files:?}");
    let bytes = fs::read(&files[0]).expect("read PDF");
    assert!(bytes.windows(8).any(|window| window == b"/Count 1"));
    assert_eq!(count(&bytes, b"/Type /Page "), 1);
    assert_eq!(count(&bytes, &jpeg), 1);
}

#[test]
fn empty_feeder_leaves_no_file() {
    let scanner = support::FakeScanner::start(support::ScanConfig::default(), 0, 6);
    let dir = support::temp_dir("e2e-empty");
    let cfg = config(&scanner, &dir, true);
    let err = daemon::run(cfg).expect_err("no paper");
    assert!(matches!(err, Error::NoPaper));
    assert!(pdfs(&dir.join("out")).is_empty());
}

#[test]
fn registration_status_is_read_at_reply_offset_eight() {
    for (status, needle) in [(-2, "Wi-Fi password"), (-7, "USB")] {
        let scanner = support::FakeScanner::start(support::ScanConfig::default(), status, 6);
        let dir = support::temp_dir("e2e-status");
        let cfg = config(&scanner, &dir, false);
        let err = daemon::run(cfg).expect_err("registration rejected");
        let Error::Registration(got) = &err else {
            panic!("expected a registration error, got {err}");
        };
        assert_eq!(*got, status);
        let text = err.to_string();
        assert!(text.contains(needle), "{status} message was {text:?}");
    }
}

fn session(
    scanner: &support::FakeScanner,
    dir: &Path,
    simplex: bool,
) -> (
    Config,
    freejitsu::discovery::ScannerInfo,
    freejitsu::identity::Identity,
) {
    let cfg = config(scanner, dir, simplex);
    let info = support::scanner_info(Ipv4Addr::LOCALHOST, scanner.scan.port, scanner.control.port);
    let identity = freejitsu::identity::Identity {
        host_id: scanner.paired_host_id,
        mac: scanner.mac,
        host_port: 55265,
        password: None,
    };
    (cfg, info, identity)
}

#[test]
fn capture_cancel_drops_the_partial_pdf() {
    let jpeg = fixture();
    let scanner = support::FakeScanner::start(
        support::ScanConfig {
            sheets: vec![
                support::FakeSheet::new(jpeg.clone(), jpeg.clone()),
                support::FakeSheet::new(jpeg.clone(), jpeg.clone()),
            ],
            ..support::ScanConfig::default()
        },
        0,
        6,
    );
    let dir = support::temp_dir("e2e-cancel");
    let (cfg, info, identity) = session(&scanner, &dir, false);

    let err = daemon::capture(
        &cfg,
        &info,
        &identity,
        Dpi::D300,
        Area::from(PageSize::Auto),
        false,
        &mut |_: &freejitsu::scan::Sheet| Err(Error::Canceled),
    )
    .expect_err("cancel");

    assert!(matches!(err, Error::Canceled));
    assert!(pdfs(&dir.join("out")).is_empty());
    let log = scanner.scan.snapshot();
    assert_eq!(log.connections, 2, "scan plus the D6 release connection");
    assert_eq!(log.cdbs.last().map(|cdb| cdb[0]), Some(0xd6));
}

#[test]
fn capture_reports_a_jam_after_pages_as_early() {
    let jpeg = fixture();
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
    let dir = support::temp_dir("e2e-early");
    let (cfg, info, identity) = session(&scanner, &dir, true);

    let capture = daemon::capture(
        &cfg,
        &info,
        &identity,
        Dpi::D300,
        Area::from(PageSize::Auto),
        true,
        &mut |_| Ok(()),
    )
    .expect("partial PDF");

    assert!(matches!(capture.early, Some(Error::Feed("paper jam"))));
    assert!(capture.path.exists());
}

#[test]
fn capture_takes_the_dpi_and_area_from_the_caller() {
    let jpeg = fixture();
    let scanner = support::FakeScanner::start(
        support::ScanConfig {
            sheets: vec![support::FakeSheet::new(jpeg.clone(), jpeg.clone())],
            ..support::ScanConfig::default()
        },
        0,
        6,
    );
    let dir = support::temp_dir("e2e-dpi");
    let (cfg, info, identity) = session(&scanner, &dir, true);

    daemon::capture(
        &cfg,
        &info,
        &identity,
        Dpi::D600,
        Area::from(PageSize::A4),
        true,
        &mut |_| Ok(()),
    )
    .expect("scan");

    let log = scanner.scan.snapshot();
    assert_eq!(
        log.out[3],
        freejitsu::scan::window_block(Dpi::D600, PageSize::A4.into())
    );
}
