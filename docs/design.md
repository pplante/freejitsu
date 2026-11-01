# freejitsu design

A single-binary daemon for one ScanSnap iX500. It registers with the scanner, waits for the Scan button, scans the feeder, and writes one PDF per press.

Protocol source of truth: `docs/protocol-ix500.md` in the scansnack reference clone. This design keeps the wire facts and drops the adapter stack.

## Modules

| File | Responsibility |
|---|---|
| `src/main.rs` | parse flags, call `daemon::run`, map errors to exit codes |
| `src/lib.rs` | module declarations only, so integration tests drive the real code |
| `src/config.rs` | `Config`, `Ports`, defaults, clap parsing |
| `src/error.rs` | `Error`, `Result` |
| `src/identity.rs` | load or create the persistent host identity |
| `src/iface.rs` | local IP and MAC towards the scanner |
| `src/wire.rs` | VENS TCP framing, greeting, frame status |
| `src/discovery.rs` | UDP probe and 132-byte reply, heartbeat thread |
| `src/control.rs` | registration 0x11 body and status |
| `src/scsi.rs` | SCSI tunnel on the discovered scan port, sense, D6 release |
| `src/scan.rs` | D5 to D6 sequence, D4 and E9 blocks, sheet reads |
| `src/notify.rs` | UDP listeners for the button and power notices, dedupe |
| `src/pdf.rs` | streamed PDF with DCTDecode pages, atomic rename |
| `src/daemon.rs` | session state machine and one capture |

Scan and control ports come from the discovery reply, so a fake scanner can advertise ephemeral ports. Discovery, button and startup ports live in `Ports` with protocol defaults, so tests can move them off the fixed ports.

## Core types

```rust
pub struct Config {
    pub ip: Ipv4Addr,             // --ip, default 192.168.0.130
    pub out_dir: PathBuf,         // --out, default $HOME/Scans
    pub dpi: Dpi,                 // --dpi, default 300
    pub page: PageSize,           // --page, default auto
    pub simplex: bool,            // --simplex, drop back images
    pub identity: PathBuf,        // --identity, default ~/.config/freejitsu/identity
    pub password: Option<String>, // --password, default last four serial digits
    pub once: bool,               // --once, scan now and exit
    pub ports: Ports,             // protocol defaults, overridden in tests
}
pub struct Ports { pub discovery: u16, pub startup: u16 } // 52217, 53220
pub enum Dpi { D150, D200, D300, D600 }
pub enum PageSize { Auto, A4, Letter, Legal, B5, A5, B6 }
pub struct Identity {
    pub host_id: [u8; 8],   // paired host ID from discovery on first run
    pub mac: [u8; 6],       // real interface MAC, else random locally administered unicast
    pub host_port: u16,     // 55265, where the scanner sends the button notice
    pub password: Option<String>,
}
pub struct ScannerInfo {
    pub ip: Ipv4Addr,            // reply offset 16, must equal the UDP sender
    pub scan_port: u16,          // 20
    pub control_port: u16,       // 24
    pub mac: [u8; 6],            // 28
    pub name: String,            // 40, "iX500-<serial>"
    pub model: String,           // 104
    pub if_version: u16,         // 8
    pub paired_ip: Ipv4Addr,     // 120
    pub paired_host_id: [u8; 8], // 124
    pub serial: String,          // name after the last '-'
}
pub struct ImageInfo { pub width: u32, pub max_height: u32, pub height: u32,
                       pub dpi_x: u16, pub dpi_y: u16 }
pub struct Sheet { pub front: Vec<u8>, pub back: Vec<u8>, pub info: ImageInfo }
pub struct Sense { pub key: u8, pub asc: u8, pub ascq: u8, pub eom: bool, pub info: u32 }
pub enum Event { Button, PowerOff, PowerOn }
pub enum Error {
    Config(String), Io(std::io::Error), Protocol(String), Discovery(String),
    Registration(i32), Scsi { cd: u8, status: u8, sense: Option<Sense> },
    NoPaper, Feed(&'static str), Pdf(String),
}
```

Both sides always arrive. Simplex is a capture decision, not a protocol change.

## Session state

```
run(cfg):
  ident = load identity if present
  mac = interface MAC towards cfg.ip, else stored MAC, else random
  listeners on ident.host_port or 55265, and cfg.ports.startup
  if cfg.once:
      info = discover()?; ident = ensure_identity(info, mac)?; register()?; capture()?
  loop:
    Down:  open_session() -> Up | log and sleep 2s
    Up:    Button   -> capture, log path or error
           PowerOff -> Down
           Timeout  -> heartbeat dead ? Down : stay
           PowerOn  -> ignore, already up
open_session:
  info = discover()?          // 3 attempts, 1s timeout each
  register(info, ident)?      // 0x11, status at reply body offset 8
  Heartbeat::start(info, ident.mac)
  Up
capture:
  pdf = PdfWriter::create(out_dir / "scan-<local time>.pdf")
  ch = ScsiChannel::open(info.scan_port, ident.mac)?
  scan(&mut ch, dpi, page, |sheet| { pdf.add_page(front); pdf.add_page(back) unless simplex })
  drop(ch)
  release(info.scan_port, ident.mac)  // D6 on a fresh connection, also after errors
  if pages > 0: pdf.finish() -> path
  else: remove temp, report NoPaper or the scan error
```

Registration happens immediately at startup, not on the first timeout. A button notice while Down is dropped.

## Behaviour decisions

- Identity. The scanner's host table is written only over USB by ScanSnap Home. On first run take `paired_host_id` from the discovery reply, reject an all-zero ID with a message to pair over USB once, and persist host ID, MAC and host port. Later runs reuse the file. The file is created with mode 0600, written to a temp file and renamed.
- MAC. Every frame header carries the same MAC. Prefer the real interface MAC towards the scanner. If it cannot be found, use a random locally administered unicast MAC and persist it.
- Password. Precedence is `--password`, then the identity file, then the last four characters of the serial. Registration status -2 names the password, -7 names USB pairing.
- Notifications. Filter on the scanner IP. Drop repeats with the same event and counter within 3 seconds. A new counter is a new press even inside that window.
- Partial scans. A jam after some sheets writes a valid PDF with the delivered sheets and logs the error. A scan that delivers zero sheets leaves no file. No file is ever renamed into place before it is complete.
- PDF. Each JPEG is embedded unchanged as a DCTDecode stream. Page size is pixels times 72 divided by DPI. Pages carry `/Rotate 180` because the scanner delivers them upside down. No crop, no deskew, no gray conversion, no blank page removal.
- Failure surface. The daemon logs scan failures and keeps the session. Only a listener bind failure at startup is fatal.

## Dependencies

| Crate | Why |
|---|---|
| `clap` | eight flags with defaults, `--help`, and validation at the boundary |
| `libc` | `localtime_r` and `tm_gmtoff` for the registration time fields |
| `getrandom` | host ID and fallback MAC from OS entropy |

Everything else is std: TCP and UDP sockets, threads and `mpsc`, `sync_all` plus rename for atomic output, `route` and `networksetup` or `/sys/class/net` for the MAC, hand parsed JPEG SOF, hand written PDF.

## Verification

- Golden vectors below, produced once from the reference builders.
- Fake scanner on loopback for discovery, control and scan, with ephemeral ports.
- Scan transcript asserts the exact CDB order and every header MAC.
- PDF test parses its own xref, checks raw JPEG bytes, `/Rotate 180`, MediaBox and page count, and checks temp cleanup.
- Failure cases: jam, empty feeder, triple button notice, truncated frame, registration -2 and -7.
- Final gate is one press on the real iX500. It has not run yet.

## Golden vectors

```
window300auto 012c012c000028d0000044dc0500000000000000000000000000000000000000
mode300auto   00030101d000c1808080908080000000000000000000000000000000000000300010012c012c05810000000028d0000044dc040000000000000000000000000000000000000000000000000000000000
window150a4   00960096000026c1000036d00500000000000000000000000000000000000000
mode150a4     00030000d000c18080808080800000000000000000000000000000000000003000100096009605810500000026c1000036d0040000000000000000000000000000000000000000000000000000000000
window600a4   02580258000026c1000036d00500000000000000000000000000000000000000
mode600a4     00030000d000c18080808080808000000000000000000000000000000000003000100258025805810a00000026c1000036d0040000000000000000000000000000000000000000000000000000000000
read_front0   280000020000300000000000
read_back1    280000020080300000000100
info1         280080000080000020000100
pw1234        172131179178
pw0000        171129176174
reg_v6        00061e000000000000000001c0a8000a0000d7e131373231333131373931373800000000000000000000000000000000000000000000000000000000000000000000000007ea0b010c2238000011223344556677000046500000000000000000
reg_v3        00031e000000000000000001c0a8000a0000d7e131373231333131373931373800000000000000000000000000000000000000000000000000000000000000000000000007ea0b010c2238000011223344556677000000000000000000000000
```

`reg_v6` and `reg_v3` use host ID `0011223344556677`, MAC `aabbccddeeff`, host IP `192.168.0.10`, host port 55265, password `1234`, and local time 2026-11-01 12:34:56 at UTC-5. `v6` writes the time zone bias at offset 84, `v3` does not.

## Synthesis note

Base is candidate 2 (streamed PDF, explicit session state machine, strong fixture and fake-scanner plan). Grafts from candidate 1: config validation before discovery, `O_EXCL` identity creation with mode 0600, and notification dedupe on event plus counter. Rejected candidate 1 because its PDF has no rotation and its listener cannot check the sender. Rejected candidate 2's hand parsed argv in favour of clap, and its 5 second blanket capture ignore in favour of counter based dedupe.
