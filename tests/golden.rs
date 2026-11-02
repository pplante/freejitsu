use std::net::Ipv4Addr;

use freejitsu::config::{Dpi, PageSize};
use freejitsu::control::{WallClock, default_password, encode_password, register_body};
use freejitsu::identity::Identity;
use freejitsu::scan::{Area, info_cdb, param_block, read_cdb, window_block};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

const WINDOW_300_AUTO: &str = "012c012c000028d0000044dc0500000000000000000000000000000000000000";
const MODE_300_AUTO: &str = "00030101d000c1808080908080000000000000000000000000000000000000\
300010012c012c05810000000028d0000044dc040000000000000000000000000000000000000000000000000000000000";
const WINDOW_150_A4: &str = "00960096000026c1000036d00500000000000000000000000000000000000000";
const MODE_150_A4: &str = "00030000d000c1808080808080000000000000000000000000000000000000\
3000100096009605810500000026c1000036d0040000000000000000000000000000000000000000000000000000000000";
const WINDOW_600_A4: &str = "02580258000026c1000036d00500000000000000000000000000000000000000";
const MODE_600_A4: &str = "00030000d000c18080808080808000000000000000000000000000000000003000100258025805810a00000026c1000036d0040000000000000000000000000000000000000000000000000000000000";

#[test]
fn window_blocks_match_the_vectors() {
    assert_eq!(
        hex(&window_block(Dpi::D300, PageSize::Auto.into())),
        WINDOW_300_AUTO
    );
    assert_eq!(
        hex(&window_block(Dpi::D150, PageSize::A4.into())),
        WINDOW_150_A4
    );
    assert_eq!(
        hex(&window_block(Dpi::D600, PageSize::A4.into())),
        WINDOW_600_A4
    );
}

#[test]
fn mode_blocks_match_the_vectors() {
    assert_eq!(
        hex(&param_block(Dpi::D300, PageSize::Auto.into())),
        MODE_300_AUTO
    );
    assert_eq!(
        hex(&param_block(Dpi::D150, PageSize::A4.into())),
        MODE_150_A4
    );
    assert_eq!(
        hex(&param_block(Dpi::D600, PageSize::A4.into())),
        MODE_600_A4
    );
}

#[test]
fn arbitrary_area_sets_window_and_mode_fields() {
    let area = Area {
        width: 2480,
        height: 3508,
        automatic: false,
    };
    let window = window_block(Dpi::D200, area);
    assert_eq!(u16::from_be_bytes([window[0], window[1]]), 200);
    assert_eq!(u32::from_be_bytes(window[4..8].try_into().unwrap()), 2480);
    assert_eq!(u32::from_be_bytes(window[8..12].try_into().unwrap()), 3508);
    assert_eq!(window[12], 5);

    let mode = param_block(Dpi::D200, area);
    assert_eq!(mode[2], 0);
    assert_eq!(mode[3], 0);
    assert_eq!(u16::from_be_bytes(mode[34..36].try_into().unwrap()), 200);
    assert_eq!(u16::from_be_bytes(mode[36..38].try_into().unwrap()), 200);
    assert_eq!(u32::from_be_bytes(mode[42..46].try_into().unwrap()), 2480);
    assert_eq!(u32::from_be_bytes(mode[46..50].try_into().unwrap()), 3508);
}

#[test]
fn read_and_info_cdbs_match_the_vectors() {
    assert_eq!(hex(&read_cdb(false, 0)), "280000020000300000000000");
    assert_eq!(hex(&read_cdb(true, 1)), "280000020080300000000100");
    assert_eq!(hex(&info_cdb(1)), "280080000080000020000100");
}

#[test]
fn password_encoding_matches_the_vectors() {
    assert_eq!(encode_password("1234").unwrap(), "172131179178");
    assert_eq!(encode_password("0000").unwrap(), "171129176174");
    assert_eq!(default_password("A0XX012345"), "2345");
}

#[test]
fn invalid_passwords_are_rejected() {
    assert!(encode_password("").is_err());
    assert!(encode_password("12345678901234567").is_err());
    assert!(encode_password("pässword").is_err());
}

fn identity() -> Identity {
    Identity {
        host_id: [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77],
        mac: [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
        host_port: 55265,
        password: None,
    }
}

fn wall_clock() -> WallClock {
    WallClock {
        year: 2026,
        month: 11,
        day: 1,
        hour: 12,
        min: 34,
        sec: 56,
        tz_bias_secs: 18000,
    }
}

const REG_V6: &str = "00061e000000000000000001c0a8000a0000d7e131373231333131373931373800000000000000000000000000000000000000000000000000000000000000000000000007ea0b010c2238000011223344556677000046500000000000000000";

const REG_V3: &str = "00031e000000000000000001c0a8000a0000d7e131373231333131373931373800000000000000000000000000000000000000000000000000000000000000000000000007ea0b010c2238000011223344556677000000000000000000000000";

#[test]
fn registration_bodies_match_the_vectors() {
    let host_ip = Ipv4Addr::new(192, 168, 0, 10);
    assert_eq!(
        hex(
            &register_body(&identity(), host_ip, 6, "1234", wall_clock())
                .expect("registration body")
        ),
        REG_V6
    );
    assert_eq!(
        hex(
            &register_body(&identity(), host_ip, 3, "1234", wall_clock())
                .expect("registration body")
        ),
        REG_V3
    );
}
