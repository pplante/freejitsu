use freejitsu::config::Dpi;
use freejitsu::escl::{
    Capabilities, Format, JobView, ScanSettings, SettingsError, capabilities_xml, device_uuid,
    parse_scan_settings, status_xml, with_orientation_180,
};
use freejitsu::scan::Area;

const FULL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<scan:ScanSettings xmlns:scan="http://schemas.hp.com/imaging/escl/2011/05/03"
                   xmlns:pwg="http://www.pwg.org/schemas/2010/12/sm">
  <scan:InputSource>Feeder</scan:InputSource>
  <scan:ColorMode>RGB24</scan:ColorMode>
  <scan:XResolution>300</scan:XResolution>
  <scan:YResolution>300</scan:YResolution>
  <scan:Duplex>true</scan:Duplex>
  <pwg:ScanRegions><pwg:ScanRegion>
    <pwg:Width>2480</pwg:Width><pwg:Height>3508</pwg:Height>
    <pwg:XOffset>0</pwg:XOffset><pwg:YOffset>0</pwg:YOffset>
  </pwg:ScanRegion></pwg:ScanRegions>
  <pwg:DocumentFormat>image/jpeg</pwg:DocumentFormat>
</scan:ScanSettings>"#;

fn parse(body: &str) -> Result<ScanSettings, SettingsError> {
    parse_scan_settings(body.as_bytes())
}

#[test]
fn defaults_are_300_dpi_auto_simplex_jpeg() {
    let settings = parse("<scan:ScanSettings/>").unwrap();
    assert_eq!(settings.dpi, Dpi::D300);
    assert_eq!(
        settings.area,
        Area {
            width: 0x28d0,
            height: 0x44dc,
            automatic: true
        }
    );
    assert!(!settings.duplex);
    assert_eq!(settings.format, Format::Jpeg);
}

#[test]
fn reads_the_full_settings_document() {
    let settings = parse(FULL).unwrap();
    assert_eq!(settings.dpi, Dpi::D300);
    assert!(settings.duplex);
    assert_eq!(
        settings.area,
        Area {
            width: 9920,
            height: 14032,
            automatic: false
        }
    );
}

#[test]
fn snaps_resolution_and_prefers_x() {
    assert_eq!(
        parse("<ScanSettings><XResolution>75</XResolution></ScanSettings>")
            .unwrap()
            .dpi,
        Dpi::D150
    );
    assert_eq!(
        parse("<ScanSettings><XResolution>400</XResolution></ScanSettings>")
            .unwrap()
            .dpi,
        Dpi::D300
    );
    assert_eq!(
        parse("<ScanSettings><XResolution>1200</XResolution></ScanSettings>")
            .unwrap()
            .dpi,
        Dpi::D600
    );
    assert_eq!(
        parse("<ScanSettings><YResolution>600</YResolution></ScanSettings>")
            .unwrap()
            .dpi,
        Dpi::D600
    );
    assert_eq!(
        parse("<ScanSettings><XResolution>600</XResolution><YResolution>300</YResolution></ScanSettings>")
            .unwrap()
            .dpi,
        Dpi::D600
    );
}

#[test]
fn clamps_the_region_in_three_hundredths() {
    let small = parse(
        "<ScanSettings><ScanRegion><Width>10</Width><Height>10</Height></ScanRegion></ScanSettings>",
    )
    .unwrap();
    assert_eq!(
        small.area,
        Area {
            width: 1200,
            height: 1200,
            automatic: false
        }
    );
    let large = parse(
        "<ScanSettings><ScanRegion><Width>99999</Width><Height>99999</Height></ScanRegion></ScanSettings>",
    )
    .unwrap();
    assert_eq!(
        large.area,
        Area {
            width: 10448,
            height: 17628,
            automatic: false
        }
    );
}

#[test]
fn rejects_unsupported_settings() {
    for body in [
        "<ScanSettings><InputSource>Platen</InputSource></ScanSettings>",
        "<ScanSettings><ColorMode>Grayscale8</ColorMode></ScanSettings>",
        "<ScanSettings><DocumentFormat>image/tiff</DocumentFormat></ScanSettings>",
    ] {
        assert!(
            matches!(parse(body), Err(SettingsError::Unsupported(_))),
            "{body}"
        );
    }
}

#[test]
fn rejects_malformed_xml() {
    assert!(matches!(
        parse("<ScanSettings><Duplex>true</ScanSettings>"),
        Err(SettingsError::BadXml(_))
    ));
    assert!(matches!(
        parse("<ScanSettings>&xxe;</ScanSettings>"),
        Err(SettingsError::BadXml(_))
    ));
    assert!(parse("<ScanSettings><Intent>Document</Intent></ScanSettings>").is_ok());
}

#[test]
fn rejects_xml_deeper_than_eight() {
    let mut body = String::from("<ScanSettings>");
    for _ in 0..10 {
        body.push_str("<a>");
    }
    for _ in 0..10 {
        body.push_str("</a>");
    }
    body.push_str("</ScanSettings>");
    assert!(matches!(parse(&body), Err(SettingsError::BadXml(_))));
}

#[test]
fn capabilities_describe_the_advertised_scanner() {
    let xml = capabilities_xml(&Capabilities {
        model: "ScanSnap iX500",
        serial: "A0XX012345",
        uuid: "00000000-0000-5000-8000-000000000000",
    });
    assert!(xml.contains("<pwg:MakeAndModel>ScanSnap iX500</pwg:MakeAndModel>"));
    assert!(xml.contains("<scan:SerialNumber>A0XX012345</scan:SerialNumber>"));
    assert_eq!(
        xml.matches("ScannerCapabilities").count(),
        2,
        "Adf must sit directly under the root"
    );
    assert!(xml.contains("<scan:FeederCapacity>50</scan:FeederCapacity>"));
    assert!(xml.contains("<scan:ColorMode>RGB24</scan:ColorMode>"));
    assert!(!xml.contains("Grayscale8"));
    assert!(xml.contains("<scan:XResolution>600</scan:XResolution>"));
    assert!(xml.contains("<scan:MaxWidth>2612</scan:MaxWidth>"));
    assert!(xml.contains("<scan:MaxHeight>4407</scan:MaxHeight>"));
    assert!(xml.contains("<scan:AdfDuplexInputCaps>"));
    assert!(xml.contains("<scan:DocumentFormatExt>application/pdf</scan:DocumentFormatExt>"));

    let escaped = capabilities_xml(&Capabilities {
        model: "A&B",
        serial: "S<1>",
        uuid: "u",
    });
    assert!(escaped.contains("A&amp;B"));
    assert!(escaped.contains("S&lt;1&gt;"));
}

#[test]
fn status_lists_the_state_and_jobs() {
    let xml = status_xml(
        "Idle",
        "ScannerAdfLoaded",
        &[JobView {
            id: "abc".into(),
            age_secs: 2,
            images: 3,
            to_transfer: 1,
            state: "Processing",
            reason: "JobScanning",
        }],
    );
    assert!(xml.contains("<pwg:State>Idle</pwg:State>"));
    assert!(xml.contains("<scan:AdfState>ScannerAdfLoaded</scan:AdfState>"));
    assert!(xml.contains("<pwg:JobUri>/eSCL/ScanJobs/abc</pwg:JobUri>"));
    assert!(xml.contains("<pwg:JobState>Processing</pwg:JobState>"));
}

#[test]
fn jpeg_gets_a_180_degree_exif_orientation() {
    let jpeg = std::fs::read("tests/fixtures/page.jpg").unwrap();
    let out = with_orientation_180(&jpeg).unwrap();
    assert_eq!(out.len(), jpeg.len() + 36);
    assert_eq!(&out[2..4], &[0xff, 0xe1]);
    assert_eq!(&out[4..6], &[0x00, 0x22]);
    assert_eq!(&out[6..12], b"Exif\0\0");
    assert_eq!(&out[30..32], &[0x00, 0x03]);
    let mut stripped = out[..2].to_vec();
    stripped.extend_from_slice(&out[38..]);
    assert_eq!(stripped, jpeg);
    assert!(with_orientation_180(b"not a jpeg").is_err());
}

#[test]
fn device_uuid_is_stable_and_version_five() {
    let uuid = device_uuid("ScanSnap iX500", "A0XX012345");
    assert_eq!(uuid.len(), 36);
    assert_eq!(&uuid[14..15], "5");
    assert!(matches!(&uuid[19..20], "8" | "9" | "a" | "b"));
    assert_eq!(uuid, device_uuid("ScanSnap iX500", "A0XX012345"));
    assert_ne!(uuid, device_uuid("ScanSnap iX500", "A0XX012346"));
}

#[test]
fn rejects_cdata_text_over_4096_bytes() {
    let body = format!(
        "<ScanSettings><Intent><![CDATA[{}]]></Intent></ScanSettings>",
        "a".repeat(5000)
    );
    assert!(matches!(parse(&body), Err(SettingsError::BadXml(_))));
}
