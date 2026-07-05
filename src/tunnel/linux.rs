use super::{Profile, Stats, INTERFACE};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn config_path() -> std::path::PathBuf {
    std::path::Path::new("/etc/wireguard").join(format!("{INTERFACE}.conf"))
}

/// True when wireguard-tools is installed, which is what this backend drives.
/// Without it the embedded tunnel takes over. Probed once: it costs a process.
pub fn available() -> bool {
    static PRESENT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PRESENT.get_or_init(|| Command::new("wg-quick").arg("--help").output().is_ok())
}

pub fn up(profile: &Profile) -> Result<(), String> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, profile.render()).map_err(|e| format!("writing {}: {e}", path.display()))?;

    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));

    if active() {
        let _ = down();
    }

    let output = Command::new("wg-quick")
        .args(["up", INTERFACE])
        .output()
        .map_err(|e| format!("wg-quick not found: {e}"))?;

    if output.status.success() {
        return Ok(());
    }
    Err(last_line(&output.stderr, "wg-quick failed"))
}

pub fn down() -> Result<(), String> {
    let output = Command::new("wg-quick").args(["down", INTERFACE]).output();

    if let Ok(output) = &output {
        if output.status.success() && !active() {
            return Ok(());
        }
    }

    // wg-quick refuses when its own state file is gone; removing the link
    // still takes the tunnel down.
    let _ = Command::new("ip")
        .args(["link", "del", INTERFACE])
        .output();

    if active() {
        return Err(match output {
            Ok(output) => last_line(&output.stderr, "le tunnel est toujours actif"),
            Err(e) => format!("wg-quick not found: {e}"),
        });
    }
    Ok(())
}

pub fn active() -> bool {
    Command::new("wg")
        .args(["show", "interfaces"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .any(|name| name == INTERFACE)
        })
        .unwrap_or(false)
}

pub fn stats() -> Stats {
    let Ok(output) = Command::new("wg").args(["show", INTERFACE, "dump"]).output() else {
        return Stats::default();
    };

    let text = String::from_utf8_lossy(&output.stdout);
    // The first line describes the interface; peers follow.
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

pub fn elevated() -> bool {
    // Safe: geteuid cannot fail and touches no memory we own.
    unsafe { libc::geteuid() == 0 }
}

fn last_line(stderr: &[u8], fallback: &str) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .next_back()
        .unwrap_or(fallback)
        .to_string()
}
