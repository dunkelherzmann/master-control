//! Who a machine is: its host name, its operating system's release and the hardware address of its main network
//! interface, together. Every machine running Master Control, master or worker, is told apart by these three.

#[cfg(not(target_os = "linux"))]
use std::process::Command;
use std::sync::OnceLock;

use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq)]
pub struct Identity {
    pub hostname: String,
    /// The system and its release: `Ubuntu 24.04.3 LTS`, `macOS 27.2`, `Windows 10.0.28000`.
    pub os_release: String,
    /// The main interface's hardware address, lower case with colons; empty when none could be read.
    pub mac: String,
}

impl Identity {
    /// The three parts as one line. This is the machine's id wherever one is needed.
    pub fn id(&self) -> String {
        format!("{}|{}|{}", self.hostname.to_lowercase(), self.os_release, self.mac)
    }

    pub fn to_json(&self) -> Value {
        json!({ "id": self.id(), "hostname": self.hostname, "os_release": self.os_release, "mac": self.mac })
    }
}

/// The hardware address in an id, which stays the same when the system is updated or the machine renamed.
pub fn mac_of(id: &str) -> &str {
    id.rsplit('|').next().unwrap_or_default()
}

// Linux reads what it needs from files; the others ask the system's own tools.
#[cfg(not(target_os = "linux"))]
fn output(program: &str, args: &[&str]) -> String {
    let mut command = Command::new(program);
    command.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console window of its own.
        command.creation_flags(0x0800_0000);
    }
    command.output().map(|output| String::from_utf8_lossy(&output.stdout).into_owned()).unwrap_or_default()
}

fn tidy_mac(text: &str) -> String {
    let hex: String = text.chars().filter(char::is_ascii_hexdigit).collect::<String>().to_lowercase();
    if hex.len() != 12 || hex.chars().all(|digit| digit == '0') {
        return String::new();
    }
    hex.as_bytes().chunks(2).map(|pair| String::from_utf8_lossy(pair).into_owned()).collect::<Vec<_>>().join(":")
}

/// `PRETTY_NAME` from /etc/os-release, or the name and version when there is none.
pub fn linux_release(os_release: &str) -> String {
    let field = |name: &str| os_release.lines().find_map(|line| line.strip_prefix(name)?.strip_prefix('=')).map(|value| value.trim().trim_matches('"').to_string());
    field("PRETTY_NAME").filter(|name| !name.is_empty()).unwrap_or_else(|| [field("NAME"), field("VERSION_ID")].into_iter().flatten().collect::<Vec<_>>().join(" "))
}

/// The interface the default route goes through, from /proc/net/route.
pub fn linux_default_interface(routes: &str) -> Option<String> {
    routes.lines().skip(1).map(|line| line.split_whitespace().collect::<Vec<_>>()).find(|fields| fields.get(1) == Some(&"00000000")).map(|fields| fields[0].to_string())
}

/// The hardware address of a device, from `networksetup -listallhardwareports`: the real one, where the address the
/// interface shows a network may be a private, changing one.
pub fn macos_hardware_address(ports: &str, device: &str) -> String {
    let mut lines = ports.lines();
    while let Some(line) = lines.next() {
        if line.trim() == format!("Device: {device}") {
            return lines.next().and_then(|next| next.trim().strip_prefix("Ethernet Address:")).map(tidy_mac).unwrap_or_default();
        }
    }
    String::new()
}

/// The first adapter that is connected to something, from `getmac /fo csv /nh`.
pub fn windows_mac(getmac: &str) -> String {
    getmac
        .lines()
        .filter_map(|line| line.split_once(','))
        .find(|(_, transport)| transport.contains("\\Device\\"))
        .map(|(address, _)| tidy_mac(address))
        .unwrap_or_default()
}

/// `Windows` and the version's first three numbers, from what `ver` prints.
pub fn windows_release(ver: &str) -> String {
    let version: String = ver.split(|character: char| !(character.is_ascii_digit() || character == '.')).find(|part| part.matches('.').count() >= 2).unwrap_or_default().split('.').take(3).collect::<Vec<_>>().join(".");
    format!("Windows {version}").trim().to_string()
}

fn read() -> Identity {
    let hostname = crate::platform::hostname();
    #[cfg(target_os = "linux")]
    let (os_release, mac) = {
        let interface = linux_default_interface(&std::fs::read_to_string("/proc/net/route").unwrap_or_default());
        let address = |name: &str| tidy_mac(&std::fs::read_to_string(format!("/sys/class/net/{name}/address")).unwrap_or_default());
        // Without a default route: the first interface, by name, that has an address of its own.
        let mac = interface.map(|name| address(&name)).filter(|mac| !mac.is_empty()).unwrap_or_else(|| {
            let mut names: Vec<String> = std::fs::read_dir("/sys/class/net").map(|entries| entries.filter_map(|entry| entry.ok()?.file_name().into_string().ok()).collect()).unwrap_or_default();
            names.sort();
            names.iter().map(|name| address(name)).find(|mac| !mac.is_empty()).unwrap_or_default()
        });
        (linux_release(&std::fs::read_to_string("/etc/os-release").unwrap_or_default()), mac)
    };
    #[cfg(target_os = "macos")]
    let (os_release, mac) = {
        let route = output("route", &["-n", "get", "default"]);
        let device = route.lines().find_map(|line| line.trim().strip_prefix("interface:")).map_or("en0".to_string(), |name| name.trim().to_string());
        let ports = output("networksetup", &["-listallhardwareports"]);
        let mac = Some(macos_hardware_address(&ports, &device)).filter(|mac| !mac.is_empty()).unwrap_or_else(|| macos_hardware_address(&ports, "en0"));
        (format!("macOS {}", output("sw_vers", &["-productVersion"]).trim()), mac)
    };
    #[cfg(windows)]
    let (os_release, mac) = (windows_release(&output("cmd", &["/C", "ver"])), windows_mac(&output("getmac", &["/fo", "csv", "/nh"])));
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    let (os_release, mac) = (std::env::consts::OS.to_string(), String::new());
    Identity { hostname, os_release, mac }
}

/// This machine's identity. Read once: it doesn't change while the program runs.
pub fn this_machine() -> &'static Identity {
    static IDENTITY: OnceLock<Identity> = OnceLock::new();
    IDENTITY.get_or_init(read)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_parts_make_the_id() {
        let identity = Identity { hostname: "Example-Host".into(), os_release: "Ubuntu 24.04.3 LTS".into(), mac: "aa:bb:cc:dd:ee:ff".into() };
        assert_eq!(identity.id(), "example-host|Ubuntu 24.04.3 LTS|aa:bb:cc:dd:ee:ff");
        assert_eq!(mac_of(&identity.id()), "aa:bb:cc:dd:ee:ff");
        assert_eq!(identity.to_json()["os_release"], "Ubuntu 24.04.3 LTS");
    }

    #[test]
    fn each_systems_own_words_are_read() {
        assert_eq!(linux_release("NAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nPRETTY_NAME=\"Ubuntu 24.04.3 LTS\"\n"), "Ubuntu 24.04.3 LTS");
        assert_eq!(linux_release("NAME=Alpine\nVERSION_ID=3.20\n"), "Alpine 3.20");
        assert_eq!(linux_default_interface("Iface\tDestination\tGateway\nwlp2s0\t0000A8C0\t00000000\nenp3s0\t00000000\t0101A8C0\n").as_deref(), Some("enp3s0"));
        assert_eq!(linux_default_interface("Iface\tDestination\n"), None);
        let ports = "Hardware Port: Ethernet\nDevice: en5\nEthernet Address: 11:22:33:44:55:66\n\nHardware Port: Wi-Fi\nDevice: en0\nEthernet Address: AA:BB:CC:DD:EE:0F\n";
        assert_eq!((macos_hardware_address(ports, "en0"), macos_hardware_address(ports, "en9")), ("aa:bb:cc:dd:ee:0f".to_string(), String::new()));
        let getmac = "\"00-FF-11-22-33-44\",\"Medios desconectados\"\r\n\"A0-B1-C2-D3-E4-F5\",\"\\Device\\Tcpip_{0A1B}\"\r\n";
        assert_eq!(windows_mac(getmac), "a0:b1:c2:d3:e4:f5");
        assert_eq!(windows_release("\r\nMicrosoft Windows [Versi\u{f3}n 10.0.28000.3086]\r\n"), "Windows 10.0.28000");
        assert_eq!(tidy_mac("00:00:00:00:00:00"), "", "an all-zero address is no address");
    }

    #[test]
    fn this_machine_has_all_three() {
        let identity = this_machine();
        assert!(!identity.hostname.is_empty() && !identity.os_release.is_empty(), "{identity:?}");
        assert_eq!(identity.mac.len(), 17, "a hardware address: {identity:?}");
        assert_eq!(identity.id().matches('|').count(), 2);
    }
}
