//! Routing and DNS for the embedded tunnel.
//!
//! Two things have to be true for a full tunnel to work, and the second is the
//! one that is easy to miss:
//!
//!   * everything must leave through the tunnel interface, and
//!   * the encrypted packets themselves must not, or they would be routed into
//!     the tunnel that is carrying them and nothing would move at all.
//!
//! So a host route pins the relay to the physical gateway, and the default is
//! taken over by two halves — `0.0.0.0/1` and `128.0.0.0/1`. They beat the
//! existing default without deleting it, which means reverting is only ever
//! removing what was added: if this process dies without reverting, the original
//! default is still sitting there once the interface disappears.
//!
//! Everything here drives the tools that ship with the operating system. Nothing
//! in this file needs WireGuard installed.

use std::net::IpAddr;
use std::process::Command;
use tun_rs::SyncDevice;

/// Runs an OS tool without letting a console window flash on Windows.
fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new(program);
    command.args(args);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let output = command
        .output()
        .map_err(|error| format!("{program} : {error}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr
            .lines()
            .filter(|line| !line.trim().is_empty())
            .next_back()
            .unwrap_or("")
            .trim()
            .to_string();
        return Err(if detail.is_empty() {
            format!("{program} {} failed", args.join(" "))
        } else {
            detail
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

// ── Windows ──────────────────────────────────────────────────────────────────

#[cfg(windows)]
pub struct Routes {
    endpoint: IpAddr,
    index: u32,
}

#[cfg(windows)]
pub fn apply(
    _name: &str,
    device: &SyncDevice,
    endpoint: IpAddr,
    dns: &[String],
) -> Result<Routes, String> {
    let index = device
        .if_index()
        .map_err(|error| format!("index de l'interface : {error}"))?;

    // The relay first: if this fails the tunnel would deadlock on itself.
    if let Some(gateway) = default_gateway() {
        let _ = run(
            "route",
            &[
                "add",
                &endpoint.to_string(),
                "mask",
                "255.255.255.255",
                &gateway,
                "metric",
                "1",
            ],
        );
    }

    for prefix in ["0.0.0.0/1", "128.0.0.0/1"] {
        run(
            "netsh",
            &[
                "interface",
                "ipv4",
                "add",
                "route",
                &format!("prefix={prefix}"),
                &format!("interface={index}"),
                "metric=1",
                "store=active",
            ],
        )?;
    }

    apply_dns_windows(index, dns);
    Ok(Routes { endpoint, index })
}

#[cfg(windows)]
pub fn revert(routes: Routes) {
    let _ = run("route", &["delete", &routes.endpoint.to_string()]);
    for prefix in ["0.0.0.0/1", "128.0.0.0/1"] {
        let _ = run(
            "netsh",
            &[
                "interface",
                "ipv4",
                "delete",
                "route",
                &format!("prefix={prefix}"),
                &format!("interface={}", routes.index),
                "store=active",
            ],
        );
    }
    // The resolver settings belong to the interface and go with it, but say so
    // explicitly in case the adapter outlives us by a moment.
    let _ = run(
        "netsh",
        &[
            "interface",
            "ipv4",
            "set",
            "dnsservers",
            &format!("name={}", routes.index),
            "dhcp",
        ],
    );
}

#[cfg(windows)]
fn apply_dns_windows(index: u32, dns: &[String]) {
    let servers: Vec<&String> = dns
        .iter()
        .filter(|server| {
            server
                .parse::<IpAddr>()
                .map(|address| address.is_ipv4())
                .unwrap_or(false)
        })
        .collect();

    let Some(primary) = servers.first() else {
        return;
    };
    let _ = run(
        "netsh",
        &[
            "interface",
            "ipv4",
            "set",
            "dnsservers",
            &format!("name={index}"),
            "static",
            primary,
            "primary",
            "validate=no",
        ],
    );
    for (position, server) in servers.iter().enumerate().skip(1) {
        let _ = run(
            "netsh",
            &[
                "interface",
                "ipv4",
                "add",
                "dnsservers",
                &format!("name={index}"),
                server,
                &format!("index={}", position + 1),
                "validate=no",
            ],
        );
    }
}

/// The gateway the machine used before we touched anything. Read through
/// PowerShell because `route print` is localised and this is not.
#[cfg(windows)]
fn default_gateway() -> Option<String> {
    let out = run(
        "powershell",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-NetRoute -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue \
             | Sort-Object RouteMetric | Select-Object -First 1).NextHop",
        ],
    )
    .ok()?;

    let gateway = out.trim().to_string();
    (!gateway.is_empty() && gateway != "0.0.0.0").then_some(gateway)
}

// ── Linux ────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
pub struct Routes {
    endpoint: IpAddr,
    name: String,
    resolv: Option<Vec<u8>>,
}

#[cfg(target_os = "linux")]
pub fn apply(
    name: &str,
    _device: &SyncDevice,
    endpoint: IpAddr,
    dns: &[String],
) -> Result<Routes, String> {
    if let Some((gateway, interface)) = linux_route_to(endpoint) {
        let _ = run(
            "ip",
            &[
                "route",
                "add",
                &format!("{endpoint}/32"),
                "via",
                &gateway,
                "dev",
                &interface,
            ],
        );
    }

    for half in ["0.0.0.0/1", "128.0.0.0/1"] {
        run("ip", &["route", "add", half, "dev", name])?;
    }
    for half in ["::/1", "8000::/1"] {
        let _ = run("ip", &["-6", "route", "add", half, "dev", name]);
    }

    let resolv = write_resolv_conf(dns);
    Ok(Routes {
        endpoint,
        name: name.to_string(),
        resolv,
    })
}

#[cfg(target_os = "linux")]
pub fn revert(routes: Routes) {
    let _ = run("ip", &["route", "del", &format!("{}/32", routes.endpoint)]);
    for half in ["0.0.0.0/1", "128.0.0.0/1"] {
        let _ = run("ip", &["route", "del", half, "dev", &routes.name]);
    }
    for half in ["::/1", "8000::/1"] {
        let _ = run("ip", &["-6", "route", "del", half, "dev", &routes.name]);
    }
    if let Some(previous) = routes.resolv {
        let _ = std::fs::write("/etc/resolv.conf", previous);
    }
}

/// `ip route get` answers with the gateway and interface the kernel would use,
/// which is exactly the pair the relay has to keep using.
#[cfg(target_os = "linux")]
fn linux_route_to(endpoint: IpAddr) -> Option<(String, String)> {
    let out = run("ip", &["route", "get", &endpoint.to_string()]).ok()?;
    let fields: Vec<&str> = out.split_whitespace().collect();
    let gateway = field_after(&fields, "via")?;
    let interface = field_after(&fields, "dev")?;
    Some((gateway, interface))
}

#[cfg(target_os = "linux")]
fn write_resolv_conf(dns: &[String]) -> Option<Vec<u8>> {
    if dns.is_empty() {
        return None;
    }
    let previous = std::fs::read("/etc/resolv.conf").ok();
    let body: String = dns
        .iter()
        .map(|server| format!("nameserver {server}\n"))
        .collect();
    std::fs::write("/etc/resolv.conf", body).ok()?;
    previous
}

// ── macOS ────────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
pub struct Routes {
    endpoint: IpAddr,
    name: String,
    dns_services: Vec<String>,
}

#[cfg(target_os = "macos")]
pub fn apply(
    name: &str,
    _device: &SyncDevice,
    endpoint: IpAddr,
    dns: &[String],
) -> Result<Routes, String> {
    if let Some(gateway) = macos_gateway_to(endpoint) {
        let _ = run("route", &["-n", "add", "-host", &endpoint.to_string(), &gateway]);
    }

    for half in ["0.0.0.0/1", "128.0.0.0/1"] {
        run("route", &["-n", "add", "-net", half, "-interface", name])?;
    }

    let dns_services = apply_dns_macos(dns);
    Ok(Routes {
        endpoint,
        name: name.to_string(),
        dns_services,
    })
}

#[cfg(target_os = "macos")]
pub fn revert(routes: Routes) {
    let _ = run("route", &["-n", "delete", "-host", &routes.endpoint.to_string()]);
    for half in ["0.0.0.0/1", "128.0.0.0/1"] {
        let _ = run("route", &["-n", "delete", "-net", half, "-interface", &routes.name]);
    }
    for service in &routes.dns_services {
        let _ = run("networksetup", &["-setdnsservers", service, "Empty"]);
    }
}

#[cfg(target_os = "macos")]
fn macos_gateway_to(endpoint: IpAddr) -> Option<String> {
    let out = run("route", &["-n", "get", &endpoint.to_string()]).ok()?;
    out.lines()
        .find_map(|line| line.trim().strip_prefix("gateway:"))
        .map(|gateway| gateway.trim().to_string())
        .filter(|gateway| !gateway.is_empty())
}

/// macOS keeps resolvers per network service, not per interface, so every
/// service gets pointed at the tunnel and reset on the way out.
#[cfg(target_os = "macos")]
fn apply_dns_macos(dns: &[String]) -> Vec<String> {
    if dns.is_empty() {
        return Vec::new();
    }
    let Ok(listing) = run("networksetup", &["-listallnetworkservices"]) else {
        return Vec::new();
    };

    let mut touched = Vec::new();
    for service in listing.lines().skip(1) {
        // A leading asterisk marks a disabled service.
        let service = service.trim();
        if service.is_empty() || service.starts_with('*') {
            continue;
        }
        let mut args = vec!["-setdnsservers", service];
        args.extend(dns.iter().map(String::as_str));
        if run("networksetup", &args).is_ok() {
            touched.push(service.to_string());
        }
    }
    touched
}

// ── shared ───────────────────────────────────────────────────────────────────

#[cfg(any(target_os = "linux", test))]
fn field_after(fields: &[&str], key: &str) -> Option<String> {
    fields
        .iter()
        .position(|field| *field == key)
        .and_then(|at| fields.get(at + 1))
        .map(|value| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_value_that_follows_a_key() {
        let line = "1.2.3.4 via 192.168.1.1 dev eth0 src 192.168.1.20 uid 0";
        let fields: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(field_after(&fields, "via").as_deref(), Some("192.168.1.1"));
        assert_eq!(field_after(&fields, "dev").as_deref(), Some("eth0"));
        assert_eq!(field_after(&fields, "absent"), None);
    }

    #[test]
    fn a_direct_route_has_no_gateway() {
        // On the same subnet the kernel answers without `via`, and pinning the
        // relay to a gateway that does not exist would be worse than not
        // pinning it at all.
        let line = "10.0.0.5 dev eth0 src 10.0.0.2 uid 0";
        let fields: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(field_after(&fields, "via"), None);
        assert_eq!(field_after(&fields, "dev").as_deref(), Some("eth0"));
    }
}
