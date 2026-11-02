use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use crate::config::Config;
use crate::control;
use crate::discovery::{self, Heartbeat, ScannerInfo};
use crate::error::{Error, Result};
use crate::identity::{self, Identity};
use crate::iface;
use crate::notify::{self, Event};
use crate::pdf::PdfWriter;
use crate::scan;
use crate::scsi::{self, ScsiChannel};

/// With `once`, discovers, registers, captures one job and returns. Otherwise runs until killed.
pub fn run(cfg: Config) -> Result<()> {
    std::fs::create_dir_all(&cfg.out_dir)?;
    let stored = identity::load(&cfg.identity)?;
    let mac = resolve_mac(cfg.ip, stored.as_ref());
    eprintln!("freejitsu: host MAC {}", mac_text(mac));

    if cfg.once {
        let mut stored = stored;
        let (info, identity) = open_session(&cfg, &mut stored, mac)?;
        let path = capture(&cfg, &info, &identity)?;
        println!("{}", path.display());
        return Ok(());
    }

    let host_port = stored
        .as_ref()
        .map(|identity| identity.host_port)
        .unwrap_or(identity::DEFAULT_HOST_PORT);
    let (tx, rx) = mpsc::channel();
    let _button = notify::listen(cfg.ip, host_port, tx.clone())?;
    let _startup = notify::listen(cfg.ip, cfg.ports.startup, tx.clone())?;
    drop(tx);

    let mut stored = stored;
    let mut info: Option<ScannerInfo> = None;
    let mut heartbeat: Option<Heartbeat> = None;
    let mut up = false;

    loop {
        if !up {
            match open_session(&cfg, &mut stored, mac) {
                Ok((found, _identity)) => {
                    eprintln!("freejitsu: registered with {}", found.name);
                    eprintln!("freejitsu: waiting for the Scan button");
                    heartbeat = Some(Heartbeat::start(&found, mac, cfg.ports.discovery));
                    info = Some(found);
                    up = true;
                }
                Err(err) => {
                    eprintln!("freejitsu: {err}");
                    match rx.recv_timeout(Duration::from_secs(2)) {
                        Ok(_) => {}
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => {
                            std::thread::sleep(Duration::from_secs(2));
                        }
                    }
                    continue;
                }
            }
        }

        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Event::Button) => {
                if let (Some(found), Some(identity)) = (info.as_ref(), stored.as_ref()) {
                    match capture(&cfg, found, identity) {
                        Ok(path) => eprintln!("freejitsu: wrote {}", path.display()),
                        Err(err) => eprintln!("freejitsu: scan failed: {err}"),
                    }
                }
            }
            Ok(Event::PowerOff) => {
                eprintln!("freejitsu: scanner powered off");
                heartbeat = None;
                info = None;
                up = false;
            }
            Ok(Event::PowerOn) => {}
            Err(RecvTimeoutError::Timeout) => {
                if !heartbeat.as_ref().is_some_and(Heartbeat::alive) {
                    eprintln!("freejitsu: heartbeat lost");
                    heartbeat = None;
                    info = None;
                    up = false;
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                heartbeat = None;
                info = None;
                up = false;
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
}

fn resolve_mac(target: std::net::Ipv4Addr, stored: Option<&Identity>) -> [u8; 6] {
    iface::mac_towards(target)
        .or_else(|| stored.map(|identity| identity.mac))
        .unwrap_or_else(iface::random_mac)
}

fn mac_text(mac: [u8; 6]) -> String {
    mac.iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn choose_password(cfg: &Config, identity: &Identity) -> String {
    cfg.password
        .clone()
        .or_else(|| identity.password.clone())
        .unwrap_or_default()
}

fn open_session(
    cfg: &Config,
    stored: &mut Option<Identity>,
    mac: [u8; 6],
) -> Result<(ScannerInfo, Identity)> {
    let info = discovery::discover(cfg.ip, mac, cfg.ports.discovery)?;
    let mut identity = identity::ensure(&cfg.identity, &info, mac)?;
    register(cfg, &info, &identity)?;
    if let Some(password) = &cfg.password
        && identity.password.as_deref() != Some(password.as_str())
    {
        identity.password = Some(password.clone());
        identity::store(&cfg.identity, &identity)?;
    }
    *stored = Some(identity.clone());
    Ok((info, identity))
}

fn register(cfg: &Config, info: &ScannerInfo, identity: &Identity) -> Result<()> {
    let host_ip = iface::local_ip_towards(cfg.ip)?;
    let password = choose_password(cfg, identity);
    control::register(info, identity, host_ip, &password, control::local_now())
}

fn capture(cfg: &Config, info: &ScannerInfo, identity: &Identity) -> Result<std::path::PathBuf> {
    let dest = output_path(cfg);
    let mut pdf = PdfWriter::create(dest)?;
    let dpi = cfg.dpi;
    let simplex = cfg.simplex;

    let result = match ScsiChannel::open(info, identity.mac) {
        Ok(mut channel) => {
            let result = scan::scan(&mut channel, dpi, cfg.page.into(), &mut |sheet| {
                pdf.add_page(&sheet.front, dpi.as_u16())?;
                if !simplex {
                    pdf.add_page(&sheet.back, dpi.as_u16())?;
                }
                Ok(())
            });
            drop(channel);
            result
        }
        Err(err) => Err(err),
    };

    if let Err(err) = scsi::release(info, identity.mac) {
        eprintln!("freejitsu: releasing the scanner failed: {err}");
    }

    match result {
        Ok(_) => {
            if pdf.pages() == 0 {
                return Err(Error::NoPaper);
            }
            pdf.finish()
        }
        Err(err @ Error::Pdf(_)) => {
            drop(pdf);
            Err(err)
        }
        Err(err) => {
            if pdf.pages() > 0 {
                let path = pdf.finish()?;
                eprintln!("freejitsu: scan ended early: {err}");
                Ok(path)
            } else {
                drop(pdf);
                Err(err)
            }
        }
    }
}

fn output_path(cfg: &Config) -> std::path::PathBuf {
    let now = control::local_now();
    let name = format!(
        "scan-{:04}{:02}{:02}-{:02}{:02}{:02}",
        now.year, now.month, now.day, now.hour, now.min, now.sec
    );
    let mut path = cfg.out_dir.join(format!("{name}.pdf"));
    let mut suffix = 2;
    while path.exists() {
        path = cfg.out_dir.join(format!("{name}-{suffix}.pdf"));
        suffix += 1;
    }
    path
}
