use serde::Deserialize;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Deserialize, Clone)]
pub struct Device {
    pub id: i64,
    pub name: String,
    pub public_key: String,
    pub tunnel_ip: String,
    pub tunnel_ip6: String,
}

#[derive(Deserialize)]
pub struct Granted {
    pub token: String,
    pub device: Device,
}

/// Coordinates the control plane may or may not send. It does not today, so the
/// client falls back to the table in `geo.rs`; the moment the service starts
/// sending them these fields fill in on their own and take precedence. The
/// aliases cover the spellings a service is likely to pick.
#[derive(Deserialize, Clone, Copy, Default)]
pub struct Coords {
    #[serde(default, alias = "lat")]
    pub latitude: Option<f64>,
    #[serde(default, alias = "lon", alias = "lng", alias = "long")]
    pub longitude: Option<f64>,
}

impl Coords {
    pub fn pair(&self) -> Option<(f64, f64)> {
        match (self.latitude, self.longitude) {
            // Null Island is what a service sends when it means "unknown".
            (Some(lat), Some(lon)) if lat != 0.0 || lon != 0.0 => Some((lat, lon)),
            _ => None,
        }
    }
}

#[derive(Deserialize, Clone)]
pub struct Relay {
    pub country: String,
    pub city: String,
    pub endpoint: String,
    pub port: u16,
    pub public_key: String,
    #[serde(flatten)]
    pub coords: Coords,
}

#[derive(Deserialize, Clone)]
pub struct Exit {
    pub id: i64,
    /// The country's full English name. Always present.
    pub country: String,
    /// ISO 3166-1 alpha-2, upper case — but empty on some records, which is why
    /// `geo::resolve_code` also takes the name above.
    #[serde(default)]
    pub country_code: String,
    pub city: String,
    pub moniker: String,
    /// `provisioned` once the service has set this exit up for this device;
    /// `discovered` for the rest, which are available but not yet ours.
    pub state: String,
    /// How many peers the exit carries. A rough stand-in for load.
    #[serde(default)]
    pub peers: i64,
    /// Whether the node sits on a residential line or in a datacentre. Optional
    /// on purpose: the service leaves it null where it does not know, and a
    /// client that guessed — from the moniker, or the peer count — would be
    /// confidently wrong.
    #[serde(default)]
    pub residential: Option<bool>,
    /// The autonomous system the node announces from. Shown as provenance.
    #[serde(default)]
    pub asn: Option<String>,
    #[serde(flatten)]
    pub coords: Coords,
}

/// What kind of line an exit leaves by. The numbers cross into Slint, which
/// has no room for a Rust enum in a struct field.
pub const KIND_UNKNOWN: i32 = 0;
pub const KIND_RESIDENTIAL: i32 = 1;
pub const KIND_DATACENTRE: i32 = 2;

impl Exit {
    pub fn kind(&self) -> i32 {
        match self.residential {
            Some(true) => KIND_RESIDENTIAL,
            Some(false) => KIND_DATACENTRE,
            None => KIND_UNKNOWN,
        }
    }
}

#[derive(Deserialize)]
pub struct Account {
    pub expires_at: String,
    pub active: bool,
    pub devices: i64,
    pub max_devices: i64,
}

#[derive(Deserialize)]
struct Refusal {
    error: Option<String>,
    devices: Option<Vec<Device>>,
}

#[derive(Debug)]
pub enum Error {
    /// The account already carries the maximum number of devices.
    TooManyDevices(Vec<String>),
    /// The token has lapsed. They last a day, and there is no dedicated refresh
    /// route: signing in again with the key already registered returns the same
    /// device and a fresh token. Callers go through `authenticated` in main,
    /// which does exactly that and retries, so this rarely reaches a screen.
    Unauthorised,
    Refused(String),
    Unreachable(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::TooManyDevices(names) => write!(
                f,
                "Device limit reached. Revoke one of these: {}",
                names.join(", ")
            ),
            Error::Unauthorised => write!(f, "Session expired."),
            Error::Refused(message) => write!(f, "{message}"),
            Error::Unreachable(message) => write!(f, "Service unreachable: {message}"),
        }
    }
}

pub struct Client {
    base: String,
    http: reqwest::blocking::Client,
}

/// Chooses the cryptography behind TLS, once for the process.
///
/// `reqwest` is built without a provider of its own so that the client does not
/// depend on aws-lc — which is C, and would put cmake and a compiler between a
/// user on a minimal Linux install and a working build. Nothing connects before
/// this has run.
fn install_crypto() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // Already installed is not a failure: it only means something else got
        // there first, and any provider will do.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

impl Client {
    pub fn new(base: &str) -> Result<Self, String> {
        Self::with_timeout(base, TIMEOUT)
    }

    /// The same client on a deadline of the caller's choosing. The background
    /// refresh uses a short one so it cannot hold the work queue.
    pub fn with_timeout(base: &str, timeout: Duration) -> Result<Self, String> {
        install_crypto();
        let http = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .user_agent(concat!("valira-desktop/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            http,
        })
    }

    /// For the calls that return nothing worth reading but can still refuse.
    fn check(&self, response: reqwest::blocking::Response) -> Result<(), Error> {
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(Error::Unauthorised);
        }
        let body = response.bytes().unwrap_or_default();
        let refusal: Refusal = serde_json::from_slice(&body).unwrap_or(Refusal {
            error: None,
            devices: None,
        });
        Err(Error::Refused(
            refusal
                .error
                .unwrap_or_else(|| format!("the service answered {status}")),
        ))
    }

    fn read<T: for<'a> Deserialize<'a>>(
        &self,
        response: reqwest::blocking::Response,
    ) -> Result<T, Error> {
        let status = response.status();
        let body = response
            .bytes()
            .map_err(|e| Error::Unreachable(e.to_string()))?;

        if status.is_success() {
            return serde_json::from_slice(&body)
                .map_err(|e| Error::Refused(format!("unreadable response: {e}")));
        }

        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(Error::Unauthorised);
        }

        let refusal: Refusal = serde_json::from_slice(&body).unwrap_or(Refusal {
            error: None,
            devices: None,
        });

        if let Some(devices) = refusal.devices {
            return Err(Error::TooManyDevices(
                devices.into_iter().map(|d| d.name).collect(),
            ));
        }

        Err(Error::Refused(
            refusal
                .error
                .unwrap_or_else(|| format!("the service answered {status}")),
        ))
    }

    /// Signing in creates the device: the account number and the freshly
    /// generated public key go up, nothing else.
    pub fn sign_in(&self, account_number: &str, public_key: &str) -> Result<Granted, Error> {
        let response = self
            .http
            .post(format!("{}/api/v1/login", self.base))
            .json(&serde_json::json!({
                "account_number": account_number,
                "public_key": public_key,
            }))
            .send()
            .map_err(|e| Error::Unreachable(e.to_string()))?;
        self.read(response)
    }

    pub fn sign_out(&self, token: &str) -> Result<(), Error> {
        let response = self
            .http
            .post(format!("{}/api/v1/logout", self.base))
            .bearer_auth(token)
            .send()
            .map_err(|e| Error::Unreachable(e.to_string()))?;
        self.check(response)
    }

    pub fn account(&self, token: &str) -> Result<Account, Error> {
        let response = self
            .http
            .get(format!("{}/api/v1/account", self.base))
            .bearer_auth(token)
            .send()
            .map_err(|e| Error::Unreachable(e.to_string()))?;
        self.read(response)
    }

    pub fn relays(&self) -> Result<Vec<Relay>, Error> {
        let response = self
            .http
            .get(format!("{}/api/v1/relays", self.base))
            .send()
            .map_err(|e| Error::Unreachable(e.to_string()))?;
        self.read(response)
    }

    pub fn exits(&self, token: &str) -> Result<Vec<Exit>, Error> {
        let response = self
            .http
            .get(format!("{}/api/v1/exits", self.base))
            .bearer_auth(token)
            .send()
            .map_err(|e| Error::Unreachable(e.to_string()))?;
        self.read(response)
    }

    /// Picks an exit for this device, or clears it when given nothing.
    ///
    /// The answer matters: this is what tells the service where to send the
    /// traffic. Ignoring a refusal here brings the tunnel up over a selection
    /// the service never accepted — the interface reports itself connected and
    /// nothing reaches the internet, with nothing on screen to say why.
    pub fn choose_exit(&self, token: &str, exit_id: Option<i64>) -> Result<(), Error> {
        let response = self
            .http
            .post(format!("{}/api/v1/exit", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({ "exit_id": exit_id }))
            .send()
            .map_err(|e| Error::Unreachable(e.to_string()))?;
        self.check(response)
    }
}
