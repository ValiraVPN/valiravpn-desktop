//! A WireGuard tunnel carried by this process.
//!
//! `boringtun` owns the protocol — handshake, session keys, timers — and
//! `tun-rs` owns the device. Between them sits the pump in this file: packets
//! off the tunnel interface get encrypted and sent to the relay, packets off the
//! socket get decrypted and written back to the interface.
//!
//! Nothing here needs WireGuard to be installed. On Windows it needs the
//! `wintun.dll` that `build.rs` places beside the executable; everywhere it
//! needs the elevated rights any tunnel interface requires.
//!
//! The tunnel lives and dies with the process. `down` puts the routing table and
//! the resolver back before it lets the device go, and `main` calls it on the
//! way out however the way out came about.

use super::{Profile, Stats, INTERFACE};
use base64::Engine;
use boringtun::noise::{Tunn, TunnResult};
use boringtun::x25519::{PublicKey, StaticSecret};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;
use tun_rs::{DeviceBuilder, InterruptEvent, SyncDevice};

mod net;
mod trace;

/// WireGuard's own default: 1420 leaves room for its 60 bytes of framing inside
/// a 1500-byte path.
const MTU: u16 = 1420;
/// One MTU plus framing, rounded up.
const BUFFER: usize = 1600;
/// How often the protocol timers run, and how long a socket read waits before
/// letting its thread notice a shutdown.
const TICK: Duration = Duration::from_millis(250);
/// WireGuard's usual keepalive, which also keeps NAT bindings open.
const KEEPALIVE: u16 = 25;

struct Running {
    stop: Arc<AtomicBool>,
    wake: Arc<InterruptEvent>,
    tunnel: Arc<Mutex<Tunn>>,
    workers: Vec<JoinHandle<()>>,
    routes: net::Routes,
}

fn slot() -> &'static Mutex<Option<Running>> {
    static SLOT: OnceLock<Mutex<Option<Running>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// A poisoned lock here means a worker panicked. The state behind it is still
/// the truth about what is running, and refusing to look at it would strand the
/// routing table.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn up(profile: &Profile) -> Result<(), String> {
    let _ = down();

    let private = StaticSecret::from(decode_key(&profile.private_key, "private")?);
    let peer = PublicKey::from(decode_key(&profile.peer_public_key, "relay public")?);
    let endpoint = resolve(&profile.endpoint)?;

    let device = Arc::new(build_device(profile)?);
    let name = device
        .name()
        .map_err(|error| format!("interface name: {error}"))?;
    let socket = Arc::new(open_socket(endpoint)?);

    let tunnel = Arc::new(Mutex::new(Tunn::new(
        private,
        peer,
        None,
        Some(KEEPALIVE),
        0,
        None,
    )));

    // Routing last: until it is in place nothing is diverted, so a failure here
    // leaves the machine exactly as it was.
    let routes = net::apply(&name, &device, endpoint.ip(), &profile.dns)?;

    let stop = Arc::new(AtomicBool::new(false));
    let wake = Arc::new(
        InterruptEvent::new().map_err(|error| format!("shutdown signal: {error}"))?,
    );

    let workers = vec![
        outbound(
            device.clone(),
            socket.clone(),
            tunnel.clone(),
            stop.clone(),
            wake.clone(),
        ),
        inbound(device.clone(), socket.clone(), tunnel.clone(), stop.clone()),
        timers(socket.clone(), tunnel.clone(), stop.clone()),
    ];
    let mut workers = workers;
    if let Some(reporter) = trace::reporting(stop.clone()) {
        workers.push(reporter);
    }

    *lock(slot()) = Some(Running {
        stop,
        wake,
        tunnel,
        workers,
        routes,
    });
    Ok(())
}

pub fn down() -> Result<(), String> {
    let Some(running) = lock(slot()).take() else {
        return Ok(());
    };

    running.stop.store(true, Ordering::Relaxed);
    let _ = running.wake.trigger();

    // Put the machine back before the device goes: reverting afterwards would
    // be aiming route deletions at an interface that no longer exists.
    net::revert(running.routes);

    for worker in running.workers {
        let _ = worker.join();
    }
    Ok(())
}

pub fn active() -> bool {
    lock(slot()).is_some()
}

pub fn stats() -> Stats {
    let guard = lock(slot());
    let Some(running) = guard.as_ref() else {
        return Stats::default();
    };
    let (since_handshake, sent, received, _loss, _rtt) = lock(&running.tunnel).stats();
    Stats {
        received: received as u64,
        sent: sent as u64,
        handshake_age: since_handshake.map(|age| age.as_secs()),
    }
}

// ── the pump ─────────────────────────────────────────────────────────────────

/// Interface to relay.
fn outbound(
    device: Arc<SyncDevice>,
    socket: Arc<UdpSocket>,
    tunnel: Arc<Mutex<Tunn>>,
    stop: Arc<AtomicBool>,
    wake: Arc<InterruptEvent>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut plain = vec![0u8; BUFFER];
        let mut sealed = vec![0u8; BUFFER];

        while !stop.load(Ordering::Relaxed) {
            // The two platforms hand packets over in genuinely different ways,
            // and reading them the same way is what made the Windows tunnel
            // slow.
            //
            // Windows is a ring with an edge-triggered event: the event fires
            // when the ring stops being empty, so a single signal can stand for
            // several packets. Taking one and going back to wait leaves the
            // rest sitting there until new traffic signals again — the backlog
            // then drains only as fast as it grows, and every packet inherits
            // the wait of everything ahead of it. Measured on the real path:
            // 1419 ms average and 55% loss before draining the ring on each
            // wake, 31 ms and no loss after.
            //
            // Unix is a file descriptor, which stays readable for as long as
            // anything is queued behind the packet just taken. One read a turn
            // loses nothing, and there is no `try_recv` to drain with anyway.
            #[cfg(windows)]
            {
                if device.wait_readable_intr(&wake).is_err() {
                    continue;
                }
                loop {
                    let read = match device.try_recv(&mut plain) {
                        Ok(0) => break,
                        Ok(read) => read,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(_) => break,
                    };
                    forward(&plain[..read], &tunnel, &socket, &mut sealed);
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                }
            }

            #[cfg(not(windows))]
            {
                // Blocks until a packet arrives or `wake` is triggered, so a
                // quiet tunnel still shuts down at once.
                let Ok(read) = device.recv_intr(&mut plain, &wake) else {
                    continue;
                };
                if read == 0 {
                    continue;
                }
                forward(&plain[..read], &tunnel, &socket, &mut sealed);
            }
        }
    })
}

/// Seals one packet from the interface and puts it on the wire.
fn forward(packet: &[u8], tunnel: &Mutex<Tunn>, socket: &UdpSocket, sealed: &mut [u8]) {
    let entered = trace::mark();
    trace::leaving(packet);
    if let TunnResult::WriteToNetwork(out) = lock(tunnel).encapsulate(packet, sealed) {
        let _ = socket.send(out);
    }
    trace::spent(trace::OUTBOUND, entered);
}

/// Relay to interface.
fn inbound(
    device: Arc<SyncDevice>,
    socket: Arc<UdpSocket>,
    tunnel: Arc<Mutex<Tunn>>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut sealed = vec![0u8; BUFFER];
        let mut plain = vec![0u8; BUFFER];
        let mut queued = vec![0u8; BUFFER];

        while !stop.load(Ordering::Relaxed) {
            // The read timeout is what lets this thread see the stop flag.
            let Ok(read) = socket.recv(&mut sealed) else {
                continue;
            };

            let entered = trace::mark();
            let mut guard = lock(&tunnel);
            match guard.decapsulate(None, &sealed[..read], &mut plain) {
                TunnResult::WriteToNetwork(reply) => {
                    let _ = socket.send(reply);
                    // A handshake leaves more to send; boringtun hands those
                    // over one at a time when called again with no datagram.
                    while let TunnResult::WriteToNetwork(more) =
                        guard.decapsulate(None, &[], &mut queued)
                    {
                        let _ = socket.send(more);
                    }
                }
                TunnResult::WriteToTunnelV4(packet, _) | TunnResult::WriteToTunnelV6(packet, _) => {
                    trace::returning(packet);
                    let _ = device.send(packet);
                }
                TunnResult::Done | TunnResult::Err(_) => {}
            }
            drop(guard);
            trace::spent(trace::INBOUND, entered);
        }
    })
}

/// Handshake retries, rekeying and keepalives.
fn timers(
    socket: Arc<UdpSocket>,
    tunnel: Arc<Mutex<Tunn>>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut scratch = vec![0u8; BUFFER];
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(TICK);
            if let TunnResult::WriteToNetwork(packet) = lock(&tunnel).update_timers(&mut scratch) {
                let _ = socket.send(packet);
            }
        }
    })
}

// ── setup ────────────────────────────────────────────────────────────────────

fn build_device(profile: &Profile) -> Result<SyncDevice, String> {
    let mut builder = DeviceBuilder::new().name(INTERFACE).mtu(MTU);

    for address in &profile.addresses {
        match parse_cidr(address)? {
            (IpAddr::V4(host), prefix) => builder = builder.ipv4(host, prefix, None),
            (IpAddr::V6(host), prefix) => builder = builder.ipv6(host, prefix),
        }
    }

    builder.build_sync().map_err(|error| {
        format!(
            "création de l'interface {INTERFACE} : {error}. \
             Vérifiez que ValiraVPN tourne avec les droits nécessaires."
        )
    })
}

fn open_socket(endpoint: SocketAddr) -> Result<UdpSocket, String> {
    let local: SocketAddr = if endpoint.is_ipv4() {
        "0.0.0.0:0".parse().expect("literal")
    } else {
        "[::]:0".parse().expect("literal")
    };

    let socket = UdpSocket::bind(local).map_err(|error| format!("UDP socket: {error}"))?;
    socket
        .connect(endpoint)
        .map_err(|error| format!("connecting to relay {endpoint}: {error}"))?;
    // Bounds how long the reader waits, which is how it notices a shutdown.
    socket
        .set_read_timeout(Some(TICK))
        .map_err(|error| format!("read timeout: {error}"))?;
    Ok(socket)
}

fn resolve(endpoint: &str) -> Result<SocketAddr, String> {
    endpoint
        .to_socket_addrs()
        .map_err(|error| format!("relay address {endpoint:?}: {error}"))?
        .next()
        .ok_or_else(|| format!("relay {endpoint:?} resolves to no address"))
}

fn decode_key(encoded: &str, what: &str) -> Result<[u8; 32], String> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|error| format!("unreadable {what} key: {error}"))?;
    raw.try_into()
        .map_err(|_| format!("{what} key: expected 32 bytes"))
}

fn parse_cidr(address: &str) -> Result<(IpAddr, u8), String> {
    let (host, prefix) = match address.split_once('/') {
        Some((host, prefix)) => (
            host,
            prefix
                .parse::<u8>()
                .map_err(|_| format!("unreadable prefix in {address:?}"))?,
        ),
        // A bare address is the single host it names.
        None => (address, if address.contains(':') { 128 } else { 32 }),
    };

    let host: IpAddr = host
        .trim()
        .parse()
        .map_err(|_| format!("unreadable address: {address:?}"))?;
    Ok((host, prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_addresses_with_and_without_a_prefix() {
        assert_eq!(
            parse_cidr("10.64.0.12/32").unwrap(),
            ("10.64.0.12".parse::<IpAddr>().unwrap(), 32)
        );
        assert_eq!(
            parse_cidr("10.64.0.12").unwrap(),
            ("10.64.0.12".parse::<IpAddr>().unwrap(), 32)
        );
        let (host, prefix) = parse_cidr("fda8:75e8:355::2/128").unwrap();
        assert!(host.is_ipv6());
        assert_eq!(prefix, 128);
        // A bare v6 address defaults to a single host, not to 32.
        assert_eq!(parse_cidr("fda8:75e8:355::2").unwrap().1, 128);
    }

    #[test]
    fn rejects_rubbish_addresses() {
        assert!(parse_cidr("not-an-address").is_err());
        assert!(parse_cidr("10.64.0.12/nope").is_err());
    }

    #[test]
    fn decodes_a_wireguard_key_and_refuses_a_short_one() {
        let key = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
        assert_eq!(decode_key(&key, "test").unwrap(), [7u8; 32]);

        let short = base64::engine::general_purpose::STANDARD.encode([7u8; 16]);
        assert!(decode_key(&short, "test").is_err());
        assert!(decode_key("not base64!!", "test").is_err());
    }
}

#[cfg(test)]
mod socket_timing {
    use std::net::UdpSocket;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// What a datagram costs on the socket pattern the pump uses.
    ///
    /// Not a test of correctness: it reproduces, on loopback and with nothing
    /// else involved, the one arrangement the tunnel depends on — a single
    /// blocking UDP socket with a receive timeout, read by one thread while
    /// another writes to it. If that arrangement is what is costing two
    /// seconds a packet, it shows up here, with no interface, no relay, no
    /// route and no risk to the machine.
    ///
    /// `cargo test --release --lib -- --ignored --nocapture what_a_datagram_costs`
    #[test]
    #[ignore = "timing, not correctness"]
    fn what_a_datagram_costs() {
        // Stands in for the relay: echoes every datagram straight back.
        let relay = UdpSocket::bind("127.0.0.1:0").expect("relay socket");
        let relay_addr = relay.local_addr().expect("relay address");
        std::thread::spawn(move || {
            let mut buf = [0u8; 1600];
            while let Ok((read, from)) = relay.recv_from(&mut buf) {
                let _ = relay.send_to(&buf[..read], from);
            }
        });

        // The tunnel's own socket, set up exactly as `open_socket` does.
        let socket = UdpSocket::bind("127.0.0.1:0").expect("tunnel socket");
        socket.connect(relay_addr).expect("connect");
        socket
            .set_read_timeout(Some(super::TICK))
            .expect("read timeout");

        // The reader, mirroring `inbound`: blocked in `recv` almost always.
        let reader = socket.try_clone().expect("clone");
        let (seen, arrivals) = mpsc::channel::<Instant>();
        std::thread::spawn(move || {
            let mut buf = [0u8; 1600];
            loop {
                match reader.recv(&mut buf) {
                    Ok(_) => {
                        if seen.send(Instant::now()).is_err() {
                            return;
                        }
                    }
                    // The timeout is what lets the real thread see its stop flag.
                    Err(_) => continue,
                }
            }
        });

        const ROUNDS: usize = 20;
        let payload = [7u8; 128];
        let mut send_cost = Vec::with_capacity(ROUNDS);
        let mut round_trip = Vec::with_capacity(ROUNDS);

        for _ in 0..ROUNDS {
            let started = Instant::now();
            socket.send(&payload).expect("send");
            send_cost.push(started.elapsed());
            match arrivals.recv_timeout(Duration::from_secs(5)) {
                Ok(at) => round_trip.push(at.duration_since(started)),
                Err(_) => panic!("the echo never came back"),
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        let report = |what: &str, samples: &[Duration]| {
            let ms = |d: &Duration| d.as_secs_f64() * 1000.0;
            let total: f64 = samples.iter().map(ms).sum();
            let worst = samples.iter().map(ms).fold(0.0_f64, f64::max);
            println!(
                "{what:<34} moyenne {:>8.3} ms   pire {:>8.3} ms",
                total / samples.len() as f64,
                worst
            );
        };
        report("send, lecteur bloque a cote", &send_cost);
        report("aller-retour complet", &round_trip);
    }
}

#[cfg(test)]
mod crypto_timing {
    use boringtun::noise::{Tunn, TunnResult};
    use boringtun::x25519::{PublicKey, StaticSecret};
    use std::time::Instant;

    /// What boringtun costs per packet, once a session is up.
    ///
    /// Two peers talking to each other in memory: no socket, no interface, no
    /// relay. Isolates the protocol work from everything the pump wraps around
    /// it.
    ///
    /// `cargo test --release --lib -- --ignored --nocapture what_a_packet_costs_to_seal`
    #[test]
    #[ignore = "timing, not correctness"]
    fn what_a_packet_costs_to_seal() {
        let client_secret = StaticSecret::from([3u8; 32]);
        let relay_secret = StaticSecret::from([5u8; 32]);
        let client_public = PublicKey::from(&client_secret);
        let relay_public = PublicKey::from(&relay_secret);

        let mut client = Tunn::new(client_secret, relay_public, None, None, 0, None);
        let mut relay = Tunn::new(relay_secret, client_public, None, None, 1, None);

        let mut a = vec![0u8; super::BUFFER];
        let mut b = vec![0u8; super::BUFFER];

        // A real IPv4 packet: `decapsulate` parses what it opens and refuses
        // anything that is not one, so a buffer of filler would be rejected
        // rather than measured.
        let payload = ipv4_packet(32);
        let started = Instant::now();
        let TunnResult::WriteToNetwork(init) = client.encapsulate(&payload, &mut a) else {
            panic!("no handshake initiation");
        };
        let init = init.to_vec();
        let TunnResult::WriteToNetwork(response) = relay.decapsulate(None, &init, &mut b) else {
            panic!("no handshake response");
        };
        let response = response.to_vec();
        let name = |r: &TunnResult| match r {
            TunnResult::Done => "Done",
            TunnResult::Err(_) => "Err",
            TunnResult::WriteToNetwork(_) => "WriteToNetwork",
            TunnResult::WriteToTunnelV4(_, _) => "WriteToTunnelV4",
            TunnResult::WriteToTunnelV6(_, _) => "WriteToTunnelV6",
        };
        let first = client.decapsulate(None, &response, &mut a);
        println!("reponse de poignee de main -> {}", name(&first));
        let mut queued = Vec::new();
        if let TunnResult::WriteToNetwork(packet) = first {
            queued.push(packet.to_vec());
        }
        // Ce que la pompe fait ensuite : redemander tant qu'il reste a envoyer.
        loop {
            let more = client.decapsulate(None, &[], &mut a);
            println!("  relance a vide          -> {}", name(&more));
            match more {
                TunnResult::WriteToNetwork(packet) => queued.push(packet.to_vec()),
                _ => break,
            }
        }
        println!("poignee de main                    {:>8.3} ms", started.elapsed().as_secs_f64() * 1000.0);
        println!("paquets liberes par la relance     {}", queued.len());

        // Steady state: seal one, open it on the other side.
        const ROUNDS: usize = 2000;
        let started = Instant::now();
        for _ in 0..ROUNDS {
            let TunnResult::WriteToNetwork(sealed) = client.encapsulate(&payload, &mut a) else {
                panic!("nothing to send");
            };
            let sealed = sealed.to_vec();
            match relay.decapsulate(None, &sealed, &mut b) {
                TunnResult::WriteToTunnelV4(_, _) | TunnResult::WriteToTunnelV6(_, _) => {}
                TunnResult::Done => {}
                other => panic!("unexpected result: {other:?}"),
            }
        }
        let each = started.elapsed().as_secs_f64() * 1000.0 / ROUNDS as f64;
        println!("sceller + ouvrir, par paquet       {each:>8.4} ms");
    }

    /// Smallest thing boringtun will accept as a tunnelled packet: a well
    /// formed IPv4 header, with `payload` bytes after it.
    fn ipv4_packet(payload: usize) -> Vec<u8> {
        let total = 20 + payload;
        let mut packet = vec![0u8; total];
        packet[0] = 0x45; // version 4, 5 words of header
        packet[2..4].copy_from_slice(&(total as u16).to_be_bytes());
        packet[8] = 64; // TTL
        packet[9] = 1; // ICMP
        packet[12..16].copy_from_slice(&[10, 64, 0, 2]);
        packet[16..20].copy_from_slice(&[1, 1, 1, 1]);
        packet
    }
}

#[cfg(all(test, windows))]
mod device_timing {
    use std::time::{Duration, Instant};
    use tun_rs::{DeviceBuilder, InterruptEvent};

    /// What a packet costs to cross the tunnel interface, both ways.
    ///
    /// Windows does the sending and the timing: `ping` is aimed at an address
    /// on the far side of the interface, this reads each echo request out of
    /// the ring, turns it into a reply and writes it back. What `ping` reports
    /// is then the cost of a full write-and-read through Wintun and nothing
    /// else — no relay, no encryption, no routing.
    ///
    /// The interface carries an unused /30 and no routes at all, so nothing on
    /// the machine is diverted; it lives only for the length of the test.
    ///
    /// Needs administrator, like any tunnel interface.
    /// `cargo test --release --lib -- --ignored --nocapture what_the_interface_costs`
    #[test]
    #[ignore = "timing, needs administrator, creates a temporary interface"]
    fn what_the_interface_costs() {
        const HOST: &str = "10.255.253.1";
        const PEER: &str = "10.255.253.2";

        let device = std::sync::Arc::new(
            DeviceBuilder::new()
                .name("valira-probe")
                .mtu(super::MTU)
                .ipv4(HOST.parse::<std::net::Ipv4Addr>().unwrap(), 30, None)
                .build_sync()
                .expect("creating the probe interface (administrator?)"),
        );

        // The interface has just appeared: let the stack finish bringing it up.
        std::thread::sleep(Duration::from_millis(1500));

        let answer = device.clone();
        let answering = std::thread::spawn(move || {
            let wake = InterruptEvent::new().expect("interrupt event");
            let mut buf = vec![0u8; super::BUFFER];
            let deadline = Instant::now() + Duration::from_secs(25);
            let mut answered = 0usize;
            let mut waits: Vec<Duration> = Vec::new();
            let mut writes: Vec<Duration> = Vec::new();
            let mut idle = Instant::now();
            while Instant::now() < deadline {
                let asked = Instant::now();
                let Ok(read) = answer.recv_intr_timeout(
                    &mut buf,
                    &wake,
                    Some(Duration::from_millis(20)),
                ) else {
                    continue;
                };
                // How long this call had to wait once it was the one that
                // returned a packet, and how long since the previous packet.
                waits.push(asked.elapsed());
                let gap = idle.elapsed();
                idle = Instant::now();
                let _ = gap;
                // IPv4 without options, ICMP, echo request.
                if read < 28 || buf[0] != 0x45 || buf[9] != 1 || buf[20] != 8 {
                    continue;
                }
                let mut reply = buf[..read].to_vec();
                // Source and destination swap, which leaves the header checksum
                // untouched: a ones' complement sum does not care in which
                // order its words appear.
                for offset in 0..4 {
                    reply.swap(12 + offset, 16 + offset);
                }
                reply[20] = 0; // echo reply
                reply[22] = 0;
                reply[23] = 0;
                let checksum = ones_complement(&reply[20..read]);
                reply[22..24].copy_from_slice(&checksum.to_be_bytes());
                let writing = Instant::now();
                let wrote = answer.send(&reply).is_ok();
                writes.push(writing.elapsed());
                if wrote {
                    answered += 1;
                }
            }
            let ms = |d: &Duration| d.as_secs_f64() * 1000.0;
            let mean = |v: &Vec<Duration>| {
                if v.is_empty() { 0.0 } else { v.iter().map(ms).sum::<f64>() / v.len() as f64 }
            };
            let worst = |v: &Vec<Duration>| v.iter().map(ms).fold(0.0_f64, f64::max);
            println!(
                "lecture qui rend un paquet   moyenne {:>8.3} ms   pire {:>8.3} ms   ({} paquets)",
                mean(&waits), worst(&waits), waits.len()
            );
            println!(
                "ecriture vers l'interface    moyenne {:>8.3} ms   pire {:>8.3} ms",
                mean(&writes), worst(&writes)
            );
            answered
        });

        let output = std::process::Command::new("ping")
            .args(["-n", "8", "-w", "3000", PEER])
            .output()
            .expect("running ping");
        let text = String::from_utf8_lossy(&output.stdout);
        let answered = answering.join().unwrap_or(0);

        let mut times = Vec::new();
        for line in text.lines() {
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

        println!("--- ce que ping a vu ---");
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            println!("{line}");
        }
        println!("paquets repondus par le test : {answered}");
        assert!(
            !times.is_empty(),
            "ping reported no round trip; the interface answered {answered} packets"
        );
        let total: f64 = times.iter().sum();
        let worst = times.iter().cloned().fold(0.0_f64, f64::max);
        println!(
            "interface, aller-retour   {} mesures   moyenne {:.1} ms   pire {:.1} ms",
            times.len(),
            total / times.len() as f64,
            worst
        );
    }

    fn ones_complement(bytes: &[u8]) -> u16 {
        let mut sum = 0u32;
        for pair in bytes.chunks(2) {
            let word = match pair {
                [high, low] => u16::from_be_bytes([*high, *low]),
                [high] => u16::from_be_bytes([*high, 0]),
                _ => unreachable!(),
            };
            sum += word as u32;
        }
        while sum >> 16 != 0 {
            sum = (sum & 0xFFFF) + (sum >> 16);
        }
        !(sum as u16)
    }
}
