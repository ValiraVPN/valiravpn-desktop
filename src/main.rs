// Hide the console window on Windows: this is a desktop application.
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

use valira_desktop::{api, flags, geo, keys, store, tunnel, worldmap};

#[cfg(windows)]
mod win32_frame;
mod sole_instance;
mod window_ops;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel, Weak};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

slint::include_modules!();

const DEFAULT_API: &str = "https://valiravpn.com";
const TUNNEL_POLL: Duration = Duration::from_secs(2);
const EXIT_POLL: Duration = Duration::from_secs(10);
/// What the background refresh waits before giving up. Far shorter than a
/// request the user is watching, because it holds the queue while it runs.
const POLL_TIMEOUT: Duration = Duration::from_secs(5);
/// Map colours, matching `Theme.map-land` and `Theme.map-border`. The renderer
/// paints with them directly rather than tinting a mask afterwards. Land is
/// translucent so the window behind the map still shows through.
const MAP_LAND: [u8; 4] = [0x2b, 0x31, 0x3a, 0x8a];
const MAP_BORDER: [u8; 4] = [0x7d, 0x87, 0x98, 0xa8];
/// How much larger than the pane the map is drawn, before being scaled back
/// down. See where it is used.
const MAP_SUPERSAMPLE: f32 = 2.0;

thread_local! {
    /// The last exit list the service sent, kept on the interface thread so
    /// typing in the filter redraws immediately instead of waiting for the next
    /// poll to come round.
    static EXIT_CACHE: RefCell<Vec<api::Exit>> = const { RefCell::new(Vec::new()) };

    /// Current map scale. Cities that merge into one node at a whole-world view
    /// have to come apart again once the view is close enough to tell them
    /// apart, so the grouping distance is divided by this.
    static MAP_ZOOM: Cell<f32> = const { Cell::new(1.0) };

    /// Countries the user has folded open or shut by hand. Only explicit choices
    /// are recorded: everything else follows the default, which is shut unless
    /// the country holds the exit currently in service.
    static FOLDED: RefCell<HashMap<String, bool>> = RefCell::new(HashMap::new());
}

/// Work that must not run on the interface thread.
enum Task {
    SignIn(String),
    LoadExits,
    PickExit(i64),
    SignOut,
}

/// What the worker sends back once it is done.
enum Outcome {
    SignedIn(Box<store::Session>, api::Account, api::Relay),
    Exits(Vec<api::Exit>),
    Relay(api::Relay),
    ExitPicked(i64),
    Account(api::Account),
    SignedOut,
    Failed(String),
}

impl Outcome {
    /// True when this concludes something the user started. The background
    /// polls must never clear a spinner they did not raise, or the buttons come
    /// back to life halfway through a sign-in.
    fn concludes_request(&self) -> bool {
        !matches!(
            self,
            Outcome::Exits(_) | Outcome::Account(_) | Outcome::Relay(_)
        )
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Asks a client that is already running to shut down, and returns without
    // opening a window of its own. The installer calls this before replacing
    // the binary: the tray keeps the old process alive with the file open, and
    // terminating it would strand the routes its tunnel had pinned.
    #[cfg(windows)]
    if std::env::args().any(|arg| arg == "--quit") {
        win32_frame::ask_running_client_to_quit(Duration::from_secs(10));
        return Ok(());
    }

    // One client to a machine. A second copy would open a window over a tunnel
    // it does not own, and the two would disagree about its state the moment
    // either touched it.
    if !sole_instance::claim() {
        // Someone double-clicking the icon while the window is hidden means
        // "show me the client", and answering that with nothing at all looks
        // like a program that failed to start.
        #[cfg(windows)]
        win32_frame::reveal_running_client();
        return Ok(());
    }

    guard_against_stranded_routes();

    let (app, gpu) = create_window()?;
    app.set_opaque(!gpu);

    let base = std::env::var("VALIRA_API").unwrap_or_else(|_| DEFAULT_API.to_string());

    let (orders, inbox) = mpsc::channel::<Task>();
    let handle = app.as_weak();

    // Every network call and every privileged command runs here, so the window
    // never stops answering, whatever the service does.
    std::thread::spawn({
        let handle = handle.clone();
        move || worker(base, inbox, handle)
    });

    wire(&app, orders.clone());
    watch_tunnel(app.as_weak());

    if let Some(session) = store::load() {
        apply_session(&app, &session);
        let _ = orders.send(Task::LoadExits);
        // Opened on the chooser, whatever exit is remembered. Which screen is
        // the honest one depends on the tunnel, and the first probe of the
        // watcher decides it a moment from now.
        show_chooser(&app, false);
    }

    if let Ok(preview) = std::env::var("VALIRA_UI_PREVIEW") {
        apply_ui_preview(&app, &preview);
    }

    #[cfg(windows)]
    {
        let weak = app.as_weak();
        slint::Timer::single_shot(Duration::from_millis(0), move || {
            if let Some(app) = weak.upgrade() {
                win32_frame::install_and_reveal(&app, gpu, on_show_failure);
            }
        });
    }
    #[cfg(not(windows))]
    {
        if let Err(reason) = app.show() {
            on_show_failure();
            return Err(reason.into());
        }
        // Opened filling the screen, as on Windows.
        let weak = app.as_weak();
        slint::Timer::single_shot(Duration::from_millis(0), move || {
            if let Some(app) = weak.upgrade() {
                window_ops::maximise(&app);
            }
        });
    }

    // The GL context is created on the first present, so a driverless machine
    // fails here rather than at startup.
    //
    // `until_quit`, because this client lives in the tray: closing the window
    // hides it and leaves the tunnel running, and only "Close" in the tray menu
    // ends the program. The plain loop would exit the moment the window went.
    let outcome = slint::run_event_loop_until_quit();
    let _ = app.hide();
    // Reached only when the tray menu asked to quit — closing the window merely
    // hides it. So this is the one place the tunnel comes down on the way out.
    tunnel::release_on_exit();
    if outcome.is_err() {
        on_show_failure();
    }
    outcome?;
    Ok(())
}

/// A panic anywhere must still put the routing table back. Without this a crash
/// while connected would leave the machine with no way out.
fn guard_against_stranded_routes() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tunnel::release_on_exit();
        previous(info);
    }));
}

/// Picks the renderer for this run, GPU first.
///
/// Only the GPU renderer can present a transparent window, which is what the
/// acrylic backdrop shows through, so it is the default. It cannot be probed up
/// front: Slint builds every renderer suspended and only creates the OpenGL
/// context when the window is first shown, long after the backend has been
/// committed for the process. So the retry is a relaunch rather than a rebuild
/// — see `on_show_failure`.
///
/// `VALIRA_RENDERER=software` or `=gpu` pins one of the two.
fn create_window() -> Result<(App, bool), slint::PlatformError> {
    let gpu = !matches!(
        std::env::var("VALIRA_RENDERER").as_deref(),
        Ok("software") | Ok("cpu")
    );
    select_backend(if gpu { "winit-femtovg" } else { "winit-software" });
    App::new().map(|app| (app, gpu))
}

fn select_backend(name: &str) {
    // Safe here: nothing else has been spawned yet, this is the first thing main
    // does.
    unsafe { std::env::set_var("SLINT_BACKEND", name) };
}

/// The window never appeared. On a machine with no usable OpenGL driver —
/// remote desktop, a VM without 3D, a fresh install before the display driver —
/// that is the GPU renderer failing to create its context. Slint cannot swap the
/// backend out mid-process, so start again with the software renderer pinned and
/// let this process go, rather than sitting on an event loop with no window and
/// no console to say why.
fn on_show_failure() {
    // Only the first attempt may retry: the child already has one pinned.
    if std::env::var("VALIRA_RENDERER").is_ok() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    // Handed over before the child starts, or it would find this process still
    // holding the claim and stand down.
    sole_instance::release();
    let relaunched = std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .env("VALIRA_RENDERER", "software")
        .spawn();
    if relaunched.is_ok() {
        std::process::exit(0);
    }
}

/// Test hook for the relaunch above, which is otherwise only reachable on a
/// machine with no OpenGL driver. Debug builds only.
#[cfg(debug_assertions)]
pub fn force_render_failure() -> bool {
    std::env::var("VALIRA_FORCE_RENDER_FAILURE").is_ok()
}

#[cfg(not(debug_assertions))]
pub fn force_render_failure() -> bool {
    false
}

/// Drives the window straight to one screen with sample data, so the interface
/// can be looked at without an account and without touching the stored session.
/// `VALIRA_UI_PREVIEW=signin|choosing|connected`, optionally suffixed with a
/// kind tab — `choosing:home`, `list:datacentre` — which is how the filter gets
/// looked at without a pointer.
fn apply_ui_preview(app: &App, which: &str) {
    let (which, kind) = match which.split_once(':') {
        Some((screen, "home")) => (screen, api::KIND_RESIDENTIAL),
        Some((screen, "datacentre")) => (screen, api::KIND_DATACENTRE),
        Some((screen, _)) => (screen, -1),
        None => (which, -1),
    };
    app.set_kind_filter(kind);

    match which {
        "signin" => {
            app.set_account_number(SharedString::new());
            app.set_screen(Screen::SignIn);
        }
        // Opens the chooser on the account's own exits, substituting nothing.
        // The way to look at what the service actually returned.
        "list" => show_chooser(app, true),
        "choosing" => {
            EXIT_CACHE.with(|cache| *cache.borrow_mut() = preview_exits());
            app.set_chosen_exit(3);
            apply_relay(app, &preview_relay());
            render_exits(app);
            show_chooser(app, true);
        }
        "connected" | "menu" | "profile" => {
            EXIT_CACHE.with(|cache| *cache.borrow_mut() = preview_exits());
            app.set_chosen_exit(3);
            app.set_account_expiry("active until 2027-01-14 · 2/5 devices".into());
            app.set_device_name("Keen Viper".into());
            app.set_tunnel_address("10.64.0.12/32".into());
            apply_relay(app, &preview_relay());
            render_exits(app);
            app.set_screen(Screen::Connected);
            if which == "profile" {
                app.set_profile_menu_open(true);
            }
            if which == "menu" {
                apply_tunnel(
                    app,
                    Some(tunnel::Mode::Embedded),
                    tunnel::Stats {
                        received: 4_713_984,
                        sent: 918_272,
                        handshake_age: Some(37),
                    },
                );
                app.set_tunnel_menu_open(true);
            }
        }
        _ => {}
    }
}

fn preview_relay() -> api::Relay {
    api::Relay {
        country: "fr".into(),
        city: "Paris".into(),
        endpoint: "203.0.113.1".into(),
        port: 51820,
        public_key: String::new(),
        coords: api::Coords::default(),
    }
}

/// Shaped like the real payload: the full English name beside an ISO-2 code,
/// `discovered` everywhere but the one exit the service has set up for us, and a
/// record with no code at all — the service sends those, and they still have to
/// find their flag and their place on the map.
fn preview_exits() -> Vec<api::Exit> {
    let sample = [
        (1, "Switzerland", "CH", "Zurich", "silent-ibex", "discovered"),
        (2, "Sweden", "SE", "Stockholm", "pale-auroch", "discovered"),
        (3, "Iceland", "IS", "Reykjavik", "cold-fulmar", "provisioned"),
        (4, "The Netherlands", "NL", "Amsterdam", "flat-heron", "discovered"),
        (5, "Canada", "CA", "Montreal", "quiet-marten", "discovered"),
        (6, "Japan", "JP", "Tokyo", "narrow-tanuki", "discovered"),
        (7, "United States", "US", "New York", "brisk-osprey", "discovered"),
        (8, "United States", "", "Los Angeles", "warm-coyote", "discovered"),
        (9, "Singapore", "SG", "Singapore", "damp-civet", "discovered"),
        (10, "Australia", "AU", "Sydney", "dry-quoll", "discovered"),
        (11, "Brazil", "BR", "Sao Paulo", "loud-agouti", "discovered"),
        (12, "South Africa", "ZA", "Johannesburg", "lean-caracal", "discovered"),
        (13, "Germany", "DE", "Frankfurt", "grey-marder", "discovered"),
        (14, "Czechia", "CZ", "Prague", "stark-jezek", "discovered"),
    ];
    sample
        .into_iter()
        .map(|(id, country, code, city, moniker, state)| api::Exit {
            id,
            country: country.to_string(),
            country_code: code.to_string(),
            city: city.to_string(),
            moniker: moniker.to_string(),
            state: state.to_string(),
            peers: 4,
            // A spread of the three cases, so the preview exercises every blip
            // and every tab: a few residential, one the service has no answer
            // for, the rest in datacentres.
            residential: match id % 5 {
                1 => Some(true),
                4 => None,
                _ => Some(false),
            },
            asn: Some(format!("AS{}", 12000 + id * 7)),
            coords: api::Coords::default(),
        })
        .collect()
}

fn elevation_hint() -> &'static str {
    if cfg!(target_os = "windows") {
        "Run ValiraVPN as administrator to bring the tunnel up."
    } else {
        "Run ValiraVPN with sudo to bring the tunnel up."
    }
}

fn wire(app: &App, orders: mpsc::Sender<Task>) {
    let handle = app.as_weak();

    app.on_sign_in({
        let orders = orders.clone();
        let handle = handle.clone();
        move || {
            let Some(app) = handle.upgrade() else { return };
            if app.get_busy() {
                return;
            }
            let digits: String = app
                .get_account_number()
                .chars()
                .filter(|c| c.is_ascii_digit())
                .collect();
            if digits.len() != 16 {
                notify(&app, "An account number is sixteen digits.", true);
                return;
            }
            clear_notice(&app);
            app.set_busy(true);
            let _ = orders.send(Task::SignIn(digits));
        }
    });

    app.on_pick_exit({
        let orders = orders.clone();
        let handle = handle.clone();
        move |id| {
            let Some(app) = handle.upgrade() else { return };
            if app.get_busy() {
                return;
            }
            app.set_busy(true);
            // Marks the row itself, so the wait is visible where it was asked
            // for rather than only in a status band.
            app.set_pending_exit(id);
            clear_notice(&app);
            let _ = orders.send(Task::PickExit(id as i64));
        }
    });

    app.on_disconnect({
        let handle = handle.clone();
        move || {
            let Some(app) = handle.upgrade() else { return };
            if app.get_busy() {
                return;
            }
            clear_notice(&app);
            // Down on screen the moment it is asked for, with no progress bar
            // to sit through. The teardown itself still takes about a third of
            // a second — the service manager has to finish pulling the routes
            // out, and cutting that short is exactly what strands them — but
            // there is nothing in that wait for the user. If it fails, the
            // reason is shown and the watcher puts the real state back within
            // its next two seconds.
            apply_tunnel(&app, None, tunnel::Stats::default());
            show_chooser(&app, false);
            tear_down(handle.clone());
        }
    });

    app.on_sign_out({
        let orders = orders.clone();
        let handle = handle.clone();
        move || {
            let Some(app) = handle.upgrade() else { return };
            if app.get_busy() {
                return;
            }
            // The tunnel goes straight away; revoking the device is a request
            // and takes its place in the queue behind whatever is in flight.
            app.set_busy(true);
            clear_notice(&app);
            tear_down(handle.clone());
            let _ = orders.send(Task::SignOut);
        }
    });

    app.on_pick_kind({
        let handle = handle.clone();
        move |kind| {
            let Some(app) = handle.upgrade() else { return };
            app.set_kind_filter(kind);
            render_exits(&app);
        }
    });

    app.on_minimise_window({
        let handle = handle.clone();
        move || {
            if let Some(app) = handle.upgrade() {
                window_ops::minimise(&app);
            }
        }
    });

    app.on_toggle_maximise_window({
        let handle = handle.clone();
        move || {
            if let Some(app) = handle.upgrade() {
                window_ops::toggle_maximise(&app);
            }
        }
    });

    app.on_drag_window({
        let handle = handle.clone();
        move || {
            if let Some(app) = handle.upgrade() {
                window_ops::start_drag(&app);
            }
        }
    });

    app.on_close_window({
        let handle = handle.clone();
        move || {
            let Some(app) = handle.upgrade() else { return };
            // Windows keeps the client alive behind its tray icon, so the
            // window only hides. Nowhere else is there a tray yet, and hiding
            // the only window of a program that cannot be brought back is a
            // trap: there the close button closes the client, tunnel and all.
            #[cfg(windows)]
            {
                let _ = app.hide();
            }
            #[cfg(not(windows))]
            {
                let _ = app;
                let _ = slint::quit_event_loop();
            }
        }
    });

    app.on_toggle_country({
        let handle = handle.clone();
        move |code| {
            let Some(app) = handle.upgrade() else { return };
            let code = code.to_string();
            FOLDED.with(|folded| {
                let mut folded = folded.borrow_mut();
                // Whatever it shows now is what the click is reversing, so read
                // the row rather than guessing at the default.
                let showing_open = app.get_exits().iter().any(|entry| {
                    entry.header && entry.code == code.as_str() && !entry.collapsed
                });
                folded.insert(code, !showing_open);
            });
            render_exits(&app);
        }
    });

    app.on_open_chooser({
        let handle = handle.clone();
        move || {
            if let Some(app) = handle.upgrade() {
                show_chooser(&app, true);
            }
        }
    });

    app.on_close_chooser({
        let handle = handle.clone();
        move || {
            if let Some(app) = handle.upgrade() {
                app.set_screen(Screen::Connected);
            }
        }
    });

    app.on_filter_changed({
        let handle = handle.clone();
        move |_| {
            if let Some(app) = handle.upgrade() {
                render_exits(&app);
            }
        }
    });

    app.on_dismiss_notice({
        let handle = handle.clone();
        move || {
            if let Some(app) = handle.upgrade() {
                clear_notice(&app);
            }
        }
    });

    // The land is drawn into a texture wider than the pane and kept. While the
    // view stays inside it, moving the map costs nothing at all: the pane just
    // shifts the picture it already holds. Only leaving that texture, or asking
    // it for more detail than it carries, draws a new one.
    app.on_map_view_changed({
        let handle = handle.clone();
        move |vx, vy, vw, vh, pane_w, pane_h| {
            let Some(app) = handle.upgrade() else { return };
            refresh_map_texture(&app, vx, vy, vw, vh, pane_w, pane_h, false);
        }
    });

    // Once the gesture stops, whatever was drawn quickly to keep up is drawn
    // again properly.
    app.on_map_view_settled({
        let handle = handle.clone();
        move |vx, vy, vw, vh, pane_w, pane_h| {
            let Some(app) = handle.upgrade() else { return };
            refresh_map_texture(&app, vx, vy, vw, vh, pane_w, pane_h, true);
        }
    });

    app.on_map_zoom_changed({
        let handle = handle.clone();
        move |zoom| {
            let Some(app) = handle.upgrade() else { return };
            let zoom = zoom.max(1.0);
            // Regrouping the markers is all a scale change asks for. Rebuilding
            // the exit list as well meant every wheel notch threw away and
            // rebuilt two hundred rows nobody had touched.
            if (MAP_ZOOM.with(|current| current.get()) - zoom).abs() < f32::EPSILON {
                return;
            }
            MAP_ZOOM.with(|current| current.set(zoom));
            regroup_map(&app);
        }
    });
}

fn worker(base: String, inbox: mpsc::Receiver<Task>, handle: Weak<App>) {
    let client = match api::Client::new(&base) {
        Ok(client) => client,
        Err(reason) => {
            report(&handle, Outcome::Failed(reason));
            return;
        }
    };
    // The background refresh gets a much shorter deadline than anything the
    // user asked for. It runs on the same thread, so every second it spends
    // waiting is a second the next request waits too — and a tunnel that has
    // stopped passing traffic makes each of its calls run to the very end.
    // Nothing is lost by giving up early: the refresh ignores its own failures
    // and comes back ten seconds later.
    let polling = match api::Client::with_timeout(&base, POLL_TIMEOUT) {
        Ok(polling) => polling,
        Err(reason) => {
            report(&handle, Outcome::Failed(reason));
            return;
        }
    };

    loop {
        // Waiting with a deadline lets the same loop refresh the exit list
        // while provisioning happens on the server.
        match inbox.recv_timeout(EXIT_POLL) {
            Ok(Task::SignIn(number)) => match sign_in(&client, &number) {
                Ok(outcome) => report(&handle, outcome),
                Err(reason) => report(&handle, Outcome::Failed(reason)),
            },
            Ok(Task::LoadExits) => refresh(&polling, &handle),
            Ok(Task::PickExit(id)) => match pick_exit(&client, id) {
                Ok(outcome) => report(&handle, outcome),
                Err(reason) => report(&handle, Outcome::Failed(reason)),
            },
            Ok(Task::SignOut) => {
                // The tunnel is already coming down on its own thread; this is
                // only the revocation.
                // Renewed first if it has lapsed: signing out is what revokes
                // the device, and failing it silently would leave a dead device
                // holding one of the account's six slots.
                let _ = authenticated(&client, |token| client.sign_out(token));
                let _ = store::clear();
                report(&handle, Outcome::SignedOut);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => refresh(&polling, &handle),
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn refresh(client: &api::Client, handle: &Weak<App>) {
    let Some(session) = store::load() else {
        return;
    };
    if let Ok(list) = client.exits(&session.token) {
        report(handle, Outcome::Exits(list));
    }
    // The one authenticated call on the poll, so it is also what notices a
    // lapsed token and renews it before the user asks for anything.
    if let Ok(account) = authenticated(client, |token| client.account(token)) {
        report(handle, Outcome::Account(account));
    }
    // Without this the entry node stayed blank until the next exit was picked,
    // so a restarted client showed no entry at all.
    if let Ok(relay) = first_relay(client) {
        report(handle, Outcome::Relay(relay));
    }
}

/// Runs a call that needs a token, renewing the session once if it has lapsed.
///
/// Tokens last a day and there is no dedicated refresh route: signing in again
/// with the key **already registered** returns the same device, a fresh token,
/// and consumes no device slot. Generating a new key here would register a
/// device on every renewal instead, and fill the account's six slots — which is
/// how this account reached five.
fn authenticated<T, F>(client: &api::Client, call: F) -> Result<T, api::Error>
where
    F: Fn(&str) -> Result<T, api::Error>,
{
    let Some(session) = store::load() else {
        return Err(api::Error::Unauthorised);
    };

    match call(&session.token) {
        Err(api::Error::Unauthorised) => {}
        settled => return settled,
    }

    let granted = client.sign_in(&session.account_number, &session.public_key)?;

    // The tunnel encrypts with the private key on disk. A renewal that came back
    // as a different device would leave us using a key the relay has never seen:
    // a tunnel that connects, counts bytes, and carries nothing.
    if granted.device.public_key != session.public_key {
        return Err(api::Error::Refused(
            "The service returned a different device on renewal.".into(),
        ));
    }

    let renewed = store::Session {
        token: granted.token,
        device_id: granted.device.id,
        device_name: granted.device.name,
        tunnel_ip: granted.device.tunnel_ip,
        tunnel_ip6: granted.device.tunnel_ip6,
        ..session
    };
    let _ = store::save(&renewed);

    call(&renewed.token)
}

fn sign_in(client: &api::Client, number: &str) -> Result<Outcome, String> {
    let pair = keys::generate()?;
    let granted = client
        .sign_in(number, &pair.public)
        .map_err(|e| e.to_string())?;

    // Signing in with a key the account already carries returns that device
    // untouched, and our fresh private key would not match it.
    if granted.device.public_key != pair.public {
        return Err(
            "This device already exists under a different key. Revoke it and try again.".to_string(),
        );
    }

    let session = store::Session {
        account_number: number.to_string(),
        token: granted.token,
        private_key: pair.private,
        public_key: pair.public,
        device_id: granted.device.id,
        device_name: granted.device.name.clone(),
        tunnel_ip: granted.device.tunnel_ip.clone(),
        tunnel_ip6: granted.device.tunnel_ip6.clone(),
        exit_id: None,
    };

    let account = client.account(&session.token).map_err(|e| e.to_string())?;
    let relay = first_relay(client)?;

    store::save(&session)?;
    Ok(Outcome::SignedIn(Box::new(session), account, relay))
}

fn first_relay(client: &api::Client) -> Result<api::Relay, String> {
    client
        .relays()
        .map_err(|e| e.to_string())?
        .into_iter()
        .next()
        .ok_or_else(|| "No entry node available.".to_string())
}

fn pick_exit(client: &api::Client, id: i64) -> Result<Outcome, String> {
    authenticated(client, |token| client.choose_exit(token, Some(id)))
        .map_err(|e| e.to_string())?;

    // Reloaded after the call, not before: a lapsed session was renewed in
    // place, and the tunnel addresses came back with it.
    let mut session = store::load().ok_or("No session. Sign in again.")?;

    let relay = first_relay(client)?;
    let profile = tunnel::Profile {
        private_key: session.private_key.clone(),
        addresses: vec![session.tunnel_ip.clone(), session.tunnel_ip6.clone()],
        dns: vec!["10.64.0.1".into(), "fda8:75e8:355::1".into()],
        peer_public_key: relay.public_key.clone(),
        endpoint: format!("{}:{}", relay.endpoint, relay.port),
    };
    tunnel::up(&profile)?;

    session.exit_id = Some(id);
    store::save(&session)?;

    Ok(Outcome::ExitPicked(id))
}

/// Brings the tunnel down at once, on a thread of its own.
///
/// This used to be a task like any other, handled by the single worker that
/// also makes the requests. So a disconnect asked for while the ten-second
/// refresh was mid-flight waited for it — and a tunnel that has stopped
/// passing traffic is exactly the case where those requests run to their full
/// timeout. The moment the tunnel most needed to go was the moment it took
/// longest. Nothing here touches the network, so nothing here should ever
/// queue behind something that does.
fn tear_down(handle: Weak<App>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static COMING_DOWN: AtomicBool = AtomicBool::new(false);

    // Two teardowns at once would each ask the service manager to remove the
    // same tunnel, and the second would report the first one's work as a
    // failure.
    if COMING_DOWN.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let failure = tunnel::down().err();
        COMING_DOWN.store(false, Ordering::SeqCst);
        // Silence on success: the screen already says the tunnel is down, and
        // saying it again would only redraw what is there. A failure has to be
        // heard.
        if let Some(reason) = failure {
            report(&handle, Outcome::Failed(reason));
        }
    });
}

fn report(handle: &Weak<App>, outcome: Outcome) {
    let _ = handle.upgrade_in_event_loop(move |app| {
        if outcome.concludes_request() {
            app.set_busy(false);
            app.set_pending_exit(-1);
        }
        match outcome {
            Outcome::SignedIn(session, account, relay) => {
                apply_session(&app, &session);
                apply_account(&app, &account);
                apply_relay(&app, &relay);
                clear_notice(&app);
                show_chooser(&app, false);
            }
            Outcome::Exits(list) => {
                EXIT_CACHE.with(|cache| *cache.borrow_mut() = list);
                render_exits(&app);
            }
            Outcome::Relay(relay) => apply_relay(&app, &relay),
            Outcome::ExitPicked(id) => {
                app.set_chosen_exit(id as i32);
                render_exits(&app);
                // The poll may not have carried this exit yet; the row the user
                // just clicked is still in the model, so read it from there.
                if app.get_exit_label().is_empty() {
                    label_from_model(&app, id);
                }
                clear_notice(&app);
                app.set_screen(Screen::Connected);
            }
            Outcome::Account(account) => apply_account(&app, &account),
            Outcome::SignedOut => {
                EXIT_CACHE.with(|cache| cache.borrow_mut().clear());
                app.set_screen(Screen::SignIn);
                app.set_account_number(SharedString::new());
                app.set_filter(SharedString::new());
                app.set_exits(ModelRc::new(VecModel::from(Vec::<ExitEntry>::new())));
                app.set_total_exits(0);
                app.set_chosen_exit(-1);
                app.set_exit_label(SharedString::new());
                app.set_exit_state(SharedString::new());
                app.set_can_close_chooser(false);
                app.set_map_nodes(ModelRc::new(VecModel::from(Vec::<MapNode>::new())));
                app.set_map_link(ModelRc::new(VecModel::from(Vec::<MapDot>::new())));
                app.set_unplaced(0);
                app.set_has_relay(false);
                clear_notice(&app);
            }
            Outcome::Failed(reason) => notify(&app, &reason, true),
        }
    });
}

fn show_chooser(app: &App, closable: bool) {
    app.set_can_close_chooser(closable);
    app.set_screen(Screen::Choosing);
}

fn notify(app: &App, text: &str, danger: bool) {
    // An advisory and a service failure do not deserve the same colour.
    app.set_notice_danger(danger);
    app.set_notice(text.into());
}

fn clear_notice(app: &App) {
    app.set_notice(SharedString::new());
}

fn apply_session(app: &App, session: &store::Session) {
    app.set_device_name(session.device_name.clone().into());
    app.set_tunnel_address(session.tunnel_ip.clone().into());
    app.set_chosen_exit(session.exit_id.unwrap_or(-1) as i32);
}

fn apply_account(app: &App, account: &api::Account) {
    let day = account.expires_at.split('T').next().unwrap_or("");
    let label = if account.active {
        format!("active until {day}")
    } else {
        format!("expired on {day}")
    };
    app.set_account_expiry(
        format!(
            "{label} · {}/{} devices",
            account.devices, account.max_devices
        )
        .into(),
    );
}

/// Filters, sorts and publishes the exit list and the map from whatever the last
/// poll brought back. Called both when the service answers and on every
/// keystroke in the filter, so the map always shows the same set as the list.
fn render_exits(app: &App) {
    let needle = geo::fold(&app.get_filter());
    let needle = needle.trim();

    EXIT_CACHE.with(|cache| {
        let list = cache.borrow();
        app.set_total_exits(list.len() as i32);

        // The kind tabs count what the text filter left, so switching tabs
        // never claims nodes the search has already excluded.
        let searched: Vec<&api::Exit> = list
            .iter()
            .filter(|exit| matches_filter(exit, needle))
            .collect();
        app.set_count_all(searched.len() as i32);
        app.set_count_residential(
            searched.iter().filter(|e| e.kind() == api::KIND_RESIDENTIAL).count() as i32,
        );
        app.set_count_datacentre(
            searched.iter().filter(|e| e.kind() == api::KIND_DATACENTRE).count() as i32,
        );

        let wanted = app.get_kind_filter();
        let matching: Vec<&api::Exit> = searched
            .into_iter()
            .filter(|exit| wanted < 0 || exit.kind() == wanted)
            .collect();

        // Picking a tab narrows the list as deliberately as typing does, so it
        // opens the countries it left standing rather than handing back a
        // shorter row of shut folders.
        let entries = group_by_country(app, &matching, !needle.is_empty() || wanted >= 0);

        // Replacing the model rebuilds every row, dropping hover and scroll and
        // letting the sort reorder lines under the pointer. The poll runs every
        // ten seconds, so only publish when something actually moved.
        if !same_entries(&app.get_exits(), &entries) {
            app.set_exits(ModelRc::new(VecModel::from(entries)));
        }

        app.set_shown_exits(matching.len() as i32);
        render_map(app, &matching);
        refresh_exit_labels(app, &list);
    });
}

/// Lays the matching exits out as countries, each folding open onto its nodes.
///
/// The result is one flat list because that is what a `ListView` takes: a header
/// row per country, followed by its nodes when it is open.
fn group_by_country(app: &App, matching: &[&api::Exit], filtering: bool) -> Vec<ExitEntry> {
    let chosen = app.get_chosen_exit() as i64;

    let mut countries: Vec<(String, Vec<&api::Exit>)> = Vec::new();
    for exit in matching {
        let code = country_key(exit);
        match countries.iter_mut().find(|(at, _)| *at == code) {
            Some((_, members)) => members.push(exit),
            None => countries.push((code, vec![exit])),
        }
    }

    // By the name on screen, not the code behind it: "Allemagne" belongs under
    // A, however "de" sorts. Folded, so accented names land where a reader
    // looks for them rather than past Z.
    countries.sort_by_cached_key(|(code, members)| {
        geo::fold(&country_label(code, &members[0].country))
    });

    let mut rows = Vec::new();
    app.set_country_count(countries.len() as i32);

    for (code, members) in &countries {
        let holds_chosen = members.iter().any(|exit| exit.id == chosen);
        // A filter opens everything it matched; otherwise only the country in
        // service is open, unless the user said otherwise.
        let open = filtering
            || FOLDED.with(|folded| folded.borrow().get(code).copied()).unwrap_or(holds_chosen);

        rows.push(ExitEntry {
            header: true,
            collapsed: !open,
            id: -1,
            code: code.clone().into(),
            label: country_label(code, &members[0].country).into(),
            moniker: SharedString::new(),
            ready: members.iter().any(|exit| is_ready(exit)),
            // A country's row shows a kind only when every node under it agrees.
            kind: members
                .iter()
                .map(|exit| exit.kind())
                .reduce(|a, b| if a == b { a } else { api::KIND_UNKNOWN })
                .unwrap_or(api::KIND_UNKNOWN),
            asn: SharedString::new(),
            count: members.len() as i32,
            ready_count: members.iter().filter(|exit| is_ready(exit)).count() as i32,
            flag: flags::flag(code),
            has_flag: flags::known(code),
        });

        if !open {
            continue;
        }

        let mut nodes: Vec<&api::Exit> = members.clone();
        // Whatever is already running comes first: picking it costs nothing.
        nodes.sort_by(|a, b| {
            is_ready(b)
                .cmp(&is_ready(a))
                .then(a.city.cmp(&b.city))
                .then(a.moniker.cmp(&b.moniker))
        });
        rows.extend(nodes.into_iter().map(to_entry));
    }

    rows
}


/// Where a node belongs on the map. Coordinates from the service win; the table
/// in `geo.rs` only covers for a service that does not send any.
fn spot_for(exit: &api::Exit) -> Option<geo::Spot> {
    exit.coords
        .pair()
        .map(|(lat, lon)| geo::project(lat, lon))
        .or_else(|| geo::locate(&country_key(exit), &exit.city))
}

fn relay_spot(relay: &api::Relay) -> Option<geo::Spot> {
    relay
        .coords
        .pair()
        .map(|(lat, lon)| geo::project(lat, lon))
        .or_else(|| geo::locate(&relay.country, &relay.city))
}

fn apply_relay(app: &App, relay: &api::Relay) {
    // Same shape as the exit label: city first, then the country spelled out.
    let code = geo::resolve_code(&relay.country, &relay.country).unwrap_or_default();
    app.set_relay_label(
        format!("{} · {}", relay.city, country_label(&code, &relay.country.to_uppercase())).into(),
    );
    match relay_spot(relay) {
        Some(spot) => {
            app.set_relay_nx(spot.nx);
            app.set_relay_ny(spot.ny);
            app.set_has_relay(true);
        }
        None => app.set_has_relay(false),
    }
    render_link(app);
}

/// How far past the visible window the land is drawn. The margin is what a
/// drag moves into, so nothing is ever revealed blank; it is clamped to the
/// world, which is why a whole-world view costs exactly what it always did.
const MAP_OVERSCAN: f32 = 1.7;
/// A texture is kept while it still carries this share of the detail the
/// current view would ask for. Below it the map would start to look soft.
const MAP_MIN_DETAIL: f32 = 0.62;
/// Drawn at this while a gesture is running, and again at `MAP_SUPERSAMPLE`
/// once it stops. A refill in the middle of a drag has to be quick; a picture
/// that is going to sit there has to be clean.
const MAP_SUPERSAMPLE_QUICK: f32 = 1.0;
/// Ceiling on a single texture, so a very large window cannot ask for an
/// allocation to match.
const MAP_MAX_PIXELS: f32 = 9.0e6;

/// What the land texture currently on screen covers, and how finely.
struct MapTexture {
    view: worldmap::View,
    /// Pixels per unit of world width.
    detail: f32,
    /// False while it is the quick version drawn mid-gesture.
    clean: bool,
}

thread_local! {
    static MAP_TEXTURE: RefCell<Option<MapTexture>> = const { RefCell::new(None) };
}

/// Draws the land again only if the texture in hand cannot serve `view`.
///
/// `finishing` marks the call that comes once the view has stopped moving: it
/// redraws a quick texture at full quality even when the quick one still
/// covers the window.
#[allow(clippy::too_many_arguments)]
fn refresh_map_texture(
    app: &App,
    vx: f32,
    vy: f32,
    vw: f32,
    vh: f32,
    pane_w: f32,
    pane_h: f32,
    finishing: bool,
) {
    if vw <= 0.0 || vh <= 0.0 || pane_w <= 0.0 || pane_h <= 0.0 {
        return;
    }

    let display = app.window().scale_factor().max(1.0);
    let supersample = if finishing {
        MAP_SUPERSAMPLE
    } else {
        MAP_SUPERSAMPLE_QUICK
    };
    let wanted_detail = pane_w * display * supersample / vw;

    let view = worldmap::View {
        x: vx,
        y: vy,
        w: vw,
        h: vh,
    };

    let covered = MAP_TEXTURE.with(|cache| {
        cache.borrow().as_ref().is_some_and(|texture| {
            // A quick texture is never good enough to end on, however well it
            // covers the window.
            texture.view.contains(&view)
                && texture.detail >= wanted_detail * MAP_MIN_DETAIL
                && (texture.clean || !finishing)
        })
    });
    if covered {
        return;
    }

    let region = worldmap::overscan(view, MAP_OVERSCAN);
    let grown = region.w / vw;
    let mut width = pane_w * display * supersample * grown;
    let mut height = pane_h * display * supersample * grown;
    let pixels = width * height;
    if pixels > MAP_MAX_PIXELS {
        let back = (MAP_MAX_PIXELS / pixels).sqrt();
        width *= back;
        height *= back;
    }
    let (width, height) = (width as u32, height as u32);
    if width == 0 || height == 0 {
        return;
    }

    app.set_map_image(worldmap::render(region, width, height, MAP_LAND, MAP_BORDER));
    app.set_map_image_view_x(region.x);
    app.set_map_image_view_y(region.y);
    app.set_map_image_view_w(region.w);
    app.set_map_image_view_h(region.h);

    MAP_TEXTURE.with(|cache| {
        *cache.borrow_mut() = Some(MapTexture {
            view: region,
            detail: width as f32 / region.w,
            clean: finishing,
        });
    });
}

/// Re-collapses the markers for the current scale, and nothing else. Which
/// cities merge into one marker depends on the zoom, so a scale change has to
/// redo this — but it has no bearing on the list beside it.
fn regroup_map(app: &App) {
    EXIT_CACHE.with(|cache| {
        let list = cache.borrow();
        let needle = geo::fold(&app.get_filter());
        let needle = needle.trim();
        let wanted = app.get_kind_filter();
        let matching: Vec<&api::Exit> = list
            .iter()
            .filter(|exit| matches_filter(exit, needle))
            .filter(|exit| wanted < 0 || exit.kind() == wanted)
            .collect();
        render_map(app, &matching);
    });
}

/// Collapses the matching exits onto map nodes. Several exits in one city land
/// on the same spot, so they become one node carrying the count; clicking it
/// picks a ready one when there is a choice.
fn render_map(app: &App, matching: &[&api::Exit]) {
    // Two exits are the same node when they sit within a few tenths of a degree
    // of each other, which also groups everything that fell back to a country
    // centroid. Zooming in shrinks that distance, so neighbouring cities split
    // back apart once the view is close enough to show them separately.
    const SAME: f32 = 0.003;
    let same = SAME / MAP_ZOOM.with(|zoom| zoom.get());

    let mut groups: Vec<(geo::Spot, Vec<&api::Exit>)> = Vec::new();
    let mut unplaced = 0;

    for exit in matching {
        let Some(spot) = spot_for(exit) else {
            unplaced += 1;
            continue;
        };
        match groups
            .iter_mut()
            .find(|(at, _)| (at.nx - spot.nx).abs() < same && (at.ny - spot.ny).abs() < same)
        {
            Some((_, members)) => members.push(exit),
            None => groups.push((spot, vec![exit])),
        }
    }

    let nodes: Vec<MapNode> = groups
        .iter()
        .map(|(spot, members)| {
            let lead = members
                .iter()
                .find(|exit| is_ready(exit))
                .unwrap_or(&members[0]);
            MapNode {
                id: lead.id as i32,
                label: format!("{} · {}", lead.country.to_uppercase(), lead.city).into(),
                detail: if members.len() > 1 {
                    format!("{} sorties ici", members.len()).into()
                } else {
                    lead.moniker.clone().into()
                },
                nx: spot.nx,
                ny: spot.ny,
                ready: members.iter().any(|exit| is_ready(exit)),
                kind: lead.kind(),
                count: members.len() as i32,
            }
        })
        .collect();

    app.set_unplaced(unplaced);
    if !same_nodes(&app.get_map_nodes(), &nodes) {
        app.set_map_nodes(ModelRc::new(VecModel::from(nodes)));
    }
    render_link(app);
}

/// The dotted trail from the entry node to the active exit. Interpolated here
/// so the spacing stays even whatever size the pane ends up.
fn render_link(app: &App) {
    const DOTS: usize = 22;

    let chosen = app.get_chosen_exit() as i64;
    let target = EXIT_CACHE.with(|cache| {
        cache
            .borrow()
            .iter()
            .find(|exit| exit.id == chosen)
            .and_then(spot_for)
    });

    let trail = match (app.get_has_relay(), target) {
        (true, Some(to)) => {
            let from = geo::Spot {
                nx: app.get_relay_nx(),
                ny: app.get_relay_ny(),
            };
            // Skips both ends so the trail does not run under the markers.
            (1..DOTS)
                .map(|step| {
                    let t = step as f32 / DOTS as f32;
                    MapDot {
                        nx: from.nx + (to.nx - from.nx) * t,
                        ny: from.ny + (to.ny - from.ny) * t,
                    }
                })
                .collect()
        }
        _ => Vec::new(),
    };

    app.set_map_link(ModelRc::new(VecModel::from(trail)));
}

fn is_ready(exit: &api::Exit) -> bool {
    exit.state == "provisioned"
}

fn same_nodes(model: &ModelRc<MapNode>, nodes: &[MapNode]) -> bool {
    model.row_count() == nodes.len()
        && nodes
            .iter()
            .enumerate()
            .all(|(index, node)| model.row_data(index).as_ref() == Some(node))
}

/// Which country a record belongs to, as a lowercase ISO-2 code. Records the
/// service left without a code fall back to their name; one it cannot resolve at
/// all falls back to the folded name, so two unknown countries stay apart
/// instead of collapsing into one heap.
fn country_key(exit: &api::Exit) -> String {
    geo::resolve_code(&exit.country_code, &exit.country)
        .unwrap_or_else(|| geo::fold(&exit.country))
}

/// The flag set's name for the country, falling back to whatever the service
/// called it — which is already an English name, just occasionally a different
/// spelling of the same country.
fn country_label(code: &str, sent: &str) -> String {
    geo::english_name(code)
        .map(str::to_string)
        .unwrap_or_else(|| sent.to_string())
}

fn matches_filter(exit: &api::Exit, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let code = country_key(exit);
    // Folded on both sides, so "suede" finds Suède, and against the French name
    // too, which is the one on screen.
    [
        exit.country.as_str(),
        exit.city.as_str(),
        exit.moniker.as_str(),
        code.as_str(),
        &country_label(&code, &exit.country),
    ]
    .iter()
    .any(|field| geo::fold(field).contains(needle))
}

/// One node, under the country that already names it.
fn to_entry(exit: &api::Exit) -> ExitEntry {
    ExitEntry {
        header: false,
        collapsed: false,
        id: exit.id as i32,
        code: country_key(exit).into(),
        label: exit.city.clone().into(),
        moniker: exit.moniker.clone().into(),
        ready: is_ready(exit),
        kind: exit.kind(),
        asn: exit.asn.clone().unwrap_or_default().into(),
        count: 0,
        ready_count: 0,
        flag: slint::Image::default(),
        has_flag: false,
    }
}

fn same_entries(model: &ModelRc<ExitEntry>, entries: &[ExitEntry]) -> bool {
    model.row_count() == entries.len()
        && entries
            .iter()
            .enumerate()
            .all(|(index, entry)| model.row_data(index).as_ref() == Some(entry))
}

fn refresh_exit_labels(app: &App, list: &[api::Exit]) {
    let chosen = app.get_chosen_exit() as i64;
    if chosen < 0 {
        app.set_exit_label(SharedString::new());
        app.set_exit_state(SharedString::new());
        app.set_chosen_country(SharedString::new());
        return;
    }
    // Not finding it means the list has not arrived yet, not that the exit is
    // gone: leave the screen as it was rather than blanking it.
    if let Some(exit) = list.iter().find(|exit| exit.id == chosen) {
        let code = country_key(exit);
        app.set_exit_label(
            format!("{} · {}", exit.city, country_label(&code, &exit.country)).into(),
        );
        app.set_exit_state(waiting_notice(is_ready(exit)));
        app.set_chosen_country(code.into());
    }
}

fn label_from_model(app: &App, id: i64) {
    let model = app.get_exits();
    for index in 0..model.row_count() {
        let Some(entry) = model.row_data(index) else {
            continue;
        };
        if !entry.header && entry.id as i64 == id {
            let code = entry.code.to_string();
            app.set_exit_label(
                format!("{} · {}", entry.label, country_label(&code, &code.to_uppercase())).into(),
            );
            app.set_exit_state(waiting_notice(entry.ready));
            app.set_chosen_country(entry.code.clone());
            return;
        }
    }
}

fn waiting_notice(ready: bool) -> SharedString {
    if ready {
        SharedString::new()
    } else {
        "Exit still being activated; traffic is leaving by the entry node for now.".into()
    }
}

/// Keeps the connected screen honest about what the tunnel is really doing.
///
/// This runs on its own thread, not on a Slint timer: on Windows every call
/// below spawns a process, and doing that on the interface thread stalled the
/// render loop twice every tick.
fn watch_tunnel(handle: Weak<App>) {
    std::thread::spawn(move || {
        let mut first = true;
        loop {
            let elevated = if first { Some(tunnel::elevated()) } else { None };
            // One probe, not two: asking for the mode already tells us whether
            // anything is up, and each question costs a process on Windows.
            let mode = tunnel::mode();
            let stats = if mode.is_some() {
                tunnel::stats()
            } else {
                tunnel::Stats::default()
            };

            let posted = handle.upgrade_in_event_loop(move |app| {
                apply_tunnel(&app, mode, stats);

                // The opening screen is settled here, on the first probe, and
                // never again — the user is free to walk back to the chooser
                // with a tunnel up, and nothing should drag them out of it.
                //
                // It used to be settled from the stored session instead: an
                // exit was remembered, therefore the client opened on the
                // connected screen offering to close a tunnel. But a stored
                // exit only records which one was picked last. The tunnel does
                // not survive: the client takes it down on its way out. So a
                // fresh start showed a connection that was not there.
                if first && app.get_screen() != Screen::SignIn {
                    if mode.is_some() {
                        app.set_screen(Screen::Connected);
                    } else {
                        show_chooser(&app, false);
                    }
                }
                // A hint, not a failure, and it must not bury a real error.
                if elevated == Some(false) && app.get_notice().is_empty() {
                    notify(&app, elevation_hint(), false);
                }
            });
            if posted.is_err() {
                return; // The event loop is gone: the window closed.
            }

            first = false;
            std::thread::sleep(TUNNEL_POLL);
        }
    });
}

fn apply_tunnel(app: &App, mode: Option<tunnel::Mode>, stats: tunnel::Stats) {
    let Some(mode) = mode else {
        app.set_tunnel_state("down".into());
        app.set_tunnel_status_text("TUNNEL DOWN".into());
        app.set_tunnel_kind(SharedString::new());
        tray_tooltip(app, "ValiraVPN — tunnel down");
        // Leaving the last counters up made a closed tunnel look like a live
        // one for as long as the window stayed open.
        app.set_traffic_in(SharedString::new());
        app.set_traffic_out(SharedString::new());
        app.set_handshake(SharedString::new());
        return;
    };

    // Which backend carries the traffic, said outright rather than left to be
    // discovered: the kernel path is the faster of the two.
    app.set_tunnel_kind(
        match mode {
            tunnel::Mode::System => "WireGuard service · kernel path",
            tunnel::Mode::Embedded => "Embedded · user-space path",
        }
        .into(),
    );

    let provisioning = !app.get_exit_state().is_empty();
    app.set_tunnel_state(if provisioning { "pending" } else { "up" }.into());
    app.set_tunnel_status_text(
        if provisioning {
            "ACTIVATING"
        } else {
            "TUNNEL UP"
        }
        .into(),
    );
    app.set_traffic_in(human(stats.received).into());
    app.set_traffic_out(human(stats.sent).into());
    app.set_handshake(
        match stats.handshake_age {
            Some(age) => format!("{age} s ago"),
            None => "never".to_string(),
        }
        .into(),
    );

    let exit = app.get_exit_label();
    tray_tooltip(
        app,
        &if exit.is_empty() {
            format!("ValiraVPN — {}", if provisioning { "activating" } else { "connected" })
        } else if provisioning {
            format!("ValiraVPN — activating {exit}")
        } else {
            format!("ValiraVPN — {exit}")
        },
    );
}

/// What the tray icon says on hover. With the window hidden it is the only
/// thing reporting the tunnel, so it carries the exit rather than just a state.
fn tray_tooltip(app: &App, text: &str) {
    #[cfg(windows)]
    win32_frame::set_tray_tooltip(app, text);
    #[cfg(not(windows))]
    {
        let _ = (app, text);
    }
}

fn human(count: u64) -> String {
    const UNITS: [&str; 4] = ["o", "Ko", "Mo", "Go"];
    let mut value = count as f64;
    for unit in UNITS {
        if value < 1024.0 || unit == "Go" {
            return if unit == "o" {
                format!("{value:.0} {unit}")
            } else {
                format!("{value:.1} {unit}")
            };
        }
        value /= 1024.0;
    }
    format!("{value:.1} Go")
}
