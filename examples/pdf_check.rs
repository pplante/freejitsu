use std::path::PathBuf;

use freejitsu::pdf::PdfWriter;

/// Writes a two page PDF under target/ for `pdfinfo` and `qpdf --check`.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let jpeg = std::fs::read("tests/fixtures/page.jpg")?;
    let mut writer = PdfWriter::create(PathBuf::from("target/pdf-check.pdf"))?;
    writer.add_page(&jpeg, 300)?;
    writer.add_page(&jpeg, 300)?;
    let path = writer.finish()?;
    println!("{}", path.display());
    Ok(())
}
