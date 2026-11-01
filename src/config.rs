use std::net::Ipv4Addr;
use std::path::PathBuf;

use clap::{Parser, ValueEnum};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Dpi {
    #[value(name = "150")]
    D150,
    #[value(name = "200")]
    D200,
    #[value(name = "300")]
    D300,
    #[value(name = "600")]
    D600,
}

impl Dpi {
    pub fn as_u16(self) -> u16 {
        match self {
            Dpi::D150 => 150,
            Dpi::D200 => 200,
            Dpi::D300 => 300,
            Dpi::D600 => 600,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum PageSize {
    #[value(name = "auto")]
    Auto,
    #[value(name = "a4")]
    A4,
    #[value(name = "letter")]
    Letter,
    #[value(name = "legal")]
    Legal,
    #[value(name = "b5")]
    B5,
    #[value(name = "a5")]
    A5,
    #[value(name = "b6")]
    B6,
}

impl PageSize {
    /// Width and height in 1/1200 inch.
    pub fn dims(self) -> (u32, u32) {
        match self {
            PageSize::Auto => (0x28d0, 0x44dc),
            PageSize::A4 => (0x26c1, 0x36d0),
            PageSize::Letter => (0x27d8, 0x3390),
            PageSize::Legal => (0x27d8, 0x41a0),
            PageSize::B5 => (0x2196, 0x2f6e),
            PageSize::A5 => (0x1b50, 0x26c1),
            PageSize::B6 => (0x179f, 0x2196),
        }
    }

    pub fn automatic(self) -> bool {
        matches!(self, PageSize::Auto)
    }
}

/// UDP ports that are not advertised in the discovery reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ports {
    pub discovery: u16,
    pub startup: u16,
}

impl Default for Ports {
    fn default() -> Self {
        Ports {
            discovery: 52217,
            startup: 53220,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub ip: Ipv4Addr,
    pub out_dir: PathBuf,
    pub dpi: Dpi,
    pub page: PageSize,
    pub simplex: bool,
    pub identity: PathBuf,
    pub password: Option<String>,
    pub once: bool,
    pub ports: Ports,
}

impl Config {
    pub fn parse_args() -> Config {
        let args = Args::parse();
        let home = home_dir();
        Config {
            ip: args.ip,
            out_dir: args.out.unwrap_or_else(|| home.join("Scans")),
            dpi: args.dpi,
            page: args.page,
            simplex: args.simplex,
            identity: args
                .identity
                .unwrap_or_else(|| home.join(".config/freejitsu/identity")),
            password: args.password,
            once: args.once,
            ports: Ports::default(),
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "freejitsu",
    version,
    about = "Scan button daemon for one ScanSnap iX500"
)]
struct Args {
    /// Scanner IP address
    #[arg(long, default_value = "192.168.0.130")]
    ip: Ipv4Addr,

    /// Output directory for PDFs
    #[arg(long, value_name = "DIR")]
    out: Option<PathBuf>,

    /// Scan resolution in dpi
    #[arg(long, value_enum, default_value = "300")]
    dpi: Dpi,

    /// Page size
    #[arg(long, value_enum, default_value = "auto")]
    page: PageSize,

    /// Write only the front side
    #[arg(long)]
    simplex: bool,

    /// Identity file
    #[arg(long, value_name = "FILE")]
    identity: Option<PathBuf>,

    /// Scanner Wi-Fi password
    #[arg(long)]
    password: Option<String>,

    /// Scan one job now and exit
    #[arg(long)]
    once: bool,
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}
