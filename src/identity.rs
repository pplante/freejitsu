use std::fs;
use std::io::{ErrorKind, Write};
use std::path::Path;

use crate::discovery::ScannerInfo;
use crate::error::{Error, Result};

pub const DEFAULT_HOST_PORT: u16 = 55265;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub host_id: [u8; 8],
    pub mac: [u8; 6],
    pub host_port: u16,
    pub password: Option<String>,
}

pub fn load(path: &Path) -> Result<Option<Identity>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(Error::Io(err)),
    };
    let mut host_id = None;
    let mut mac = None;
    let mut host_port = DEFAULT_HOST_PORT;
    let mut password = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "host_id" => host_id = Some(unhex::<8>(value, "host_id")?),
            "mac" => mac = Some(unhex::<6>(value, "mac")?),
            "host_port" => {
                host_port = value
                    .trim()
                    .parse()
                    .map_err(|_| Error::Config(format!("identity file: bad host_port {value:?}")))?
            }
            "password" => password = Some(value.to_string()),
            _ => {}
        }
    }
    Ok(Some(Identity {
        host_id: host_id.ok_or_else(|| Error::Config("identity file: no host_id".into()))?,
        mac: mac.ok_or_else(|| Error::Config("identity file: no mac".into()))?,
        host_port,
        password,
    }))
}

/// Returns the stored identity, or takes the paired host ID from the scanner on first run.
pub fn ensure(path: &Path, info: &ScannerInfo, mac: [u8; 6]) -> Result<Identity> {
    if let Some(mut identity) = load(path)? {
        if identity.mac != mac {
            identity.mac = mac;
            store(path, &identity)?;
        }
        return Ok(identity);
    }
    if info.paired_host_id == [0u8; 8] {
        return Err(Error::Discovery(
            "the scanner has no paired host ID; pair this computer over USB once with ScanSnap Home"
                .into(),
        ));
    }
    let identity = Identity {
        host_id: info.paired_host_id,
        mac,
        host_port: DEFAULT_HOST_PORT,
        password: None,
    };
    store(path, &identity)?;
    Ok(identity)
}

/// Writes a temp file with mode 0600 and renames it into place.
pub fn store(path: &Path, identity: &Identity) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("identity");
    let mut file = None;
    let mut temp = path.to_path_buf();
    for attempt in 0..100 {
        temp = path.with_file_name(format!(".{name}.tmp-{}-{attempt}", std::process::id()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temp) {
            Ok(opened) => {
                file = Some(opened);
                break;
            }
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(Error::Io(err)),
        }
    }
    let mut file =
        file.ok_or_else(|| Error::Config("could not create a temp identity file".into()))?;

    let mut text = format!(
        "host_id={}\nmac={}\nhost_port={}\n",
        hex(&identity.host_id),
        hex(&identity.mac),
        identity.host_port
    );
    if let Some(password) = &identity.password {
        text.push_str(&format!("password={password}\n"));
    }
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp, path)?;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex<const N: usize>(value: &str, what: &str) -> Result<[u8; N]> {
    let value = value.trim();
    if value.len() != N * 2 {
        return Err(Error::Config(format!(
            "identity file: {what} must be {} hex digits",
            N * 2
        )));
    }
    let mut out = [0u8; N];
    for (index, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| Error::Config(format!("identity file: bad {what} {value:?}")))?;
    }
    Ok(out)
}
