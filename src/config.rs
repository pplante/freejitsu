use std::net::Ipv4Addr;
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

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
    pub airscan: bool,
    pub airscan_port: u16,
}

#[derive(Clone, Debug)]
pub enum Invocation {
    Run(Config),
    Install(Config),
    Uninstall,
}

impl Config {
    pub fn parse_args() -> Invocation {
        invocation(Args::parse())
    }
}

fn invocation(args: Args) -> Invocation {
    let command = args.command.clone();
    let home = home_dir();
    let cfg = Config {
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
        airscan: args.airscan,
        airscan_port: args.airscan_port,
    };
    match command {
        None => Invocation::Run(cfg),
        Some(Command::Install) => Invocation::Install(cfg),
        Some(Command::Uninstall) => Invocation::Uninstall,
    }
}

#[derive(Clone, Debug, Subcommand)]
pub enum Command {
    /// Install and start a launchd user agent (macOS)
    Install,
    /// Stop and remove the launchd user agent
    Uninstall,
}

#[derive(Parser, Debug)]
#[command(
    name = "freejitsu",
    version,
    about = "Scan button daemon for one ScanSnap iX500"
)]
struct Args {
    /// Scanner IP address
    #[arg(long, default_value = "192.168.0.130", global = true)]
    ip: Ipv4Addr,

    /// Output directory for PDFs
    #[arg(long, value_name = "DIR", global = true)]
    out: Option<PathBuf>,

    /// Scan resolution in dpi
    #[arg(long, value_enum, default_value = "300", global = true)]
    dpi: Dpi,

    /// Page size
    #[arg(long, value_enum, default_value = "auto", global = true)]
    page: PageSize,

    /// Write only the front side
    #[arg(long, global = true)]
    simplex: bool,

    /// Identity file
    #[arg(long, value_name = "FILE", global = true)]
    identity: Option<PathBuf>,

    /// Scanner Wi-Fi password
    #[arg(long, global = true)]
    password: Option<String>,

    /// Scan one job now and exit
    #[arg(long, global = true)]
    once: bool,

    /// Serve eSCL/AirScan over HTTP and Bonjour
    #[arg(long, conflicts_with = "once", global = true)]
    airscan: bool,

    /// AirScan HTTP port
    #[arg(long, default_value = "8090", global = true)]
    airscan_port: u16,

    #[command(subcommand)]
    command: Option<Command>,
}

pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Invocation {
        invocation(Args::parse_from(args))
    }

    #[test]
    fn no_subcommand_runs_the_daemon() {
        assert!(matches!(parse(&["freejitsu"]), Invocation::Run(_)));
        match parse(&["freejitsu", "--ip", "10.0.0.2"]) {
            Invocation::Run(cfg) => assert_eq!(cfg.ip, Ipv4Addr::new(10, 0, 0, 2)),
            other => panic!("expected run, got {other:?}"),
        }
    }

    #[test]
    fn install_accepts_flags_before_or_after_the_subcommand() {
        for args in [
            ["freejitsu", "install", "--airscan"],
            ["freejitsu", "--airscan", "install"],
        ] {
            match parse(&args) {
                Invocation::Install(cfg) => assert!(cfg.airscan),
                other => panic!("expected install, got {other:?}"),
            }
        }
    }

    #[test]
    fn uninstall_is_its_own_invocation() {
        assert!(matches!(
            parse(&["freejitsu", "uninstall"]),
            Invocation::Uninstall
        ));
    }

    #[test]
    fn airscan_still_conflicts_with_once() {
        assert!(Args::try_parse_from(["freejitsu", "--airscan", "--once"]).is_err());
    }
}
