<div align="center">

# ValiraVPN Desktop

**The official desktop client for [ValiraVPN](https://valiravpn.com).**

A WireGuard client that brings its own tunnel.

[![Rust](https://img.shields.io/badge/Rust-2024%20edition-CE422B?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org)
[![Slint](https://img.shields.io/badge/Slint-1.17-2379F4?style=flat-square)](https://slint.dev)
[![WireGuard](https://img.shields.io/badge/WireGuard-boringtun-88171A?style=flat-square&logo=wireguard&logoColor=white)](https://www.wireguard.com)
[![rustls](https://img.shields.io/badge/TLS-rustls%20%2B%20ring-4B32C3?style=flat-square)](https://github.com/rustls/rustls)

[![Windows](https://img.shields.io/badge/Windows-10%2B-0078D4?style=flat-square&logo=windows&logoColor=white)](#windows)
[![Linux](https://img.shields.io/badge/Linux-X11%20%7C%20Wayland-FCC624?style=flat-square&logo=linux&logoColor=black)](#linux)
[![macOS](https://img.shields.io/badge/macOS-supported-000000?style=flat-square&logo=apple&logoColor=white)](#macos)

![The client on Windows: the exit list beside the world map, its acrylic backdrop letting the desktop through](docs/screenshot.png)

</div>


Account and subscription live at [valiravpn.com](https://valiravpn.com); this is
the client that connects to them. The WireGuard private key is generated on the
machine and never leaves it. Only its public half travels, when signing in
creates the device.

## Highlights

- **Nothing to install alongside it.** `boringtun` carries the protocol and
  `tun-rs` owns the device, so the client is its own WireGuard. Where WireGuard
  for Windows is already present it is used instead, for its kernel data path.
- **The relay is pinned off the tunnel,** so encrypted packets cannot be routed
  back into the tunnel that produced them. That is what otherwise breaks the
  VPN the moment Internet Connection Sharing turns IP forwarding on.
- **A world map drawn on the CPU** from coastline vectors. No GPU required, and
  no tiles fetched from anywhere.
- **Residential and datacentre exits told apart** on the service's authority,
  filterable in both the list and the map.
- **A frame of its own on Windows:** acrylic backdrop, Snap Layouts, and a
  notification-area icon that keeps the tunnel up while the window is away.

## Install

Windows: take the installer from the
[latest release](https://github.com/ValiraVPN/valiravpn-desktop/releases/latest).
A SHA-256 checksum sits beside it.

Linux and macOS: build from source for now.

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

## Building

    cargo build --release
    cargo test

### Windows

Nothing to install. WireGuard for Windows is used when present. Set
`VALIRA_WIREGUARD` if it is not at the default location. The embedded
tunnel takes over when it is not. `wintun.dll` ships beside the executable and
has to stay there.

The executable asks for administrator rights through an embedded manifest, so
`cargo run` fails with `ERROR_ELEVATION_REQUIRED` from an ordinary shell. Build
and test are unaffected. Run it from an elevated terminal, or:

    Start-Process .\target\debug\valira-desktop.exe -Verb RunAs

An installer is built from `installer/valira.iss` with Inno Setup 6.

### Linux

Build dependencies:

    build-essential pkg-config libfontconfig-dev
    libxkbcommon-dev libxkbcommon-x11-dev
    libwayland-dev wayland-protocols
    libx11-dev libxcursor-dev libxrandr-dev libxi-dev
    libgl1-mesa-dev libegl1-mesa-dev

`libxkbcommon-x11` must also be present at run time, or the client exits before
drawing anything. `wireguard-tools` is used when present and not required
otherwise. Run with `sudo`: creating a tunnel interface is privileged.

There is no tray icon yet, so the close button closes the client. See
[BUILDING-LINUX.md](BUILDING-LINUX.md) for what else is still missing there.

### macOS

`wireguard-tools` from Homebrew is used when present and not required otherwise.
Run with `sudo`.

## Settings

    VALIRA_API             control plane, defaults to https://valiravpn.com
    VALIRA_WIREGUARD       path to wireguard.exe, Windows only
    VALIRA_TUNNEL          pins the backend: embedded or system. Without it the
                           choice is automatic, which also means the embedded
                           path is never taken on a machine that has WireGuard
    VALIRA_RENDERER        pins the renderer: software or gpu
    VALIRA_TUNNEL_TRACE    per-packet timings, written to the temp directory
    VALIRA_UI_PREVIEW      opens one screen with sample data and no account:
                           signin, choosing, connected or menu

## State

Kept in the platform configuration directory, restricted to the current user:

    Linux    ~/.config/valira/session.json
    macOS    ~/Library/Application Support/com.ValiraVPN.valira/session.json
    Windows  %APPDATA%\ValiraVPN\valira\config\session.json

It holds the account number, the token, and the device's private key. Signing
out removes it and revokes the device on the server.

## Design notes

How the tunnel, the exit list and the map actually work, and the measurements
behind the decisions: [docs/design.md](docs/design.md).

## Third-party

- **Wintun**, WireGuard LLC, `vendor/wintun/LICENSE.txt`. Redistributed as that
  licence allows, alongside software using only its documented API.
- **flag-icons**, MIT, `vendor/flag-icons/LICENSE`. The flag atlas is built
  from it.
- **Natural Earth**, public domain. The coastline vectors come from it.
- **Inter**, SIL Open Font License 1.1.
- **Slint**, used under the Slint Royalty-free Desktop, Mobile, and Web
  Applications License, not under its GPL option. That licence asks for
  attribution in one of two places: an About screen inside the application, or
  the public page its binaries are downloaded from.

## Licence

PolyForm Noncommercial 1.0.0, in [LICENSE.md](LICENSE.md). Anyone may read, run,
modify and share this source for any purpose that is not commercial. Commercial
use is reserved to ValiraVPN.

This is a source-available licence rather than an open source one: it restricts
the field of use, which the Open Source Definition does not permit.
