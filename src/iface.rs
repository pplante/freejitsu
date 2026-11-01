#[cfg(target_os = "linux")]
use std::fs;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::process::Command;

use crate::error::Result;

/// Local address the kernel would use to reach `target`. No packet is sent.
pub fn local_ip_towards(target: Ipv4Addr) -> Result<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.connect((target, 9))?;
    match socket.local_addr()? {
        SocketAddr::V4(addr) => Ok(*addr.ip()),
        SocketAddr::V6(_) => Ok(Ipv4Addr::UNSPECIFIED),
    }
}

/// MAC of the interface that routes to `target`, or None if it cannot be found.
#[allow(clippy::needless_return)]
pub fn mac_towards(target: Ipv4Addr) -> Option<[u8; 6]> {
    #[cfg(target_os = "macos")]
    {
        let text = output("route", &["-n", "get", &target.to_string()])?;
        let iface = text
            .lines()
            .find_map(|line| line.trim().strip_prefix("interface:").map(str::trim))?;
        let address = output("networksetup", &["-getmacaddress", iface])?;
        return valid_mac(&address);
    }
    #[cfg(target_os = "linux")]
    {
        let text = output("ip", &["route", "get", &target.to_string()])?;
        let mut words = text.split_whitespace();
        while let Some(word) = words.next() {
            if word == "dev" {
                let iface = words.next()?;
                let address = fs::read_to_string(format!("/sys/class/net/{iface}/address")).ok()?;
                return valid_mac(&address);
            }
        }
        return None;
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = target;
        None
    }
}

/// Random locally administered unicast MAC.
pub fn random_mac() -> [u8; 6] {
    let mut mac = [0u8; 6];
    getrandom::fill(&mut mac).expect("OS entropy for the fallback MAC");
    mac[0] = (mac[0] | 0x02) & 0xfe;
    mac
}

fn output(command: &str, args: &[&str]) -> Option<String> {
    let result = Command::new(command).args(args).output().ok()?;
    if !result.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&result.stdout).into_owned())
}

fn valid_mac(text: &str) -> Option<[u8; 6]> {
    for word in text.split_whitespace() {
        let word = word.trim_matches(|c: char| c == ',' || c == ';');
        let parts: Vec<&str> = word.split(':').collect();
        if parts.len() != 6 {
            continue;
        }
        let mut mac = [0u8; 6];
        let mut ok = true;
        for (slot, part) in mac.iter_mut().zip(parts) {
            match u8::from_str_radix(part, 16) {
                Ok(value) => *slot = value,
                Err(_) => {
                    ok = false;
                    break;
                }
            }
        }
        let placeholder = mac == [0u8; 6] || mac == [0x02, 0, 0, 0, 0, 0];
        if ok && !placeholder {
            return Some(mac);
        }
    }
    None
}
