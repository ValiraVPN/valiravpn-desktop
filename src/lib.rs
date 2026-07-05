//! Everything the client does that is not the window.
//!
//! The binary keeps the interface: the Slint types, the custom Windows frame,
//! the wiring between callbacks and this library. What lives here is the part
//! that can be reasoned about, and tested, without a screen.
//!
//! The split is also what keeps `cargo test` runnable. The executable carries a
//! manifest demanding administrator rights — every route to a working tunnel is
//! privileged — and Cargo attaches that manifest to any test harness built from
//! the binary too, which then refuses to start unelevated. Tests live here
//! instead, in a target the manifest never touches.

pub mod api;
pub mod flags;
pub mod geo;
pub mod keys;
pub mod store;
pub mod tunnel;
pub mod worldmap;
