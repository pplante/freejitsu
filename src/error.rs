use std::fmt;
use std::io;

use crate::scsi::Sense;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Config(String),
    Io(io::Error),
    Protocol(String),
    Discovery(String),
    Registration(i32),
    Scsi {
        cd: u8,
        status: u8,
        sense: Option<Sense>,
    },
    NoPaper,
    Feed(&'static str),
    Pdf(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Config(msg) => write!(f, "configuration: {msg}"),
            Error::Io(err) => write!(f, "I/O: {err}"),
            Error::Protocol(msg) => write!(f, "protocol: {msg}"),
            Error::Discovery(msg) => write!(f, "discovery: {msg}"),
            Error::Registration(-2) => write!(
                f,
                "registration rejected with status -2: the scanner wants its Wi-Fi password (use --password)"
            ),
            Error::Registration(-3) => write!(
                f,
                "registration rejected with status -3: wrong Wi-Fi password (check --password)"
            ),
            Error::Registration(-7) => write!(
                f,
                "registration rejected with status -7: this host is not paired with the scanner; pair over USB once with ScanSnap Home"
            ),
            Error::Registration(status) => {
                write!(
                    f,
                    "registration rejected with status {status}: unknown reason"
                )
            }
            Error::Scsi { cd, status, sense } => {
                write!(f, "SCSI command {cd:#04x} failed with status {status}")?;
                if let Some(sense) = sense {
                    write!(f, ": {sense}")?;
                }
                Ok(())
            }
            Error::NoPaper => write!(f, "no paper in the feeder"),
            Error::Feed(reason) => write!(f, "feeder error: {reason}"),
            Error::Pdf(msg) => write!(f, "PDF: {msg}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::Io(err)
    }
}
