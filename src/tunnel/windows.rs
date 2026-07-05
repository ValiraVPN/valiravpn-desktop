use super::{Profile, Stats, INTERFACE};
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// A console program started from a windowed process gets a brand new console,
/// which flashes on screen. The tunnel is polled every couple of seconds, so
/// without this flag the desktop blinks for as long as the client runs.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn quiet(program: &str) -> Command {
    let mut command = Command::new(program);
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

/// The official WireGuard for Windows installs a service manager. Handing it a
/// profile makes it create the adapter and a matching service, which survives
/// the client being closed.
fn wireguard_exe() -> String {
    std::env::var("VALIRA_WIREGUARD").unwrap_or_else(|_| {
        r"C:\Program Files\WireGuard\wireguard.exe".to_string()
    })
}

/// True when WireGuard for Windows is installed. Its service manager puts the
/// data path in the kernel and keeps the tunnel alive after this process exits,
/// so it is preferred; without it the embedded tunnel takes over.
pub fn available() -> bool {
    std::path::Path::new(&wireguard_exe()).exists()
}

fn config_path() -> std::path::PathBuf {
    let base = std::env::var("PROGRAMDATA").unwrap_or_else(|_| r"C:\ProgramData".to_string());
    std::path::Path::new(&base)
        .join("ValiraVPN")
        .join(format!("{INTERFACE}.conf"))
}

fn service_name() -> String {
    format!("WireGuardTunnel${INTERFACE}")
}

pub fn up(profile: &Profile) -> Result<(), String> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, profile.render()).map_err(|e| format!("writing {}: {e}", path.display()))?;

    if active() {
        let _ = down();
    }

    // Read before the tunnel exists: once it is up the default route is its
    // own, and this would answer with the tunnel instead of the way out of the
    // machine.
    let gateway = default_gateway();

    let output = quiet(&wireguard_exe())
        .args(["/installtunnelservice", &path.to_string_lossy()])
        .output()
        .map_err(|e| {
            format!("WireGuard for Windows not found; install it: {e}")
        })?;

    if !output.status.success() {
        return Err(last_line(&output.stderr, "installing the tunnel failed"));
    }

    pin_endpoint(&profile.endpoint, gateway.as_deref());
    Ok(())
}

/// Routes the relay through the physical gateway, so the tunnel's own encrypted
/// packets can never be routed into the tunnel that carries them.
///
/// WireGuard normally prevents that by binding its socket to the interface the
/// relay is reachable on. Turning on IP forwarding defeats it — and Windows
/// turns forwarding on the moment Internet Connection Sharing or the mobile
/// hotspot starts. The encrypted packets are then routed like any others, match
/// the tunnel's own default route, and loop.
///
/// Measured with the hotspot running: the handshake completes, then 92 bytes
/// come back against megabytes sent, and the machine loses the network
/// altogether. With this host route in place, on the same hotspot: traffic
/// flows, and the public address is the relay's.
///
/// Costs nothing when forwarding is off, where the socket binding already
/// suffices. The embedded backend pins the endpoint the same way, in
/// `embedded::net`.
fn pin_endpoint(endpoint: &str, gateway: Option<&str>) {
    let (Some(address), Some(gateway)) = (endpoint_address(endpoint), gateway) else {
        return;
    };

    // Idempotent: a stale route from a previous run would make the add fail.
    let _ = quiet("route").args(["delete", &address]).output();
    let _ = quiet("route")
        .args([
            "add",
            &address,
            "mask",
            "255.255.255.255",
            gateway,
            "metric",
            "1",
        ])
        .output();
}

fn unpin_endpoint() {
    // Read back from the profile on disk rather than kept in memory, so a
    // client that was restarted still knows what to clean up.
    let Ok(config) = std::fs::read_to_string(config_path()) else {
        return;
    };
    let endpoint = config
        .lines()
        .find_map(|line| line.trim().strip_prefix("Endpoint"))
        .and_then(|value| value.split_once('='))
        .map(|(_, value)| value.trim());

    if let Some(address) = endpoint.and_then(endpoint_address) {
        let _ = quiet("route").args(["delete", &address]).output();
    }
}

/// The relay's address out of `host:port`. IPv6 endpoints are bracketed, and
/// are left alone: `route add` takes IPv4 here, and the v6 default is not what
/// carries this tunnel.
fn endpoint_address(endpoint: &str) -> Option<String> {
    let host = endpoint.trim();
    if host.starts_with('[') {
        return None;
    }
    let host = host.rsplit_once(':').map(|(host, _)| host).unwrap_or(host);
    host.parse::<std::net::Ipv4Addr>()
        .ok()
        .map(|address| address.to_string())
}

/// The gateway the machine uses right now, read through PowerShell because
/// `route print` is localised and this is not.
fn default_gateway() -> Option<String> {
    let output = quiet("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-NetRoute -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue \
             | Sort-Object RouteMetric | Select-Object -First 1).NextHop",
        ])
        .output()
        .ok()?;

    let gateway = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!gateway.is_empty() && gateway != "0.0.0.0").then_some(gateway)
}

pub fn down() -> Result<(), String> {
    let output = quiet(&wireguard_exe())
        .args(["/uninstalltunnelservice", INTERFACE])
        .output()
        .map_err(|e| format!("WireGuard for Windows not found: {e}"))?;

    // The command only asks the service manager to remove the tunnel; the
    // removal, and the route cleanup that goes with it, finishes afterwards.
    // Returning before it has means whatever comes next races that cleanup.
    //
    // Polled tightly because asking is now nearly free: the service goes in
    // roughly 340 ms, and this notices within 10 of them rather than rounding
    // up to the next 100.
    for _ in 0..500 {
        if !active() {
            // Only once the tunnel is really gone: dropping the pin while it
            // still stands would send the relay's own packets back into it.
            unpin_endpoint();
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    if output.status.success() {
        unpin_endpoint();
        return Ok(());
    }
    Err(last_line(&output.stderr, "the tunnel is still up"))
}

/// Whether the tunnel service is running, asked of the service manager directly.
///
/// This used to shell out to `sc query`. That costs about 113 ms a call —
/// measured — and it is called on every status poll and repeatedly while the
/// tunnel comes down, which is most of what made closing the client feel slow.
/// Through the API it is a handle open and a struct read.
pub fn active() -> bool {
    use windows::core::HSTRING;
    use windows::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_MANAGER_CONNECT,
        SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STATUS,
    };

    unsafe {
        let Ok(manager) = OpenSCManagerW(None, None, SC_MANAGER_CONNECT) else {
            return false;
        };
        let service = OpenServiceW(
            manager,
            &HSTRING::from(service_name()),
            SERVICE_QUERY_STATUS,
        );
        // Not installed: the tunnel is not up, which is the answer we want.
        let Ok(service) = service else {
            let _ = CloseServiceHandle(manager);
            return false;
        };

        let mut status = SERVICE_STATUS::default();
        let running = QueryServiceStatus(service, &mut status).is_ok()
            && (status.dwCurrentState == SERVICE_RUNNING
                || status.dwCurrentState == SERVICE_START_PENDING);

        let _ = CloseServiceHandle(service);
        let _ = CloseServiceHandle(manager);
        running
    }
}

pub fn stats() -> Stats {
    // wireguard.exe ships the same wg utility; when absent we simply report
    // nothing rather than pretending the tunnel is idle.
    let Ok(output) = quiet("wg").args(["show", INTERFACE, "dump"]).output() else {
        return Stats::default();
    };

    let text = String::from_utf8_lossy(&output.stdout);
    let Some(peer) = text.lines().nth(1) else {
        return Stats::default();
    };

    let columns: Vec<&str> = peer.split('\t').collect();
    if columns.len() < 7 {
        return Stats::default();
    }

    let handshake: u64 = columns[4].parse().unwrap_or(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    Stats {
        received: columns[5].parse().unwrap_or(0),
        sent: columns[6].parse().unwrap_or(0),
        handshake_age: (handshake > 0).then(|| now.saturating_sub(handshake)),
    }
}

/// Installing a tunnel service requires administrator rights. Asking the
/// service manager to enumerate is a cheap way to find out.
pub fn elevated() -> bool {
    quiet("net")
        .args(["session"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn last_line(stderr: &[u8], fallback: &str) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .next_back()
        .unwrap_or(fallback)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_relay_out_of_an_endpoint() {
        assert_eq!(
            endpoint_address("203.0.113.7:51820").as_deref(),
            Some("203.0.113.7")
        );
        // A bare address is one too: the port is optional here.
        assert_eq!(
            endpoint_address("203.0.113.7").as_deref(),
            Some("203.0.113.7")
        );
        assert_eq!(endpoint_address("  203.0.113.7:51820  ").as_deref(), Some("203.0.113.7"));
    }

    #[test]
    fn refuses_what_it_cannot_pin() {
        // A host name has to be resolved before it can be routed to, and this
        // is not the place for that.
        assert_eq!(endpoint_address("relay.valiravpn.com:51820"), None);
        // IPv6 endpoints are bracketed, and `route add` wants IPv4 here.
        assert_eq!(endpoint_address("[2001:db8::1]:51820"), None);
        assert_eq!(endpoint_address(""), None);
    }
}
