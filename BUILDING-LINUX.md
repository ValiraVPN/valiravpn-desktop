# Building on Linux

Verified on Ubuntu 26.04: `cargo build` clean, the client starts, the window
renders, its controls work, and 29 of the library's tests pass — the two that
do not run are the Wintun ones, which are Windows only.

## What to copy

The source only. Leave `target/` behind: it holds Windows build artefacts and
is several gigabytes of nothing useful here. `vendor/wintun/` can come along or
not — it is the Windows tunnel driver and `build.rs` ignores it on Linux.

## Build dependencies

```sh
sudo apt install build-essential pkg-config curl ca-certificates \
  libfontconfig-dev libxkbcommon-dev libxkbcommon-x11-dev \
  libwayland-dev wayland-protocols \
  libx11-dev libxcursor-dev libxrandr-dev libxi-dev \
  libgl1-mesa-dev libegl1-mesa-dev
```

`libfontconfig-dev` is not optional. Slint discovers system fonts through it,
and there is no way round it: the "load it at run time instead" build mode of
`yeslogic-fontconfig-sys` exposes a different API than the font stack above it
expects, and the build fails further along instead.

Then the toolchain, if it is not already there:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

## Run-time dependencies

**`libxkbcommon-x11`** must be present or the client dies on startup with
`Library libxkbcommon-x11.so could not be loaded` — before drawing anything.
The `-dev` package above pulls it in on a build machine; on a machine that only
runs the client, install `libxkbcommon-x11-0`.

## Building

```sh
cargo build --release        # target/release/valira-desktop
cargo test --lib             # 29 tests
```

## Running

The tunnel needs to create a network interface and rewrite the routing table,
so the client needs `CAP_NET_ADMIN` — in practice, root:

```sh
sudo ./target/release/valira-desktop
```

On a machine with no usable OpenGL driver the client relaunches itself with the
software renderer; `VALIRA_RENDERER=software` pins that from the start.

## What is not there yet

Honest list, so nothing comes as a surprise:

* **No tray icon.** On Windows the client lives in the notification area and
  the close button only hides its window. There is no equivalent here yet, so
  on Linux the close button *closes the client* — tunnel and all. Hiding the
  only window of a program that cannot be brought back would be a trap. This is
  deliberate, and it is the first thing to replace, with StatusNotifierItem.
* **No privilege escalation.** Windows carries a manifest that asks for
  administrator before the program starts. Here you run it with `sudo`
  yourself; `pkexec` is the piece that would make that automatic.
* **The tunnel has never been exercised on Linux.** The code is there
  (`src/tunnel/linux.rs`, `tun-rs` over `/dev/net/tun`, routes through `ip` and
  a `resolv.conf` rewrite) and it compiles, but no tunnel has actually been
  brought up on a Linux machine. Treat it as unproven.
* **No packaging.** No `.deb`, no AppImage, no desktop entry.

## Things that were platform-specific and are not any more

Worth knowing, if the Windows build ever looks different from this one:

* The interface's small symbols used to be characters from "Segoe MDL2 Assets",
  a font that exists only on Windows. Everywhere else the lock, the profile and
  all three window controls were simply absent — not even a missing-glyph box.
  They are vector paths now (`ui/components/glyphs.slint`).
* The window controls used to be decoration, hit-tested and clicked by the Win32
  frame through `WM_NCHITTEST`. They carry their own touch areas now. Windows is
  unaffected: it declares that strip non-client, so the clicks never reach Slint
  there and Snap Layouts still work.
* The single-instance guard is a named mutex on Windows and an `flock` here.
