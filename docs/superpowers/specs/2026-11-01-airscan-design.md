# AirScan bridge design

Status: approved 2026-11-01.

## Intent

Make the ScanSnap iX500 usable as an AirScan/eSCL network scanner from macOS
Image Capture, Preview and other eSCL clients, while the daemon keeps waiting
for the Scan button. A small HTTP and Bonjour front end inside the daemon
translates an eSCL job into the scan path that already exists.

The daemon already holds the scanner session and the only scan connection, so
it is the natural host for the bridge. A separate process would have to fight
it for the scanner's single host session.

## Goals

- Announce an eSCL scanner over Bonjour (`_uscan._tcp`).
- Serve capabilities, status, scan jobs and `NextDocument`.
- Let the client drive the scan: resolution, duplex, scan region, JPEG or PDF.
- Share the scanner with the button: one scan at a time, never two connections.
- Keep the existing behaviour of writing one PDF per job to the output directory.
- Off by default, enabled with `--airscan`.

## Non-goals

- Flatbed: the iX500 has none.
- Grayscale or 1-bit output: the iX500 only produces color JPEGs, and host-side
  conversion would mean adding a codec.
- Crop, deskew, blank-page removal, background cleanup.
- TLS (`_uscans._tcp`), authentication, or exposure beyond the local network.
- Persistent jobs across daemon restarts, or several scanners.

## Constraints

- The scanner accepts one connection on the scan port at a time and one
  registered host. Every job ends with D6 on a fresh connection.
- The iX500 delivers every page rotated 180 degrees.
- No new image codec: JPEG bytes must be passed through, not re-encoded.
- The scanner's window block accepts width and height in 1/1200 inch. The
  protocol write-up only documents the named page presets; arbitrary windows
  are plausible but unverified.

## Approaches

1. **eSCL server inside the daemon, std-only HTTP, `mdns-sd` for Bonjour
   (chosen).** Reuses the live session and the existing capture path. The only
   new dependency is `mdns-sd`, which brings six small crates and handles
  probing, conflict resolution and goodbyes.
2. **Same, with a hand-written mDNS responder.** Zero new dependencies but
   roughly 300 more lines of subtle protocol. Discovery is the make-or-break
   feature, so a battle-tested responder is worth more here than dependency
   purity. Rejected.
3. **Separate bridge process.** Clean process boundary, but the scanner allows
   one session per host: the bridge would replace the button daemon instead of
   coexisting with it. Rejected.

## Decisions

### Flags

| Flag | Default | Meaning |
|---|---|---|
| `--airscan` | off | Serve eSCL and announce over Bonjour |
| `--airscan-port` | `8090` | HTTP port |

`--airscan` conflicts with `--once`. `Config` gains `airscan: bool` and
`airscan_port: u16`.

### Session and concurrency

The daemon keeps a shared slot with the current `Session { info, identity }`.
It is filled after a successful registration and cleared on power-off,
heartbeat loss, or a disconnected notification channel. AirScan capabilities
keep the last known `ScannerInfo` so the device stays describable while the
scanner is off.

A single shared scan lock (an atomic busy flag with an RAII guard) serialises
all use of the scanner:

- Button scan: `try_lock`. If the scanner is busy, log and ignore the press.
- eSCL job: `try_lock`. If busy, reply 503.
- The job holds the lock only while the scanner is reading, not while the
  client downloads pages. It is released after D6 and after the PDF is
  finished.

### Bonjour

The HTTP listener starts with the daemon. The Bonjour advertisement starts
after the first successful registration, when the model and serial are known,
and stays up across power cycles (status reports `Stopped` while offline).

- Service type: `_uscan._tcp.local.`
- Instance name and `ty`: `<model> (<serial>)`, for example
  `ScanSnap iX500 (A0XX012345)`.
- Host name: `freejitsu-<lowercase serial>.local.`, sanitised to DNS label
  characters.
- Address: the local IPv4 address towards the scanner (`iface::local_ip_towards`).
- Port: `--airscan-port`.
- TXT: `txtvers=1`, `vers=2.63`, `rs=eSCL`, `ty=<name>`,
  `pdl=image/jpeg,application/pdf`, `uuid=<device uuid>`, `cs=color`,
  `is=adf`, `duplex=T`.
- Device UUID: stable across restarts, derived from model and serial with
  128-bit FNV-1a, formatted as a version-5 UUID.
- If mdns-sd registration fails, log it and keep the HTTP server; the device
  can still be reached by URL.

### HTTP

A small HTTP/1.1 subset on `0.0.0.0:<airscan-port>`, one thread per connection.

- Request line, headers (case-insensitive names), `Content-Length` bodies.
- `Expect: 100-continue` answered before reading the body.
- `Connection: close` on every response.
- Limits: 16 KiB of request line plus headers, 1 MiB body.
- 30 second request read timeout, 120 second write timeout.
- `Date` is not required; `Server: freejitsu` is sent.

| Method | Path | Result |
|---|---|---|
| GET, HEAD | `/eSCL/ScannerCapabilities` | 200 XML, 503 before any session |
| GET, HEAD | `/eSCL/ScannerStatus` | 200 XML, 503 before any session |
| POST | `/eSCL/ScanJobs` | 201 + `Location`, 400 bad XML, 409 unsupported setting or empty feeder, 503 busy/offline |
| GET | `/eSCL/ScanJobs/<id>/NextDocument` | 200 image, 404 when the job has no more documents |
| DELETE | `/eSCL/ScanJobs/<id>` | 200 canceled, 404 unknown job |
| anything else | | 404 |

`HEAD` answers capabilities and status only; it never consumes a document.

`Location` is built from the request's `Host` header, falling back to the
listener address and port.

### Capabilities

Follow the shape proven by the scansnack eSCL bridge (version 2.63):

- `MakeAndModel`, `SerialNumber`, `UUID`.
- `Adf` with `AdfSimplexInputCaps` and `AdfDuplexInputCaps`.
- Color mode `RGB24` only.
- Formats `image/jpeg` and `application/pdf`.
- Discrete resolutions 150, 200, 300 and 600 dpi.
- Region limits in 1/300 inch: minimum 300, maximum 2612 x 4407 (the full
  iX500 scan area).
- `MaxScanRegions` 1, `MaxOpticalXResolution`/`Y` 600, feeder capacity 50,
  `AdfOption Duplex`, intents Document/TextAndGraphic/Photo.

Requests for any other color mode or format are rejected with 409 rather than
silently converted.

### Scan settings

Namespaces are ignored; element local names are matched. The first
`ScanRegion` is used.

| Element | Handling |
|---|---|
| `InputSource` | `Feeder` or `Adf` (case-insensitive), absent means `Feeder`; anything else 409 |
| `XResolution` / `YResolution` | use `XResolution`; only when it is absent use `YResolution`; snap to the nearest of 150, 200, 300, 600 |
| `Duplex` | `true`/`1` keeps both sides, otherwise front only |
| `ColorMode` | `RGB24` (default); anything else 409 |
| `DocumentFormatExt`, then `DocumentFormat` | `image/jpeg` (default) or `application/pdf`; anything else 409 |
| `ScanRegion.Width`, `.Height` | values are 1/300 inch; clamp to 300..=2612 wide and 300..=4407 high in 1/300 inch, then multiply by 4 for the 1/1200 inch `Area`; absent region means Auto |
| `ScanRegion.XOffset`, `.YOffset` | ignored; the iX500 window has no offset |
| `Intent` | ignored |

The scanner always reads both sides; simplex only drops the back.

### Scan area

`scan.rs` gains `Area { width, height, automatic }` in 1/1200 inch. `PageSize`
converts into it, and the E9/D4 builders take an `Area`. A client region sets
`automatic: false` with the clamped dimensions; no region uses `PageSize::Auto`.

Risk: arbitrary window dimensions are not verified against real hardware.
Verify early. Fallback if the scanner misbehaves: snap the requested region to
the nearest named preset.

### Job lifecycle

1. Parse settings, clone the session, `try_lock` the scan lock, create a job.
2. Start a job thread and wait up to 60 seconds for the first scanned page.
   - First page or timeout: 201 with an absolute `Location` URL.
   - Feeder empty before any page: 409.
   - Any other failure before any page: 500.
3. The job thread calls the shared capture with the client settings. Each
   sheet is added to the PDF; for JPEG format, front and, when duplex, back
   are queued for delivery as soon as read.
4. For PDF format, the finished PDF is read back from disk and queued as one
   document. JPEG clients get one document per page.
5. `NextDocument` blocks until a document is available or the job ends, then
   returns it; when the job has no more documents it returns 404.
6. `DELETE` sets a cancel flag. The scan callback checks it after each sheet
   and aborts with `Error::Canceled`; a cancelled job leaves no PDF. A sheet
   already being read is allowed to finish. Remaining paper stays in the feeder.
7. Keep the ten most recent finished jobs; prune older ones when a new job
   starts.

Job states: `Processing` (`JobScanning`), `Completed`
(`JobCompletedSuccessfully`), `Aborted` (`AdfEmpty`, `MediaJam`,
`ErrorOccurred`), `Canceled` (`JobCanceledByUser`).

`NextDocument` waits until a document is ready or the job ends; it has no
fixed deadline of its own, because a multi-sheet 600 dpi PDF job may take
minutes. The socket write timeout starts only once there is a body to write.
Documents are held as `Vec<u8>` in the job. A job keeps undelivered documents
until they are fetched or the job ages out of the ten-job window. One job at a
time and normal page-by-page delivery keep this bounded; a client that
starts a job and never fetches can hold the pages of that one job until it is
pruned.

Partial scans keep the project's failure behaviour: pages already read are
finished into a valid PDF. A jam therefore still leaves a PDF and, for JPEG
format, still delivers the pages that arrived. The job is reported `Aborted`
with `MediaJam`.

### Status

Status is served from cached state; it never opens a SCSI connection.

- `State` is `Processing` while a scan holds the shared lock (button or eSCL),
  `Stopped` while there is no session, and `Idle` otherwise.
- `AdfState` is the last known feeder state, shared by both scan paths:
  `ScannerAdfEmpty` after `NoPaper` or a feed error, `ScannerAdfLoaded` after a
  successful scan, and `ScannerAdfLoaded` before the first scan.
- The `Jobs` list is the ten most recent jobs, newest first.

### Images and rotation

- PDF: unchanged JPEGs, `/Rotate 180`, as today.
- JPEG documents: the original JPEG with a 36-byte EXIF APP1 segment inserted
  after SOI, carrying Orientation 3 (180 degrees). No re-encoding.
- Every job writes the usual `scan-YYYYMMDD-HHMMSS.pdf` into the output
  directory, including jobs whose client asked for JPEG. A cancelled job writes
  nothing.

Risk: a client that ignores EXIF orientation shows JPEG pages upside down. The
PDF format is the reliable path; if this matters in practice, the follow-up is
host-side rotation with a JPEG codec.

### Capture refactor

`daemon::capture` becomes a shared function that takes `Dpi`, `Area`, simplex
and an optional page callback, and returns:

```rust
pub struct Capture {
    pub path: PathBuf,          // finished PDF
    pub early: Option<Error>,   // jam or similar after some pages arrived
}
```

It returns `Err` only when nothing was written or the PDF itself failed.
Button scans pass the configured settings and an empty callback; eSCL passes
the client settings and a callback that queues JPEG documents. `Error::Canceled`
is added and makes capture drop the partial PDF.

### Errors

- Bad XML: 400.
- Unsupported setting: 409.
- No paper before any page: 409.
- Busy or scanner offline: 503.
- Anything else: 500, logged with the job id.

## Modules

| File | Responsibility |
|---|---|
| `src/http.rs` | HTTP request parsing, response writing, connection loop |
| `src/escl.rs` | ScanSettings parsing, capabilities/status XML, EXIF orientation, device UUID |
| `src/airscan.rs` | Service state, job table, endpoints, Bonjour, job thread |
| `src/scan.rs` | `Area`, area-aware E9/D4 builders |
| `src/daemon.rs` | shared session, scan lock, AirScan lifecycle, capture refactor |
| `src/config.rs` | `--airscan`, `--airscan-port` |
| `src/error.rs` | `Error::Canceled` |
| `src/lib.rs` | module declarations |

`airscan` is not started with `--once`.

## Types

```rust
pub struct Session { pub info: ScannerInfo, pub identity: Identity }

pub struct ScanSettings {
    pub dpi: Dpi,
    pub area: Area,
    pub duplex: bool,
    pub format: Format,
}

pub enum Format { Jpeg, Pdf }

pub struct Area { pub width: u32, pub height: u32, pub automatic: bool }

pub struct JobView {
    pub id: String,
    pub age_secs: u64,
    pub images: u32,
    pub to_transfer: u32,
    pub state: &'static str,
    pub reason: &'static str,
}
```

## Testing

New `tests/airscan.rs`:

- ScanSettings parsing: defaults, namespace prefixes, duplex, region, format,
  unsupported source/color/format/resolution.
- Capabilities and status XML contain the advertised values and escaped names.
- Device UUID is stable and has the version/variant bits set.
- EXIF orientation segment is present in delivered JPEGs and the original
  scanner bytes remain intact after it.
- HTTP over loopback against a fake scanner:
  - capabilities and status;
  - JPEG duplex job: 201, two documents, 404 after, correct E9/D4 blocks;
  - PDF job: one document starting with `%PDF`, two pages, `/Rotate 180`;
  - empty feeder: 409;
  - scan lock held: 503;
  - no session: 503;
  - unknown job and unknown path: 404;
  - DELETE on a finished job: 200.
- Saved PDF exists for both formats; cancelled job leaves none (cancel between
  sheets in the fake scanner).

Existing tests:

- `tests/scan_sequence.rs` and `tests/golden.rs` call the E9/D4 builders with
  `Area` (via `PageSize::Auto.into()`).
- `tests/e2e.rs` config builder gains the two new fields.

Manual gate on the real iX500:

- `dns-sd -B _uscan._tcp` sees `ScanSnap iX500 (...)`.
- `curl` capabilities and a 300 dpi A4 duplex job, JPEG and PDF.
- Pages are upright and have the requested size; 600 dpi and simplex work.
- Button scan still works after an AirScan job.
- macOS Image Capture discovers the scanner and scans both formats.

## Risks and open questions

1. Arbitrary scan-region dimensions are unverified on the iX500. Mitigation:
   test on hardware first; fall back to the nearest named preset.
2. EXIF orientation support varies by eSCL client. PDF avoids the question.
3. macOS may ask for firewall permission for incoming connections the first
   time; that is expected.
4. `mdns-sd` is beta software, though it implements the parts we need and is
   widely used.
5. Waiting for the first page before replying to `POST` takes as long as the
   first sheet; the reference bridge uses the same 60 second window and works
   with macOS.

## Left to the builder

- Exact XML element order and whitespace; follow the scansnack eSCL reference.
- HTTP parser structure and helper names.
- How the fake-scanner test exercises cancellation and the scan lock.
- Whether the AirScan listener binds before or after the button listeners, as
  long as a bind failure is fatal and logged clearly.
- README wording and flag table.
