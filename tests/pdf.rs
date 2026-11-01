mod support;

use std::fs;
use std::path::PathBuf;

use freejitsu::pdf::{PdfWriter, jpeg_size};

fn fixture() -> Vec<u8> {
    fs::read("tests/fixtures/page.jpg").expect("page.jpg")
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

fn xref_offsets(bytes: &[u8]) -> Vec<(usize, usize)> {
    let start = find(bytes, b"startxref\n").expect("startxref");
    let rest = &bytes[start + b"startxref\n".len()..];
    let end = rest
        .iter()
        .position(|&byte| byte == b'\n')
        .expect("line end");
    let xref: usize = std::str::from_utf8(&rest[..end])
        .expect("startxref number")
        .trim()
        .parse()
        .expect("xref offset");
    assert_eq!(&bytes[xref..xref + 4], b"xref");

    let mut lines = bytes[xref..].split(|&byte| byte == b'\n');
    assert_eq!(lines.next(), Some(&b"xref"[..]));
    let header = lines.next().expect("xref header");
    assert!(header.starts_with(b"0 "), "xref header {header:?}");
    let size: usize = std::str::from_utf8(&header[2..])
        .expect("size")
        .parse()
        .expect("size number");
    assert_eq!(lines.next(), Some(&b"0000000000 65535 f "[..]));

    let mut entries = Vec::new();
    for number in 1..size {
        let entry = lines.next().expect("xref entry");
        assert_eq!(entry.len(), 19, "entry {entry:?}");
        let offset: usize = std::str::from_utf8(&entry[..10])
            .expect("offset")
            .parse()
            .expect("offset number");
        entries.push((number, offset));
    }
    entries
}

#[test]
fn jpeg_size_reads_the_sof_marker() {
    assert_eq!(jpeg_size(&fixture()).expect("size"), (300, 400, 3));
    assert!(jpeg_size(b"not a jpeg").is_err());
}

#[test]
fn two_pages_round_trip_through_the_xref() {
    let jpeg = fixture();
    let dir = support::temp_dir("pdf");
    let dest = dir.join("scan.pdf");

    let mut writer = PdfWriter::create(dest.clone()).expect("create");
    writer.add_page(&jpeg, 300).expect("front");
    writer.add_page(&jpeg, 300).expect("back");
    let path = writer.finish().expect("finish");
    assert_eq!(path, dest);

    let bytes = fs::read(&path).expect("read PDF");
    assert!(bytes.starts_with(b"%PDF-1.4"));

    let entries = xref_offsets(&bytes);
    assert_eq!(entries.len(), 8);
    for (number, offset) in entries {
        let prefix = format!("{number} 0 obj");
        assert!(
            bytes[offset..].starts_with(prefix.as_bytes()),
            "object {number} at {offset} is not {prefix}"
        );
    }

    assert_eq!(count(&bytes, b"/Type /Page "), 2);
    assert_eq!(count(&bytes, b"/Rotate 180"), 2);
    assert!(find(&bytes, b"/Count 2").is_some());
    assert!(find(&bytes, b"/MediaBox [0 0 72 96]").is_some());
    assert_eq!(count(&bytes, &jpeg), 2);
    assert_eq!(count(&bytes, b"/Filter /DCTDecode"), 2);
    assert_eq!(count(&bytes, b"/ColorSpace /DeviceRGB"), 2);

    let files: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("out dir")
        .map(|entry| entry.expect("entry").path())
        .collect();
    assert_eq!(files.len(), 1, "temp file left behind: {files:?}");
}

#[test]
fn drop_without_finish_leaves_no_file() {
    let jpeg = fixture();
    let dir = support::temp_dir("pdf-drop");
    let dest = dir.join("scan.pdf");

    let mut writer = PdfWriter::create(dest.clone()).expect("create");
    writer.add_page(&jpeg, 300).expect("front");
    assert!(!dest.exists());
    drop(writer);

    assert!(!dest.exists());
    assert_eq!(fs::read_dir(&dir).expect("out dir").count(), 0);
}
