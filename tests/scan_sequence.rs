mod support;

use std::net::Ipv4Addr;

use freejitsu::config::{Dpi, PageSize};
use freejitsu::error::Error;
use freejitsu::scan::{Sheet, info_cdb, param_block, read_cdb, scan, window_block};
use freejitsu::scsi::{ScsiChannel, release};

const MAC: [u8; 6] = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];

fn info(scan_port: u16) -> freejitsu::discovery::ScannerInfo {
    support::scanner_info(Ipv4Addr::LOCALHOST, scan_port, 0)
}

fn sheet(front: &[u8], back: &[u8]) -> support::FakeSheet {
    support::FakeSheet::new(front.to_vec(), back.to_vec())
}

#[test]
fn scans_two_sheets_in_order_and_releases_on_a_second_connection() {
    let fake = support::FakeScan::start(support::ScanConfig {
        sheets: vec![sheet(b"front-0", b"back-0"), sheet(b"front-1", b"back-1")],
        ..support::ScanConfig::default()
    });
    let info = info(fake.port);

    let mut channel = ScsiChannel::open(&info, MAC).expect("open scan");
    let mut sheets: Vec<Sheet> = Vec::new();
    let count = scan(&mut channel, Dpi::D300, PageSize::Auto, &mut |sheet| {
        sheets.push(sheet);
        Ok(())
    })
    .expect("scan");
    drop(channel);
    release(&info, MAC).expect("release");

    assert_eq!(count, 2);
    assert_eq!(sheets.len(), 2);
    assert_eq!(sheets[0].front, b"front-0");
    assert_eq!(sheets[0].back, b"back-0");
    assert_eq!(sheets[1].front, b"front-1");
    assert_eq!(sheets[1].back, b"back-1");
    assert_eq!(sheets[0].info.width, 300);
    assert_eq!(sheets[0].info.height, 400);
    assert_eq!(sheets[0].info.dpi_x, 300);

    let log = fake.snapshot();
    let order: Vec<u8> = log.cdbs.iter().map(|cdb| cdb[0]).collect();
    assert_eq!(
        order,
        vec![
            0xc2, 0xd5, 0xd8, 0xe9, 0xd4, 0xc2, 0x03, // open session
            0xe0, 0x28, 0x03, 0x28, 0x03, 0x28, 0xc2, // first sheet
            0xe0, 0x28, 0x03, 0x28, 0x03, 0x28, 0xc2, // second sheet
            0xe0, 0x03, // feeder empty
            0xd6, // release on a new connection
        ]
    );
    assert_eq!(log.connections, 2);
    assert!(log.macs.iter().all(|mac| *mac == MAC));

    assert_eq!(log.cdbs[8], read_cdb(false, 0));
    assert_eq!(log.cdbs[9], [0x03, 0, 0, 0, 0x12, 0]);
    assert_eq!(log.cdbs[10], read_cdb(true, 1));
    assert_eq!(log.cdbs[15], read_cdb(false, 2));
    assert_eq!(log.cdbs[12], info_cdb(1));
    assert_eq!(log.cdbs[3], [0xe9, 0, 0, 0, 0, 0, 0, 0x20, 0, 0]);
    assert_eq!(log.cdbs[4], [0xd4, 0, 0, 0, 0x50, 0]);
    assert_eq!(log.out[3], window_block(Dpi::D300, PageSize::Auto));
    assert_eq!(log.out[4], param_block(Dpi::D300, PageSize::Auto));
    assert_eq!(log.out[1], [0u8; 8]);
}

#[test]
fn joins_multi_chunk_images() {
    let fake = support::FakeScan::start(support::ScanConfig {
        sheets: vec![support::FakeSheet::chunked(
            vec![vec![1, 2, 3], vec![4, 5]],
            vec![vec![9], vec![8, 7]],
        )],
        ..support::ScanConfig::default()
    });
    let info = info(fake.port);

    let mut channel = ScsiChannel::open(&info, MAC).expect("open scan");
    let mut sheets: Vec<Sheet> = Vec::new();
    let count = scan(&mut channel, Dpi::D300, PageSize::Auto, &mut |sheet| {
        sheets.push(sheet);
        Ok(())
    })
    .expect("scan");
    drop(channel);

    assert_eq!(count, 1);
    assert_eq!(sheets[0].front, vec![1, 2, 3, 4, 5]);
    assert_eq!(sheets[0].back, vec![9, 8, 7]);
}

#[test]
fn jam_after_one_sheet_returns_a_feed_error() {
    let fake = support::FakeScan::start(support::ScanConfig {
        sheets: vec![sheet(b"front", b"back"), sheet(b"lost", b"lost")],
        jam_at: vec![1],
        ..support::ScanConfig::default()
    });
    let info = info(fake.port);

    let mut channel = ScsiChannel::open(&info, MAC).expect("open scan");
    let mut sheets: Vec<Sheet> = Vec::new();
    let result = scan(&mut channel, Dpi::D300, PageSize::Auto, &mut |sheet| {
        sheets.push(sheet);
        Ok(())
    });
    drop(channel);

    assert!(matches!(result, Err(Error::Feed("paper jam"))));
    assert_eq!(sheets.len(), 1);
}

#[test]
fn empty_feeder_is_no_paper() {
    let fake = support::FakeScan::start(support::ScanConfig::default());
    let info = info(fake.port);

    let mut channel = ScsiChannel::open(&info, MAC).expect("open scan");
    let result = scan(&mut channel, Dpi::D300, PageSize::Auto, &mut |_| Ok(()));
    drop(channel);

    assert!(matches!(result, Err(Error::NoPaper)));
}
