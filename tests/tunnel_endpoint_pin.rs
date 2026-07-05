//! Brings a real tunnel up and checks that the relay stays off it.
//!
//! Ignored by default: it needs administrator rights, a stored session, and it
//! rewrites the routing table. Run it deliberately, from an elevated shell:
//!
//!     cargo test --test tunnel_endpoint_pin -- --ignored --nocapture
//!
//! This is the case that a unit test cannot reach. Enabling Internet Connection
//! Sharing or the mobile hotspot turns IP forwarding on, which defeats the
//! socket binding WireGuard relies on to keep its own encrypted packets out of
//! the tunnel; they then match the tunnel's default route and loop. Turn the
//! hotspot on before running this and it covers that case too.

#![cfg(windows)]

use std::process::Command;
use valira_desktop::{api, store, tunnel};

const RELAY_ROUTE_CHECK: &str = "(Get-NetRoute -DestinationPrefix '{}/32' \
     -ErrorAction SilentlyContinue | Measure-Object).Count";

fn powershell(script: &str) -> String {
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .expect("powershell");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn host_routes_to(address: &str) -> usize {
    powershell(&RELAY_ROUTE_CHECK.replace("{}", address))
        .parse()
        .unwrap_or(0)
}

/// The address the internet sees, or empty when nothing gets through. Asked of
/// an IP rather than a name so a broken resolver cannot be mistaken for a
/// broken tunnel.
fn public_address() -> String {
    powershell(
        "try { ((Invoke-WebRequest 'http://1.1.1.1/cdn-cgi/trace' -TimeoutSec 8 \
         -UseBasicParsing).Content -split \"`n\" | Where-Object { $_ -match '^ip=' }) \
         -replace 'ip=','' } catch { '' }",
    )
}

#[test]
#[ignore = "brings a real tunnel up: needs admin, a session, and rewrites routes"]
fn the_relay_is_pinned_off_the_tunnel() {
    let session = store::load().expect("no stored session — sign in first");
    let base = std::env::var("VALIRA_API").unwrap_or_else(|_| "https://valiravpn.com".into());
    let client = api::Client::new(&base).expect("api client");
    let relay = client
        .relays()
        .expect("relays")
        .into_iter()
        .next()
        .expect("no relay announced");

    let address = relay.endpoint.clone();
    let profile = tunnel::Profile {
        private_key: session.private_key.clone(),
        addresses: vec![session.tunnel_ip.clone(), session.tunnel_ip6.clone()],
        dns: vec!["10.64.0.1".into(), "fda8:75e8:355::1".into()],
        peer_public_key: relay.public_key.clone(),
        endpoint: format!("{}:{}", relay.endpoint, relay.port),
    };

    assert_eq!(
        host_routes_to(&address),
        0,
        "a host route for {address} was already there before the test"
    );

    let before = public_address();
    tunnel::up(&profile).expect("tunnel up");

    let pinned = host_routes_to(&address);
    let mode = tunnel::mode();

    // The handshake and the first data packets need a moment.
    std::thread::sleep(std::time::Duration::from_secs(10));
    let tunnelled = public_address();
    let stats = tunnel::stats();

    // Down before asserting, so a failure never leaves the machine tunnelled.
    tunnel::down().expect("tunnel down");
    let left_behind = host_routes_to(&address);
    let after = public_address();

    println!("public address: {before} -> {tunnelled} -> {after}");
    println!("transfer: {} received, {} sent", stats.received, stats.sent);

    assert_eq!(mode, Some(tunnel::Mode::System), "expected the system backend");
    assert_eq!(pinned, 1, "the relay was not pinned to the physical gateway");
    assert_eq!(left_behind, 0, "the host route outlived the tunnel");

    // The point of the whole thing: traffic reaches the internet, and leaves by
    // the relay. Without the pin — with forwarding on — this comes back empty,
    // because the encrypted packets loop into the tunnel carrying them.
    assert!(!tunnelled.is_empty(), "nothing reached the internet through the tunnel");
    assert_ne!(tunnelled, before, "traffic did not leave by the relay");
    assert!(stats.received > 0, "the tunnel received nothing");
}
