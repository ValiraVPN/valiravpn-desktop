# ValiraVPN for Desktop (Windows/Linux)

[![CI](https://github.com/ValiraVPN/valiravpn-desktop/actions/workflows/ci.yml/badge.svg)](https://github.com/ValiraVPN/valiravpn-desktop/actions/workflows/ci.yml)
[![Release](https://github.com/ValiraVPN/valiravpn-desktop/actions/workflows/release.yml/badge.svg)](https://github.com/ValiraVPN/valiravpn-desktop/actions/workflows/release.yml)
[![License](https://img.shields.io/badge/license-PolyForm%20Noncommercial%201.0.0-blue?style=flat-square)](LICENSE.md)

[![Rust](https://img.shields.io/badge/Rust-2024%20edition-CE422B?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org)
[![Slint](https://img.shields.io/badge/Slint-1.17-2379F4?style=flat-square)](https://slint.dev)
[![WireGuard](https://img.shields.io/badge/WireGuard-boringtun-88171A?style=flat-square&logo=wireguard&logoColor=white)](https://www.wireguard.com)
[![rustls](https://img.shields.io/badge/TLS-rustls%20%2B%20ring-4B32C3?style=flat-square)](https://github.com/rustls/rustls)
[![Windows](https://img.shields.io/badge/Windows-10%2B-0078D4?style=flat-square&logo=windows&logoColor=white)](#requirements)
[![Linux](https://img.shields.io/badge/Linux-X11%20%7C%20Wayland-FCC624?style=flat-square&logo=linux&logoColor=black)](#requirements)

**ValiraVPN for Desktop** is the official ValiraVPN app for desktop platforms. Some of the
features include: an embedded WireGuard tunnel that needs nothing installed alongside it,
residential and datacentre exits told apart and filterable, a world map drawn on the CPU
with no GPU and no map tiles, an acrylic frame with Snap Layouts on Windows, and a
notification area icon that keeps the tunnel up while the window is away.
ValiraVPN accounts are managed on the official site [valiravpn.com](https://valiravpn.com).

![ValiraVPN application image](docs/screenshot.png)

* [About this Repo](#about-this-repo)
* [Installation](#installation)
  * [Requirements](#requirements)
  * [Compilation](#compilation)
* [Configuration](#configuration)
* [Versioning](#versioning)
* [Contributing](#contributing)
* [Security Policy](#security-policy)
* [License](#license)
* [Authors](#authors)
* [Acknowledgements](#acknowledgements)

<a name="about-this-repo"></a>
## About this Repo

The desktop client, written in Rust with [Slint](https://slint.dev) for the interface.

    ui/            design tokens, widget vocabulary, the three screens
    src/api.rs     the control plane at valiravpn.com
    src/tunnel/    two backends behind one interface
    src/worldmap/  coastline vectors rasterised on the CPU
    installer/     Inno Setup script for the Windows installer

How the tunnel, the exit list and the map work, and the measurements behind those
decisions, are in [docs/design.md](docs/design.md).

<a name="installation"></a>
## Installation

Windows binaries are published with each release. Take the installer from the
[latest release](https://github.com/ValiraVPN/valiravpn-desktop/releases/latest); a
SHA-256 checksum is published beside it. Linux and macOS are built from source.

<a name="requirements"></a>
### Requirements

**Windows.** Nothing. WireGuard for Windows is used when present and the embedded tunnel
takes over when it is not. `wintun.dll` ships beside the executable.

**Linux.** Build dependencies:

    build-essential pkg-config libfontconfig-dev
    libxkbcommon-dev libxkbcommon-x11-dev
    libwayland-dev wayland-protocols
    libx11-dev libxcursor-dev libxrandr-dev libxi-dev
    libgl1-mesa-dev libegl1-mesa-dev

`libxkbcommon-x11` is also needed at run time, without which the client exits before
drawing anything. See [BUILDING-LINUX.md](BUILDING-LINUX.md) for what is not implemented
there yet.

**macOS.** `wireguard-tools` from Homebrew is used when present, and is not required.

<a name="compilation"></a>
### Compilation

    cargo build --release
    cargo test --lib

The Windows installer is built from `installer/valira.iss` with Inno Setup 6.

The executable carries a manifest asking for administrator rights, so `cargo run` is
refused from an ordinary shell with `ERROR_ELEVATION_REQUIRED`. Build and test are not
affected. Run it from an elevated terminal, or:

    Start-Process .\target\release\valira-desktop.exe -Verb RunAs

On Linux and macOS, run it with `sudo`: creating a tunnel interface is privileged.

<a name="configuration"></a>
## Configuration

    VALIRA_API             control plane, defaults to https://valiravpn.com
    VALIRA_WIREGUARD       path to wireguard.exe, Windows only
    VALIRA_TUNNEL          pins the backend: embedded or system
    VALIRA_RENDERER        pins the renderer: software or gpu
    VALIRA_TUNNEL_TRACE    per-packet timings, written to the temp directory
    VALIRA_UI_PREVIEW      opens one screen with sample data and no account

The session is kept in the platform configuration directory, readable only by the
current user. It holds the account number, the token and the device private key.
Signing out removes it and revokes the device.

    Linux    ~/.config/valira/session.json
    macOS    ~/Library/Application Support/com.ValiraVPN.valira/session.json
    Windows  %APPDATA%\ValiraVPN\valira\config\session.json

<a name="versioning"></a>
## Versioning

[Semantic Versioning](https://semver.org). Pushing a `vX.Y.Z` tag builds the Windows
installer and publishes a release. The tag is checked against `Cargo.toml` first, so an
installer can never advertise a version its executable does not carry.

<a name="contributing"></a>
## Contributing

See [CONTRIBUTING.md](.github/CONTRIBUTING.md). Note that contributions are licensed on
the terms in [LICENSE.md](LICENSE.md), which reserve commercial use to ValiraVPN.

<a name="security-policy"></a>
## Security Policy

Do not report vulnerabilities through public issues. See
[SECURITY.md](.github/SECURITY.md).

The WireGuard private key is generated on the machine and never leaves it. Only its
public half travels, when signing in creates the device.

<a name="license"></a>
## License

PolyForm Noncommercial 1.0.0, in [LICENSE.md](LICENSE.md). Anyone may read, run, modify
and share this source for any purpose that is not commercial. Commercial use is reserved
to ValiraVPN. This is a source-available licence rather than an open source one, since it
restricts the field of use.

<a name="authors"></a>
## Authors

See [AUTHORS](AUTHORS).

<a name="acknowledgements"></a>
## Acknowledgements

ValiraVPN for Desktop is built on the work of others, named in
[ACKNOWLEDGEMENTS.md](ACKNOWLEDGEMENTS.md).
