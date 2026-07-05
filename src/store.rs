use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const ORGANISATION: &str = "ValiraVPN";
const APPLICATION: &str = "valira";

/// Everything the client has to remember between runs. The account number is
/// kept because it is the only way to refresh an expired token, exactly as the
/// Mullvad client keeps it.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Session {
    pub account_number: String,
    pub token: String,
    pub private_key: String,
    pub public_key: String,
    pub device_id: i64,
    pub device_name: String,
    pub tunnel_ip: String,
    pub tunnel_ip6: String,
    pub exit_id: Option<i64>,
}

pub fn directory() -> Result<PathBuf, String> {
    let dirs = directories::ProjectDirs::from("com", ORGANISATION, APPLICATION)
        .ok_or("no configuration directory on this system")?;
    let path = dirs.config_dir().to_path_buf();
    fs::create_dir_all(&path).map_err(|e| format!("creating {}: {e}", path.display()))?;
    restrict(&path);
    Ok(path)
}

fn session_path() -> Result<PathBuf, String> {
    Ok(directory()?.join("session.json"))
}

pub fn load() -> Option<Session> {
    let path = session_path().ok()?;
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn save(session: &Session) -> Result<(), String> {
    let path = session_path()?;
    let body = serde_json::to_string_pretty(session).map_err(|e| e.to_string())?;
    fs::write(&path, body).map_err(|e| format!("writing {}: {e}", path.display()))?;
    restrict(&path);
    Ok(())
}

pub fn clear() -> Result<(), String> {
    let path = session_path()?;
    if path.exists() {
        fs::remove_file(&path).map_err(|e| format!("removing {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Keeps the private key out of reach of other users on the machine.
#[cfg(unix)]
fn restrict(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let mode = if path.is_dir() { 0o700 } else { 0o600 };
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn restrict(_path: &std::path::Path) {
    // Windows inherits the per-user profile directory's access control.
}
