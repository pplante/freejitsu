use std::path::{Path, PathBuf};
use std::process::Command;

use clap::ValueEnum;

use crate::config::{self, Config};
use crate::error::{Error, Result};

const LABEL: &str = "com.freejitsu.daemon";

pub fn install(cfg: &Config) -> Result<()> {
    ensure_macos()?;
    let exe = std::env::current_exe()?;
    let home = config::home_dir();
    let plist_path = agent_path(&home);
    let log = log_path(&home);

    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if let Some(dir) = plist_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&plist_path, plist(&program_arguments(cfg, &exe), &log))?;

    let target = target();
    let _ = launchctl(&["bootout", &target]);
    let domain = domain();
    let plist_arg = plist_path.display().to_string();
    launchctl(&["bootstrap", &domain, &plist_arg])?;

    println!("freejitsu: plist {}", plist_path.display());
    println!("freejitsu: log {}", log.display());
    if cfg.airscan {
        println!("freejitsu: AirScan port {}", cfg.airscan_port);
    }
    if cfg.password.is_some() {
        println!(
            "freejitsu: the Wi-Fi password is not written to the plist; store it with a run using --password"
        );
    }
    Ok(())
}

pub fn uninstall() -> Result<()> {
    ensure_macos()?;
    let _ = launchctl(&["bootout", &target()]);
    let plist_path = agent_path(&config::home_dir());
    if let Err(err) = std::fs::remove_file(&plist_path)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        return Err(err.into());
    }
    println!("freejitsu: removed {}", plist_path.display());
    Ok(())
}

fn ensure_macos() -> Result<()> {
    if cfg!(target_os = "macos") {
        Ok(())
    } else {
        Err(Error::Service(
            "launchd install is only supported on macOS".into(),
        ))
    }
}

fn program_arguments(cfg: &Config, exe: &Path) -> Vec<String> {
    let mut args = vec![
        exe.to_string_lossy().into_owned(),
        "--ip".into(),
        cfg.ip.to_string(),
        "--out".into(),
        cfg.out_dir.to_string_lossy().into_owned(),
        "--dpi".into(),
        cfg.dpi.as_u16().to_string(),
        "--page".into(),
        cfg.page
            .to_possible_value()
            .map(|value| value.get_name().to_string())
            .unwrap_or_else(|| "auto".into()),
        "--identity".into(),
        cfg.identity.to_string_lossy().into_owned(),
    ];
    if cfg.simplex {
        args.push("--simplex".into());
    }
    if cfg.airscan {
        args.push("--airscan".into());
        args.push("--airscan-port".into());
        args.push(cfg.airscan_port.to_string());
    }
    args
}

fn plist(program: &[String], log: &Path) -> String {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n",
    );
    xml.push_str(&entry("Label", LABEL));
    xml.push_str("\t<key>ProgramArguments</key>\n\t<array>\n");
    for arg in program {
        xml.push_str("\t\t<string>");
        xml.push_str(&escape(arg));
        xml.push_str("</string>\n");
    }
    xml.push_str("\t</array>\n");
    xml.push_str("\t<key>RunAtLoad</key>\n\t<true/>\n");
    xml.push_str("\t<key>KeepAlive</key>\n\t<true/>\n");
    xml.push_str("\t<key>ThrottleInterval</key>\n\t<integer>10</integer>\n");
    xml.push_str(&entry("StandardOutPath", &log.to_string_lossy()));
    xml.push_str(&entry("StandardErrorPath", &log.to_string_lossy()));
    xml.push_str("</dict>\n</plist>\n");
    xml
}

fn entry(key: &str, value: &str) -> String {
    format!("\t<key>{key}</key>\n\t<string>{}</string>\n", escape(value))
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

fn agent_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

fn log_path(home: &Path) -> PathBuf {
    home.join("Library/Logs/freejitsu.log")
}

fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

fn target() -> String {
    format!("{}/{}", domain(), LABEL)
}

fn launchctl(args: &[&str]) -> Result<()> {
    let output = Command::new("/bin/launchctl").args(args).output()?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    let message = if stderr.is_empty() {
        format!("launchctl {} failed with {}", args.join(" "), output.status)
    } else {
        format!("launchctl {} failed: {stderr}", args.join(" "))
    };
    Err(Error::Service(message))
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;
    use crate::config::{Dpi, PageSize, Ports};

    fn config(airscan: bool) -> Config {
        Config {
            ip: Ipv4Addr::new(192, 168, 0, 130),
            out_dir: PathBuf::from("/Users/test/Scans"),
            dpi: Dpi::D300,
            page: PageSize::Auto,
            simplex: false,
            identity: PathBuf::from("/Users/test/.config/freejitsu/identity"),
            password: None,
            once: false,
            ports: Ports::default(),
            airscan,
            airscan_port: 8090,
        }
    }

    fn args(cfg: &Config) -> Vec<String> {
        program_arguments(cfg, Path::new("/usr/local/bin/freejitsu"))
    }

    #[test]
    fn airscan_flags_follow_the_setting() {
        let with = args(&config(true));
        assert!(with.contains(&"--airscan".to_string()));
        let port = with.iter().position(|arg| arg == "--airscan-port");
        assert_eq!(port.map(|i| with[i + 1].as_str()), Some("8090"));

        let without = args(&config(false));
        assert!(!without.contains(&"--airscan".to_string()));
        assert!(!without.contains(&"--airscan-port".to_string()));
    }

    #[test]
    fn secrets_are_never_written_to_the_plist() {
        let mut cfg = config(false);
        cfg.password = Some("hunter2".into());
        cfg.once = true;
        let args = args(&cfg);
        assert!(!args.contains(&"--password".to_string()));
        assert!(!args.contains(&"--once".to_string()));
        assert!(!args.iter().any(|arg| arg.contains("hunter2")));
    }

    #[test]
    fn simplex_is_explicit_only_when_set() {
        let mut cfg = config(false);
        assert!(!args(&cfg).contains(&"--simplex".to_string()));
        cfg.simplex = true;
        assert!(args(&cfg).contains(&"--simplex".to_string()));
    }

    #[test]
    fn plist_escapes_program_arguments() {
        let program = vec!["/Applications/a & <b> \"c\".app".to_string()];
        let xml = plist(
            &program,
            Path::new("/Users/test/Library/Logs/freejitsu.log"),
        );
        assert!(xml.contains("/Applications/a &amp; &lt;b&gt; &quot;c&quot;.app"));
        assert!(!xml.contains("<b>"));
    }

    #[test]
    fn plist_has_the_launchd_keys() {
        let home = Path::new("/Users/test");
        let log = log_path(home);
        let xml = plist(&args(&config(true)), &log);
        for key in [
            "<key>Label</key>",
            "<key>RunAtLoad</key>\n\t<true/>",
            "<key>KeepAlive</key>\n\t<true/>",
            "<key>ThrottleInterval</key>\n\t<integer>10</integer>",
            "<key>StandardOutPath</key>",
            "<key>StandardErrorPath</key>",
        ] {
            assert!(xml.contains(key), "missing {key}");
        }
        assert_eq!(
            xml.matches("/Users/test/Library/Logs/freejitsu.log")
                .count(),
            2
        );
    }

    #[test]
    fn plutil_accepts_the_plist() {
        if !Path::new("/usr/bin/plutil").exists() {
            return;
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("freejitsu-plist-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let file = dir.join("com.freejitsu.daemon.plist");
        std::fs::write(&file, plist(&args(&config(true)), &log_path(&dir))).expect("write plist");

        let output = Command::new("/usr/bin/plutil")
            .arg("-lint")
            .arg(&file)
            .output()
            .expect("run plutil");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            output.status.success(),
            "plutil rejected the plist: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    #[test]
    fn paths_derive_from_home() {
        let home = Path::new("/Users/test");
        assert_eq!(
            agent_path(home),
            PathBuf::from("/Users/test/Library/LaunchAgents/com.freejitsu.daemon.plist")
        );
        assert_eq!(
            log_path(home),
            PathBuf::from("/Users/test/Library/Logs/freejitsu.log")
        );
    }
}
