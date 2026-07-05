# valira-desktop

Cross-platform ValiraVPN client. Rust, with Slint for the interface.

The WireGuard private key is generated on the machine and never leaves it:
only its public half travels, when signing in creates the device.

## Layout

    ui/theme.slint    design tokens: colour, the 44px grid, type scale
    ui/components/    title bar, caption buttons, world map, widget vocabulary
    ui/app.slint      the three screens and the responsive split
    src/lib.rs        everything that is not the window, and where tests live
    src/main.rs       the window, and the wiring between it and the library
    src/api.rs        talks to valiravpn.com
    src/flags.rs      country flags, sliced from an embedded atlas
    src/geo.rs        puts a node on the map
    src/keys.rs       WireGuard key generation, x25519
    src/store.rs      session persistence in the platform config directory
    src/tunnel/       the two backends behind one interface
    src/tunnel/embedded/  the WireGuard this process carries itself
    src/win32_frame.rs  custom title bar over the native Windows frame
    vendor/wintun/    the signed Wintun DLLs, shipped beside the executable
    vendor/flag-icons/  licence for the flag set the atlas is built from
    windows/          the elevation manifest and the application icon

The brand mark is `ui/assets/logo.svg`, taken from the favicon the site serves at
`/assets/favicon.svg`: one teal stroke, `#2dd4bf`, the site's `--accent`. It sits
in the title bar ahead of the name, the same pairing the site header uses, and
`windows/valira.ico` carries it at 16 through 256 pixels for the taskbar, Alt-Tab
and Explorer. Replacing the logo means replacing those two files — the `.ico` is
built from the `.svg`, and `windows/valira.rc` embeds it.

The title bar is drawn by Slint but hit-tested by Win32, and the two have to
agree on where each control is. They no longer agree by repetition: the bar
measures its own controls after layout and publishes them as `hit-chip-start`,
`hit-chip-end` and `hit-profile-width`, which the hit-test reads. That is what
lets the tunnel chip be as wide as its label — no constant could describe a
control that resizes with its text — and it removes a pair of numbers that used
to be stated twice and kept in step by hand.

The library holds the domain and the binary holds the interface. That is also
what keeps `cargo test` working: the executable carries an elevation manifest,
Cargo attaches it to any test harness built from that target, and such a harness
then refuses to start unelevated with `ERROR_ELEVATION_REQUIRED`. The tests live
in the library, which the manifest never touches, and the binary is built with
`test = false`.

The interface thread never performs network calls or privileged commands, and
never spawns a process: the API runs on a worker thread and the tunnel is polled
on another, both reporting back through the event loop.

Nothing comes from `std-widgets` except `ListView`. Every control is drawn from
`ui/components/primitives.slint`, so the platform style cannot leak into it.

## The tunnel

There are two backends and `tunnel::up` picks between them.

The **system** backend hands the profile to a WireGuard already installed. The
data path is then in the kernel, which is the faster of the two.

The **embedded** backend carries the protocol here instead: `boringtun` for the
handshake and the session keys, `tun-rs` for the device. Nothing has to be
installed for it to work — on Windows it only needs the `wintun.dll` that
`build.rs` copies next to the executable.

The system one is preferred and the embedded one is the fallback, so a machine
with nothing installed still gets a tunnel and a machine that has WireGuard keeps
its speed. Both need elevated rights: creating a tunnel interface and rewriting
the routing table is privileged everywhere.

**The client lives in the tray.** Closing the window hides it and leaves the
tunnel alone; the program keeps running. A left click on the tray icon brings the
window back, and "Close" in its menu is what actually quits — that is where both
backends come down, and a panic hook does the same after a crash. Being killed
outright leaves the system tunnel standing, because nothing runs to stop it;
reopening the client and closing the tunnel is the way back.

That is why `main` uses `run_event_loop_until_quit`: the plain loop ends with the
last window, which for a background client is the wrong moment. On Windows the
icon is registered through the shell directly rather than through Slint, whose
tray support is `ksni` and so D-Bus only. It re-registers itself when Explorer
restarts, without which the client would go on running with no way back to its
own window.

Two things have to be true for a full tunnel, and the second is easy to miss:
everything leaves through the tunnel interface, and the encrypted packets
themselves do not — they would be routed into the tunnel carrying them. So a host
route pins the relay to the physical gateway, and the default is taken over by
`0.0.0.0/1` and `128.0.0.0/1`, which beat the existing default without deleting
it. Reverting is then only ever removing what was added.

**Both backends pin the relay, including the system one.** WireGuard for Windows
normally keeps its own packets off the tunnel by binding its socket to the
interface the relay sits on, so no host route is needed — until IP forwarding is
on, which Windows does the moment Internet Connection Sharing or the mobile
hotspot starts. Forwarding defeats the binding: the encrypted packets are routed
like any others, match the tunnel's own default route, and loop.

Measured with the hotspot running and no pin: the handshake completes, then 92
bytes come back against 5 MiB sent, and the machine loses the network entirely —
the symptom reads like a DNS fault, but nothing at all gets through. With the
pin, on the same hotspot: traffic flows and the public address is the relay's.
`tests/tunnel_endpoint_pin.rs` covers it end to end; it is `#[ignore]`d because
it needs administrator rights and rewrites the routing table.

Splitting `AllowedIPs` into `0.0.0.0/1` + `128.0.0.0/1` was the other candidate —
WireGuard only arms its kill-switch when a lone peer carries a `/0`, so the split
disarms it. It was measured and it does **not** fix this: same 92 bytes back. The
kill-switch was never the cause, and the split would have cost it for nothing.

Wintun is redistributed under the licence in `vendor/wintun/LICENSE.txt`, which
allows it alongside software that uses it only through the documented API.

## The exit list

Exits are grouped by country: one row per country, folding open onto its nodes,
each carrying its city and its moniker. Countries sort by the name on screen and
by a folded key — the service sends `São Paulo`, `Hénin-Beaumont`, `Rajbāri`, and
byte order would bury every one of them past Z.

What the control plane sends, and what to do with it:

    country       the full English name, always present
    country_code  ISO-2 upper case, but EMPTY on some records
    state         `provisioned` or `discovered`. Not a per-device flag, and not
                  a promise: exits marked `provisioned` were measured returning
                  nothing at all, so it says little about whether one works
    latitude
    longitude     present on most records, absent on the rest

So the country is settled by `geo::resolve_code`: the code when it is there, the
English name resolved through `ui/assets/countries.tsv` when it is not — that
file also carries the spellings this service uses where they differ from the flag
set's own (`United States`, `Turkey`, `Czechia`, `The Netherlands`, `Brunei`,
`Congo (DRC)`). Without that, the records with no code would have no flag, no
country to sit under, and no place on the map.

`state` is read the same way: only the exit in service says anything in the
status column. Treating `discovered` as "waiting" labelled six hundred healthy
exits as a problem.

A country is shut by default. The one holding the exit in service is open, a
filter opens everything it matched, and an explicit click overrides both — only
those clicks are remembered, so a poll rebuilding the list never reopens what was
folded away.

The flags are images, not emoji. Windows will not draw flag emoji: Segoe UI Emoji
carries no glyphs for the regional indicator pairs and renders the two letters
instead, deliberately. So `ui/assets/flags.bin` holds all 257 ISO-3166-1 alpha-2
flags as one strip of raw RGBA at 32x24, built from the MIT-licensed flag-icons
set, and `flags.rs` hands a row of it straight to Slint — no decoding, no files
on disk. `flags.idx` is the same countries as sorted fixed-width records, which
makes the lookup a binary search.

## The map

Past 880 logical pixels the window splits: the current view on the left, the
world map on the right. Narrower than that the map is dropped and the list takes
the whole window.

The map is drawn by `worldmap.rs`: flat land on dark water, hard coastlines, no
gradient and no texture, in the same rectangular high-contrast language as the
rest of the client.

It is vector, and it is filled on the CPU. `ui/assets/world-coast.bin` holds 1489
rings — 75k `f32` points, cut from the CC0 Natural Earth blank map and thinned by
Douglas-Peucker — and `tiny-skia` fills and strokes the ones inside the visible
window. Nothing here asks anything of the GPU; only the window's acrylic backdrop
does. A raster mask went blocky the moment the view zoomed past its own
resolution, where an outline stays exact at any scale. Fixed-point coordinates
had the same failure one step later: hundredths of a degree became a visible
lattice past about 20x.

Land is translucent, so the window behind the map is still part of it, and the
fill is even-odd — that keeps lakes as water and stops two overlapping rings
darkening each other now that the fill is no longer opaque.

**Detail follows the scale.** The source paths are country polygons, so stroking
them gives national borders where two countries meet and coastline everywhere
else. Those strokes fade in between 1.4x and 3x, and a ring too small to read at
the current scale is skipped rather than drawn as a speck. Zoomed out you get the
shape of the world; zoomed in you get its divisions.

The map is drawn at twice the pane and scaled back down. Land and water are close
in tone, so a single anti-aliased pixel along a coastline barely registers and the
edge reads as stepped.

Its coordinates are already the degree grid, so placing a node is
`x = (lon + 180) / 360` and `y = (83 - lat) / 139` with no projection maths and
no drift against the coastline. The wheel zooms on the pointer, dragging pans,
and the controls in the corner zoom on the centre. Nodes that merge at a
whole-world view come apart again as the view closes in: the grouping distance is
divided by the scale.

The first render has to be asked for explicitly. Slint's `changed` reports later
changes, not the opening layout, and this pane is only built once the window
crosses the wide breakpoint — so its first size is never a change, and the map
would stay blank until something resized it. A short timer primes it instead.

The control plane sends a country code and a city name but no coordinates, so
`geo.rs` carries a table: the city first, the country centroid when the city is
unknown, and nothing at all rather than a wrong guess — the list still holds
those, and the map says how many it could not place. `api.rs` already reads
`latitude`/`longitude` (also `lat`, `lon`, `lng`, `long`) off both exits and
relays if they are ever sent, and prefers them over the table. Nothing else has
to change on the day the service starts sending them.

Exits sharing a spot collapse into one node carrying the count, so a city with
several servers is one target rather than a stack of overlapping squares.

## Rendering

The window is transparent with a permanent acrylic blur behind it, which only
the GPU renderer can present. Slint builds its renderers suspended and creates
the OpenGL context when the window first appears, so a machine with no usable
driver — remote desktop, a VM without 3D, a fresh install before the display
driver — only fails at that point, and the backend is already committed for the
process. When that happens the client relaunches itself with the software
renderer pinned and an opaque window; every other part of the interface is
unchanged.

## Building

    cargo build --release

### Linux

`wireguard-tools` is used when present and not required otherwise: without it the
embedded tunnel takes over. At build time:

    libfontconfig-dev libxkbcommon-dev libwayland-dev libxcb1-dev
    libx11-dev libxrandr-dev libxi-dev libgl1-mesa-dev libegl1-mesa-dev

Run with `sudo`: creating a tunnel interface is privileged.

### macOS

`wireguard-tools` from Homebrew is used when present and not required otherwise:

    brew install wireguard-tools

Run with `sudo`.

### Windows

Nothing to install. WireGuard for Windows is used when it is there — set
`VALIRA_WIREGUARD` if it is not at the default location — and the embedded tunnel
takes over when it is not. `wintun.dll` ships beside the executable and has to
stay there.

The executable asks for administrator rights itself: an embedded manifest sets
`requestedExecutionLevel` to `requireAdministrator`, so Windows raises the
consent prompt before the program starts rather than letting it fail on the
first privileged call.

Building is unaffected — `cargo build`, `cargo test` and `cargo clippy` all run
from an ordinary shell. Only `cargo run` is, because it launches the result: the
loader refuses it with `ERROR_ELEVATION_REQUIRED`. Either work from an elevated
terminal, where `cargo run` behaves normally, or start it explicitly:

    Start-Process .\target\debug\valira-desktop.exe -Verb RunAs

Double-clicking the executable does the same thing.

## Settings

    VALIRA_API          control plane, defaults to https://valiravpn.com
    VALIRA_WIREGUARD    path to wireguard.exe, Windows only
    VALIRA_TUNNEL       pins the backend: embedded or system. Without it the
                        choice is automatic, which also means the embedded path
                        is never taken on a machine that has WireGuard — this is
                        how to exercise it there
    VALIRA_RENDERER     pins the renderer: software or gpu
    VALIRA_UI_PREVIEW   opens one screen with sample data and no account:
                        signin, choosing, connected or menu

## State

Kept in the platform configuration directory, restricted to the current user:

    Linux    ~/.config/valira/session.json
    macOS    ~/Library/Application Support/com.ValiraVPN.valira/session.json
    Windows  %APPDATA%\ValiraVPN\valira\config\session.json

It holds the account number, the token, and the device's private key. Signing
out removes it and revokes the device on the server.
