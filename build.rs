use std::path::{Path, PathBuf};

fn main() {
    slint_build::compile("ui/app.slint").expect("compiling the Slint interface");
    require_administrator();
    place_wintun();
}

/// Embeds the manifest that makes Windows ask for elevation before the program
/// starts. Every route to a working tunnel is privileged, so asking afterwards
/// would only mean failing later.
///
/// A failure here is fatal on purpose: a binary that quietly lost its manifest
/// would look identical and never be able to bring a tunnel up.
fn require_administrator() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    println!("cargo:rerun-if-changed=windows/valira.rc");
    println!("cargo:rerun-if-changed=windows/valira.manifest");

    // Scoped to the one binary rather than every link: `compile` would attach it
    // to the test harness too, and `cargo test` would then refuse to run
    // unelevated with ERROR_ELEVATION_REQUIRED.
    embed_resource::compile_for("windows/valira.rc", ["valira-desktop"], embed_resource::NONE)
        .manifest_required()
        .expect("embedding the elevation manifest");

    stamp_version();
}

/// Writes the package version into the executable's own resources.
///
/// Generated rather than kept in `valira.rc` by hand, so it cannot drift from
/// `Cargo.toml`. The installer reads it straight back out of the built binary,
/// which means the setup it produces can never advertise a version the program
/// does not carry. It also fills in what Explorer shows under Properties, which
/// was blank.
fn stamp_version() {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let mut parts = version
        .split(['.', '-', '+'])
        .filter_map(|p| p.parse::<u16>().ok());
    let (major, minor, patch) = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );

    // No external file is named in here, so it compiles just as well from the
    // output directory as it would from the source tree.
    let script = format!(
        "1 VERSIONINFO
         FILEVERSION {major},{minor},{patch},0
         PRODUCTVERSION {major},{minor},{patch},0
         FILEOS 0x4L
         FILETYPE 0x1L
         BEGIN
         BLOCK \"StringFileInfo\"
         BEGIN
         BLOCK \"040904B0\"
         BEGIN
         VALUE \"CompanyName\", \"ValiraVPN\"
         VALUE \"FileDescription\", \"ValiraVPN\"
         VALUE \"FileVersion\", \"{major}.{minor}.{patch}.0\"
         VALUE \"InternalName\", \"valira-desktop\"
         VALUE \"LegalCopyright\", \"ValiraVPN\"
         VALUE \"OriginalFilename\", \"valira-desktop.exe\"
         VALUE \"ProductName\", \"ValiraVPN\"
         VALUE \"ProductVersion\", \"{major}.{minor}.{patch}.0\"
         END
         END
         BLOCK \"VarFileInfo\"
         BEGIN
         VALUE \"Translation\", 0x409, 1200
         END
         END
"
    );

    let out = std::path::Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR"))
        .join("version.rc");
    std::fs::write(&out, script).expect("writing the version resource");
    embed_resource::compile_for(&out, ["valira-desktop"], embed_resource::NONE)
        .manifest_optional()
        .expect("embedding the version resource");
}

/// Wintun has to sit next to the executable: it is loaded by name at runtime,
/// and shipping it is what frees the user from installing WireGuard at all. Its
/// licence allows redistribution alongside software that only uses the
/// documented API, which is what `tun-rs` does — see vendor/wintun/LICENSE.txt.
fn place_wintun() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "amd64",
        Ok("aarch64") => "arm64",
        Ok("x86") => "x86",
        Ok("arm") => "arm",
        other => {
            println!("cargo:warning=no Wintun build for target arch {other:?}");
            return;
        }
    };

    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("vendor/wintun")
        .join(arch)
        .join("wintun.dll");
    println!("cargo:rerun-if-changed={}", source.display());

    // OUT_DIR is target/<profile>/build/<crate>-<hash>/out; the executable
    // lands three levels up.
    let Ok(out_dir) = std::env::var("OUT_DIR") else {
        return;
    };
    let Some(exe_dir) = PathBuf::from(&out_dir)
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
    else {
        return;
    };

    if let Err(error) = std::fs::copy(&source, exe_dir.join("wintun.dll")) {
        println!(
            "cargo:warning=could not place wintun.dll next to the executable: {error}. The \
             embedded tunnel will not start without it."
        );
    }
}
