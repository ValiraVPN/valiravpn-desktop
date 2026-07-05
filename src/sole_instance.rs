//! One client to a machine.
//!
//! A second copy would open a window onto a tunnel it does not own. Both would
//! then report its state from their own idea of it, and the first thing either
//! did to it would make the other wrong — including bringing it down while the
//! other still showed it up.
//!
//! Each platform has its own way of saying "taken", and neither is portable:
//! Windows names a mutex in the session's namespace, Unix takes an exclusive
//! lock on a file. What they share is that the claim dies with the process, so
//! a client that is killed outright leaves nothing behind to block the next one.

/// Whether this process is the client, or a second copy that should stand down.
pub fn claim() -> bool {
    imp::claim()
}

/// Gives the claim up, so a process this one is about to start can take it.
///
/// Used by the renderer fallback, which relaunches this program with the
/// software renderer pinned: the child would otherwise find the claim held by
/// the parent that spawned it and stand down, leaving no client at all on
/// exactly the machines that need the fallback.
pub fn release() {
    imp::release();
}

#[cfg(windows)]
mod imp {
    pub fn claim() -> bool {
        crate::win32_frame::claim_sole_instance()
    }
    pub fn release() {
        crate::win32_frame::release_sole_instance();
    }
}

#[cfg(unix)]
mod imp {
    use std::fs::File;
    use std::os::fd::AsRawFd;
    use std::sync::Mutex;
    use std::sync::OnceLock;

    /// Held open for as long as this process is the client: the lock lives on
    /// the open file, not on its contents, and the kernel drops it when the
    /// last descriptor closes — including when the process dies for any reason.
    fn held() -> &'static Mutex<Option<File>> {
        static HELD: OnceLock<Mutex<Option<File>>> = OnceLock::new();
        HELD.get_or_init(|| Mutex::new(None))
    }

    /// The session's runtime directory when there is one, since that is cleared
    /// between logins and is per user. `/tmp` otherwise, where the name carries
    /// the user id so two people on one machine do not lock each other out.
    fn lock_path() -> std::path::PathBuf {
        if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
            return std::path::Path::new(&runtime).join("valira-desktop.lock");
        }
        let uid = unsafe { libc::getuid() };
        std::path::Path::new("/tmp").join(format!("valira-desktop-{uid}.lock"))
    }

    pub fn claim() -> bool {
        let Ok(file) = File::create(lock_path()) else {
            // Nowhere to put the claim. Refusing to start over that would be
            // worse than the duplicate it is meant to prevent.
            return true;
        };
        // Non-blocking on purpose: the question is whether it is free now, not
        // whether it will be.
        let taken = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        if taken {
            *held().lock().unwrap_or_else(|p| p.into_inner()) = Some(file);
        }
        taken
    }

    pub fn release() {
        // Dropping the file closes the descriptor, which is what releases the
        // lock.
        *held().lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

#[cfg(not(any(windows, unix)))]
mod imp {
    pub fn claim() -> bool {
        true
    }
    pub fn release() {}
}
