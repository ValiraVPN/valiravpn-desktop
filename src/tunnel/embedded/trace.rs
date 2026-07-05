//! Where the time goes, when a packet takes far longer than it should.
//!
//! Off unless `VALIRA_TUNNEL_TRACE` is set in the environment; when off, every
//! call here is a relaxed atomic read and a return. It writes to a file rather
//! than to the console because the release build has no console.
//!
//! What it measures is the one thing that separates a fault in this pump from a
//! fault below it. Each ICMP echo request leaving the interface is noted by its
//! identifier and sequence; when the matching reply comes back out of the
//! tunnel, the round trip *as the pump saw it* is recorded. Compare that with
//! what `ping` reports:
//!
//!   * both large — the delay is on the wire or in the relay;
//!   * pump small, `ping` large — the delay is between the operating system and
//!     the tunnel interface, on either side of us, and nothing in this file's
//!     own timings will show it;
//!   * time spent inside our own code large — the fault is here.

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const OUTBOUND: usize = 0;
pub const INBOUND: usize = 1;

pub fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VALIRA_TUNNEL_TRACE").is_some())
}

#[derive(Default)]
struct Tally {
    packets: u64,
    inside: Duration,
    worst_inside: Duration,
}

#[derive(Default)]
struct State {
    stages: [Tally; 2],
    /// Echo requests that have left, by identifier and sequence.
    outstanding: HashMap<(u16, u16), Instant>,
    trips: Vec<Duration>,
    lost: u64,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(State::default()))
}

/// Start of a stage, or `None` when tracing is off.
pub fn mark() -> Option<Instant> {
    on().then(Instant::now)
}

pub fn spent(stage: usize, since: Option<Instant>) {
    let Some(since) = since else { return };
    let taken = since.elapsed();
    let mut state = state().lock().unwrap_or_else(|p| p.into_inner());
    let tally = &mut state.stages[stage];
    tally.packets += 1;
    tally.inside += taken;
    tally.worst_inside = tally.worst_inside.max(taken);
}

/// An IP packet on its way out of the interface.
pub fn leaving(packet: &[u8]) {
    if !on() {
        return;
    }
    // Type 8 is an echo request.
    let Some((kind, id, seq)) = echo(packet) else {
        return;
    };
    if kind != 8 {
        return;
    }
    let mut state = state().lock().unwrap_or_else(|p| p.into_inner());
    // Bounded, so a long run with no replies cannot grow without limit.
    if state.outstanding.len() > 4096 {
        state.outstanding.clear();
    }
    state.outstanding.insert((id, seq), Instant::now());
}

/// An IP packet coming back out of the tunnel, on its way to the interface.
pub fn returning(packet: &[u8]) {
    if !on() {
        return;
    }
    // Type 0 is an echo reply.
    let Some((kind, id, seq)) = echo(packet) else {
        return;
    };
    if kind != 0 {
        return;
    }
    let mut state = state().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(sent) = state.outstanding.remove(&(id, seq)) {
        let trip = sent.elapsed();
        state.trips.push(trip);
    } else {
        state.lost += 1;
    }
}

/// `(type, identifier, sequence)` for an IPv4 ICMP echo, or nothing.
fn echo(packet: &[u8]) -> Option<(u8, u16, u16)> {
    if packet.len() < 20 || packet[0] >> 4 != 4 {
        return None;
    }
    // Protocol 1 is ICMP.
    if packet[9] != 1 {
        return None;
    }
    let header = ((packet[0] & 0x0F) as usize) * 4;
    if packet.len() < header + 8 {
        return None;
    }
    let kind = packet[header];
    let id = u16::from_be_bytes([packet[header + 4], packet[header + 5]]);
    let seq = u16::from_be_bytes([packet[header + 6], packet[header + 7]]);
    Some((kind, id, seq))
}

/// Writes a summary every two seconds for as long as the tunnel is up.
pub fn reporting(stop: Arc<AtomicBool>) -> Option<JoinHandle<()>> {
    if !on() {
        return None;
    }
    let path = std::env::temp_dir().join("valira-tunnel-trace.log");
    Some(std::thread::spawn(move || {
        let started = Instant::now();
        let mut file = match std::fs::File::create(&path) {
            Ok(file) => file,
            Err(_) => return,
        };
        let _ = writeln!(
            file,
            "temps  | sortants        | entrants        | aller-retour vu par la pompe"
        );
        let _ = writeln!(
            file,
            "       | nb   moy   pire | nb   moy   pire | nb   moy      pire     sans reponse"
        );
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_secs(2));
            let mut state = state().lock().unwrap_or_else(|p| p.into_inner());
            let ms = |d: Duration| d.as_secs_f64() * 1000.0;
            let mean = |t: &Tally| {
                if t.packets == 0 {
                    0.0
                } else {
                    ms(t.inside) / t.packets as f64
                }
            };
            let trips = std::mem::take(&mut state.trips);
            let (trip_mean, trip_worst) = if trips.is_empty() {
                (0.0, 0.0)
            } else {
                (
                    trips.iter().map(|d| ms(*d)).sum::<f64>() / trips.len() as f64,
                    trips.iter().map(|d| ms(*d)).fold(0.0_f64, f64::max),
                )
            };
            let _ = writeln!(
                file,
                "{:>5.0}s | {:>4} {:>5.3} {:>5.3} | {:>4} {:>5.3} {:>5.3} | {:>3} {:>8.1} {:>8.1} {:>6}",
                started.elapsed().as_secs_f64(),
                state.stages[OUTBOUND].packets,
                mean(&state.stages[OUTBOUND]),
                ms(state.stages[OUTBOUND].worst_inside),
                state.stages[INBOUND].packets,
                mean(&state.stages[INBOUND]),
                ms(state.stages[INBOUND].worst_inside),
                trips.len(),
                trip_mean,
                trip_worst,
                state.outstanding.len(),
            );
            let _ = file.flush();
        }
    }))
}
