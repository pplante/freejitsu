use freejitsu::config::Config;
use freejitsu::daemon;
use freejitsu::error::Error;

fn main() {
    let cfg = Config::parse_args();
    if let Err(err) = daemon::run(cfg) {
        eprintln!("freejitsu: {err}");
        let code = if matches!(&err, Error::NoPaper) { 2 } else { 1 };
        std::process::exit(code);
    }
}
