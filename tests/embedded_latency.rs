//! Where the embedded tunnel's latency comes from, measured on the real path.
//!
//! Brings the embedded tunnel up against the account's own relay, pings through
//! it, and takes it down again. Every stage is timed by the pump's own trace, so
//! the round trip the pump sees can be set beside the one `ping` reports: if the
//! pump sees thirty milliseconds where `ping` sees two thousand, the time is
//! being lost between the operating system and the tunnel interface, on either
//! side of us, and no amount of staring at this code would have found it.
//!
//! Ignored by default. It reroutes this machine's traffic for as long as it
//! runs, so it is never part of an ordinary `cargo test`.
//!
//! The tunnel comes down through a guard that runs on the way out however the
//! way out comes about — a failed assertion, a panic in a worker, anything. A
//! test that left the routing table pointing at a tunnel it had abandoned would
//! be worse than no test.
//!
//! Needs administrator and a signed-in session.
//! `cargo test --release --test embedded_latency -- --ignored --nocapture`

use std::process::Command;
use std::time::Duration;
use valira_desktop::{api, store, tunnel};

const PROBE: &str = "1.1.1.1";
const API: &str = "https://valiravpn.com";

/// Takes the tunnel down when it goes out of scope, whatever the reason.
struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        match tunnel::down() {
            Ok(()) => println!("\ntunnel referme"),
            Err(reason) => println!("\nECHEC DE LA FERMETURE : {reason}"),
        }
    }
}

/// Runs `ping` and returns its round trips in milliseconds, with its own words.
fn ping(count: u32) -> (Vec<f64>, String) {
    let output = Command::new("ping")
        .args(["-n", &count.to_string(), "-w", "3000", PROBE])
        .output()
        .expect("running ping");
    let text = String::from_utf8_lossy(&output.stdout).to_string();

    let mut times = Vec::new();
    for line in text.lines() {
        // `temps=31 ms` or `time=31ms`, depending on which Windows this is.
        let Some(at) = line.find("emps").or_else(|| line.find("ime")) else {
            continue;
        };
        let digits: String = line[at..]
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(value) = digits.parse::<f64>() {
            times.push(value);
        }
    }
    (times, text)
}

fn summarise(what: &str, times: &[f64]) {
    if times.is_empty() {
        println!("{what:<28} aucune reponse");
        return;
    }
    let total: f64 = times.iter().sum();
    let worst = times.iter().cloned().fold(0.0_f64, f64::max);
    let best = times.iter().cloned().fold(f64::MAX, f64::min);
    println!(
        "{what:<28} {} reponses   moyenne {:>7.1} ms   min {:>6.1}   max {:>7.1}",
        times.len(),
        total / times.len() as f64,
        best,
        worst
    );
}

#[test]
#[ignore = "reroutes this machine's traffic: run deliberately"]
fn where_the_embedded_tunnel_loses_its_time() {
    // Read once, on first use, by the pump and by the trace. Set before either
    // has had the chance.
    unsafe {
        std::env::set_var("VALIRA_TUNNEL", "embedded");
        std::env::set_var("VALIRA_TUNNEL_TRACE", "1");
    }

    let session = store::load().expect("no stored session: sign in first");
    let client = api::Client::new(API).expect("api client");
    let relay = client
        .relays()
        .expect("fetching the relays")
        .into_iter()
        .next()
        .expect("the service returned no relay");

    println!("relais : {} / {}  {}:{}", relay.country, relay.city, relay.endpoint, relay.port);

    // What the machine does with no tunnel at all, for the comparison that
    // gives every other number its meaning.
    let (before, _) = ping(5);
    summarise("sans tunnel", &before);

    let profile = tunnel::Profile {
        private_key: session.private_key.clone(),
        addresses: vec![session.tunnel_ip.clone(), session.tunnel_ip6.clone()],
        dns: vec!["10.64.0.1".into(), "fda8:75e8:355::1".into()],
        peer_public_key: relay.public_key.clone(),
        endpoint: format!("{}:{}", relay.endpoint, relay.port),
    };

    let trace_log = std::env::temp_dir().join("valira-tunnel-trace.log");
    let _ = std::fs::remove_file(&trace_log);

    tunnel::up(&profile).expect("bringing the embedded tunnel up");
    let _restore = Restore;

    // The handshake has to finish before the first packet means anything.
    std::thread::sleep(Duration::from_secs(3));

    let (through, text) = ping(20);
    summarise("a travers le tunnel", &through);
    println!("\n--- ce que ping a dit ---");
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        println!("{line}");
    }

    // Let the reporter write one last summary before the tunnel goes.
    std::thread::sleep(Duration::from_secs(3));
    drop(_restore);

    println!("\n--- ce que la pompe a vu ---");
    match std::fs::read_to_string(&trace_log) {
        Ok(log) => println!("{log}"),
        Err(error) => println!("journal illisible : {error}"),
    }

    // The machine has to be exactly as it was found.
    let (after, _) = ping(5);
    summarise("apres fermeture", &after);
    assert!(
        !after.is_empty(),
        "the machine did not get its connectivity back"
    );
}
