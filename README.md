# freejitsu

A small daemon for the Fujitsu ScanSnap iX500. It talks the scanner's Wi-Fi protocol directly, waits for the Scan button, scans every sheet in the feeder, and writes one PDF per press.

Independent interoperability project. Not affiliated with or endorsed by PFU or Fujitsu.

## Before you start

The scanner keeps a table of paired hosts that is written only over USB. Pair it once with ScanSnap Home, then quit ScanSnap Home. Only one host can hold the scanner session, and the button notices go to the host on port 55265.

## Build

```sh
cargo build --release
```

## Run

```sh
./target/release/freejitsu --ip 192.168.0.130
```

Put paper in the feeder and press Scan. The PDF lands in `~/Scans` as `scan-YYYYMMDD-HHMMSS.pdf`.

If the scanner has a Wi-Fi password, pass it once with `--password`; it is saved in the identity file (mode 0600) after a successful registration. Status -2 means a password is needed, status -3 means it is wrong.

One immediate scan, no daemon:

```sh
./target/release/freejitsu --once
```

## Flags

| Flag | Default | Meaning |
|---|---|---|
| `--ip` | `192.168.0.130` | Scanner address |
| `--out` | `~/Scans` | Output directory |
| `--dpi` | `300` | 150, 200, 300 or 600 |
| `--page` | `auto` | `auto`, `a4`, `letter`, `legal`, `b5`, `a5`, `b6` |
| `--simplex` | off | Keep front sides only |
| `--identity` | `~/.config/freejitsu/identity` | Paired host ID, MAC and port |
| `--password` | stored | Scanner Wi-Fi password, saved after a successful registration |
| `--once` | off | Scan now and exit |
| `--airscan` | off | Serve eSCL/AirScan over HTTP and Bonjour |
| `--airscan-port` | `8090` | AirScan HTTP port |

## AirScan

Run with `--airscan` to publish the scanner over eSCL/AirScan. macOS sees it in
Image Capture and Preview without a driver.

```sh
./target/release/freejitsu --airscan
```

The capabilities URL is printed at startup. One scan runs at a time; a Scan
button press during a job is ignored. Every job still writes a PDF to the
output directory. JPEG pages carry an EXIF 180-degree orientation tag; choose
PDF if your client ignores it. Grayscale and black-and-white are not offered
because the scanner only produces color JPEGs. `--airscan` conflicts with
`--once`.

## What the PDF contains

One page per side. Each JPEG from the scanner is embedded unchanged, so there is no re-encoding. Pages are rotated 180 degrees in the PDF because the scanner delivers them upside down. Page size follows the JPEG pixels and the configured DPI.

Not implemented: crop to paper, deskew, gray conversion, blank page removal.

## Failure behaviour

A jam after some sheets still writes a valid PDF with the sheets that arrived and logs the error. A scan that delivers nothing leaves no file. A partial file is never renamed into place.

## Verification

The test suite uses a loopback fake scanner. It covers the registration body, the exact scan command order, multi-chunk images, jams, an empty feeder, notifications, and the PDF xref.

```sh
cargo test
```

The final check is one press on a real iX500. That has run once: firmware 0U00, firmware name `ScanSnapix500`, registration with the stored Wi-Fi password, a duplex scan, and the resulting two-page PDF (embedded JPEGs, 180-degree rotation).

## License

MIT. The protocol documentation and Python reference are from [elte0/scansnack](https://github.com/elte0/scansnack), also MIT; see `LICENSE` and `THIRD-PARTY.md`.

## Credits

The protocol documentation and the Python reference implementation are from [elte0/scansnack](https://github.com/elte0/scansnack), MIT licensed.
