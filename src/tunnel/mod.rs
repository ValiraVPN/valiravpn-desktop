//! Brings the WireGuard tunnel up and down.
//!
//! There are two backends behind this interface.
//!
//! The **system** one hands the profile to a WireGuard already installed on the
//! machine. That puts the data path in the kernel, which is the faster of the
//! two, and on Windows it also means the tunnel outlives this process.
//!
//! The **embedded** one carries the protocol itself, in this process, over a TUN
//! device it creates. Nothing has to be installed for it to work — on Windows it
//! only needs the `wintun.dll` shipped beside the executable.
//!
//! `up` prefers the system backend and falls back to the embedded one, so a
//! machine with nothing installed still gets a tunnel and a machine that has
//! WireGuard keeps its speed. Both still need elevated rights: creating a tunnel
//! interface and rewriting the routing table is privileged everywhere.

mod embedded;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
use linux as system;
#[cfg(target_os = "macos")]
use macos as system;
#[cfg(target_os = "windows")]
use windows as system;

pub const INTERFACE: &str = "valira";

/// Which of the two backends is carrying the traffic.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// A WireGuard installed on the machine. Kernel data path.
    System,
    /// This process. Survives nothing, needs nothing.
    Embedded,
}

/// What the tunnel needs in order to come up.
pub struct Profile {
    pub private_key: String,
    pub addresses: Vec<String>,
    pub dns: Vec<String>,
    pub peer_public_key: String,
    pub endpoint: String,
}

impl Profile {
    /// Renders the configuration in the format every WireGuard tool reads.
    pub fn render(&self) -> String {
        let mut body = String::from("[Interface]\n");
        body.push_str(&format!("PrivateKey = {}\n", self.private_key));
        body.push_str(&format!("Address = {}\n", self.addresses.join(", ")));
        if !self.dns.is_empty() {
            body.push_str(&format!("DNS = {}\n", self.dns.join(", ")));
        }
        body.push_str("\n[Peer]\n");
        body.push_str(&format!("PublicKey = {}\n", self.peer_public_key));
        body.push_str(&format!("Endpoint = {}\n", self.endpoint));
        body.push_str("AllowedIPs = 0.0.0.0/0, ::/0\n");
        body.push_str("PersistentKeepalive = 25\n");
        body
    }
}

/// Bytes moved through the tunnel, and how long ago the peer last answered.
#[derive(Default, Clone, Copy)]
pub struct Stats {
    pub received: u64,
    pub sent: u64,
    pub handshake_age: Option<u64>,
}

/// `VALIRA_TUNNEL=embedded` or `=system` pins one backend. Without it the choice
/// is automatic, which also means the embedded path is never exercised on a
/// machine that has WireGuard installed — this is how you test it there.
fn forced() -> Option<Mode> {
    match std::env::var("VALIRA_TUNNEL").ok()?.trim() {
        "embedded" => Some(Mode::Embedded),
        "system" => Some(Mode::System),
        _ => None,
    }
}

pub fn up(profile: &Profile) -> Result<(), String> {
    // Only ever tear down the OTHER backend here.
    //
    // Tearing down the one we are about to bring up raced its own service
    // manager: `/uninstalltunnelservice` returns before the removal finishes, so
    // the teardown's route cleanup could land after the new tunnel had installed
    // its own — leaving an adapter that is up, holds the default route, and
    // carries nothing. Each backend already replaces its own tunnel.
    match forced() {
        Some(Mode::Embedded) => {
            if system::available() {
                let _ = system::down();
            }
            return embedded::up(profile);
        }
        Some(Mode::System) => {
            let _ = embedded::down();
            return system::up(profile);
        }
        None => {}
    }

    if !system::available() {
        return embedded::up(profile);
    }
    let _ = embedded::down();

    // No quiet substitution when the system backend is there and fails. Falling
    // through to the embedded tunnel would report a tunnel that is up while the
    // machine reaches nothing, and hide the reason. The embedded path is the
    // answer to WireGuard being absent, not to it going wrong.
    system::up(profile)
}

pub fn down() -> Result<(), String> {
    let embedded = embedded::down();
    let system = if system::available() {
        system::down()
    } else {
        Ok(())
    };
    embedded.and(system)
}

/// Which backend is carrying traffic, if any. Answering this is also how the
/// caller learns the tunnel is up, so it never has to ask twice — each question
/// costs a process on Windows.
pub fn mode() -> Option<Mode> {
    if embedded::active() {
        Some(Mode::Embedded)
    } else if forced() != Some(Mode::Embedded) && system::available() && system::active() {
        Some(Mode::System)
    } else {
        None
    }
}

pub fn stats() -> Stats {
    if embedded::active() {
        embedded::stats()
    } else if system::available() {
        system::stats()
    } else {
        Stats::default()
    }
}

/// True when the process can create a tunnel interface at all.
pub fn elevated() -> bool {
    system::elevated()
}

/// Called on the way out, however the way out came about.
///
/// Both backends go down. The system one is a Windows service and would happily
/// outlive this process — which is exactly what surprised a user who closed the
/// window and stayed connected. Closing the client means disconnecting; a tunnel
/// nobody can see the state of, or turn off from the window that started it, is
/// worse than no tunnel.
///
/// Being killed outright still leaves the service standing: nothing runs to stop
/// it. Reopening the client and closing the tunnel is the way back from that.
pub fn release_on_exit() {
    let _ = down();
}
