use freejitsu::config::{Config, Invocation};
use freejitsu::daemon;
use freejitsu::error::Error;
use freejitsu::service;

fn main() {
    let result = match Config::parse_args() {
        Invocation::Run(cfg) => daemon::run(cfg),
        Invocation::Install(cfg) => service::install(&cfg),
        Invocation::Uninstall => service::uninstall(),
    };
    if let Err(err) = result {
        eprintln!("freejitsu: {err}");
        let code = if matches!(&err, Error::NoPaper) { 2 } else { 1 };
        std::process::exit(code);
    }
}
