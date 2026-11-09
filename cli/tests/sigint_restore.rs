//! Regression guard: Ctrl-C must not leave the terminal in raw mode.
//!
//! The line editor switches the terminal into non-canonical, no-echo mode
//! and relies on `RawMode`'s `Drop` to put it back. SIGINT's default
//! disposition terminates the process *without* unwinding, so `Drop` never
//! ran: the REPL died, and the shell it returned to had no echo and no
//! line editing — every subsequent keystroke invisible until the user ran
//! `reset`. Raw mode deliberately leaves `ISIG` set so Ctrl-C still
//! interrupts (see `lineedit`'s module docs), which is exactly the path
//! that skipped the guard.
//!
//! The test drives the real binary on a pseudo-terminal — raw mode only
//! engages when stdin is a TTY, so a pipe would never exercise the guard —
//! sends it SIGINT the way a Ctrl-C does, then reads the *terminal
//! device's* settings back. `termios` belongs to the terminal, not to a
//! process, so the parent can inspect the same settings the child changed.

#![cfg(unix)]

use std::ffi::{CStr, CString};
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A pseudo-terminal pair: the child's stdin/stdout/stderr, and the
/// parent's handle on the same device for reading output and inspecting
/// `termios`.
struct Pty {
    master: File,
    slave: File,
}

impl Pty {
    fn open() -> Self {
        // SAFETY: each call is checked before its result is used; a
        // failure panics rather than handing back a bad descriptor.
        unsafe {
            let master_fd = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            assert!(
                master_fd >= 0,
                "posix_openpt: {}",
                std::io::Error::last_os_error()
            );
            assert_eq!(
                libc::grantpt(master_fd),
                0,
                "grantpt: {}",
                std::io::Error::last_os_error()
            );
            assert_eq!(
                libc::unlockpt(master_fd),
                0,
                "unlockpt: {}",
                std::io::Error::last_os_error()
            );
            let name = libc::ptsname(master_fd);
            assert!(
                !name.is_null(),
                "ptsname: {}",
                std::io::Error::last_os_error()
            );
            let path = CString::from(CStr::from_ptr(name));
            let slave_fd = libc::open(path.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
            assert!(
                slave_fd >= 0,
                "open pty slave: {}",
                std::io::Error::last_os_error()
            );
            Self {
                master: File::from_raw_fd(master_fd),
                slave: File::from_raw_fd(slave_fd),
            }
        }
    }

    /// A fresh descriptor on the slave for one `Stdio` slot: `Stdio`
    /// consumes the `File`, and stdin/stdout/stderr each need their own.
    fn slave_stdio(&self) -> File {
        self.slave.try_clone().expect("dup pty slave")
    }

    fn termios(&self) -> libc::termios {
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::tcgetattr(self.slave.as_raw_fd(), &mut t) };
        assert_eq!(rc, 0, "tcgetattr: {}", std::io::Error::last_os_error());
        t
    }

    /// Read from the master until `needle` appears, or panic. `poll` bounds
    /// the wait so a hang becomes a failure with the transcript so far.
    fn read_until(&self, needle: &[u8]) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut seen = Vec::new();
        loop {
            if seen.windows(needle.len()).any(|w| w == needle) {
                return seen;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "timed out waiting for {:?}; saw {:?}",
                String::from_utf8_lossy(needle),
                String::from_utf8_lossy(&seen),
            );
            let mut pfd = libc::pollfd {
                fd: self.master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let n = unsafe { libc::poll(&mut pfd, 1, left.as_millis() as i32) };
            assert!(n >= 0, "poll: {}", std::io::Error::last_os_error());
            if n == 0 {
                continue;
            }
            let mut buf = [0u8; 512];
            let got = unsafe {
                libc::read(
                    self.master.as_raw_fd(),
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                )
            };
            if got < 0 {
                let err = std::io::Error::last_os_error();
                // A signal interrupting the read is not a failure.
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                panic!("read pty master: {err}");
            }
            assert!(got > 0, "pty master closed early");
            seen.extend_from_slice(&buf[..got as usize]);
        }
    }
}

/// Throwaway HOME so the child's prompt/summary/config files land in a
/// directory the test owns.
struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let home = std::env::temp_dir().join(format!("magent-sigint-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".config")).expect("create sandbox HOME");
        Self { home }
    }

    fn spawn(&self, pty: &Pty) -> Child {
        Command::new(env!("CARGO_BIN_EXE_magent"))
            .args(["run", "--repl", "--mock"])
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .stdin(Stdio::from(pty.slave_stdio()))
            .stdout(Stdio::from(pty.slave_stdio()))
            .stderr(Stdio::from(pty.slave_stdio()))
            .spawn()
            .expect("spawn magent")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

#[test]
fn sigint_restores_the_terminal_before_the_repl_dies() {
    let pty = Pty::open();

    let before = pty.termios();
    assert_ne!(
        before.c_lflag & libc::ICANON,
        0,
        "the test needs a canonical tty to start from",
    );

    let sb = Sandbox::new();
    let mut child = sb.spawn(&pty);

    // The prompt is written only after raw mode is entered, so seeing it
    // proves the child is inside the guard this test is about.
    let seen = pty.read_until("❯".as_bytes());
    let raw = pty.termios();
    assert_eq!(
        raw.c_lflag & libc::ICANON,
        0,
        "raw mode should be engaged once the prompt is up; saw {:?}",
        String::from_utf8_lossy(&seen),
    );

    // Ctrl-C: SIGINT delivered to the process.
    // SAFETY: `child.id()` is a live child pid.
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };

    let status = child.wait().expect("wait for magent");
    assert_eq!(
        status.signal(),
        Some(libc::SIGINT),
        "Ctrl-C should still terminate the REPL",
    );

    let after = pty.termios();
    assert_ne!(
        after.c_lflag & libc::ICANON,
        0,
        "ICANON must be restored, or the shell is left with no line editing",
    );
    assert_ne!(
        after.c_lflag & libc::ECHO,
        0,
        "ECHO must be restored, or every subsequent keystroke is invisible",
    );
    // Not just the two bits: every setting the guard borrowed.
    assert_eq!(
        after.c_lflag, before.c_lflag,
        "the terminal's local flags were not restored to their original value",
    );
    assert_eq!(after.c_cc[libc::VMIN], before.c_cc[libc::VMIN]);
    assert_eq!(after.c_cc[libc::VTIME], before.c_cc[libc::VTIME]);
}
