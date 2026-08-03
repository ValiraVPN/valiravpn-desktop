# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project follows
[Semantic Versioning](https://semver.org).

## [Unreleased]

### Added

* Embedded WireGuard tunnel, so nothing has to be installed alongside the client.
  WireGuard for Windows is still preferred when present, for its kernel data path.
* Residential and datacentre exits told apart on the service's authority, filterable in
  both the exit list and the map.
* World map rasterised on the CPU from coastline vectors, with no GPU and no map tiles.
* Custom window frame on Windows: acrylic backdrop, Snap Layouts, and a notification
  area icon that keeps the tunnel up while the window is away.
* Windows installer, built with Inno Setup and published on tagged releases with a
  SHA-256 checksum.

### Known limitations

* Linux has no tray icon yet, so closing the window closes the client.
* The tunnel has not been exercised on Linux. It compiles and is unproven.
* macOS is untested.
