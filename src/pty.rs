//! Unix PTY engine: forkpty + tokio `AsyncFd` (ported from asciinema's
//! `src/pty.rs` pattern).
//!
//! The child process runs on the slave side of the PTY; the parent keeps the
//! master fd nonblocking and drives it through tokio's reactor. `EIO` on read
//! is mapped to EOF (`Ok(0)`) — that is how Linux reports "child gone" on a
//! PTY master.
//!
//! Fork safety: everything the child needs to exec — argv, the merged
//! environment, the PATH-resolved candidate paths, and the raw pointer
//! tables for `execve` — is allocated in [`ExecImage::prepare`] BEFORE
//! `forkpty`. Between fork and exec only async-signal-safe calls are made
//! (`signal`, `execve`, `_exit`): if the parent process has other threads,
//! any lock they hold at fork time (malloc, the env lock) is copied into
//! the child in a locked state, and touching it would deadlock.
//!
//! Lifecycle guarantees:
//! - [`Pty::shutdown`] terminates gracefully: SIGTERM, up to ~2s of polling,
//!   then SIGKILL + reap.
//! - [`Drop`] is a best-effort SIGKILL + blocking reap, so no zombie children
//!   survive even on panic/error paths.

use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::time::Duration;

use nix::errno::Errno;
use nix::fcntl::{self, FcntlArg, OFlag};
use nix::libc;
use nix::pty::{self, ForkptyResult, Winsize};
use nix::sys::signal::{self, SigHandler, Signal};
use nix::sys::wait::{self, WaitPidFlag, WaitStatus};
use nix::unistd::{self, Pid};
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;

/// How the child terminated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    /// Normal exit with the given code.
    Exited(i32),
    /// Killed by the given signal number.
    Signaled(i32),
}

/// A spawned child process attached to a pseudo-terminal.
#[derive(Debug)]
pub struct Pty {
    child: Pid,
    master: AsyncFd<OwnedFd>,
    /// Cached wait status once the child has been reaped (waitpid can only
    /// succeed once per child).
    status: Option<ExitStatus>,
}

impl Pty {
    /// Forks a child running `command` (`command[0]` is looked up on `PATH`,
    /// execvp-style, unless it contains `/`) on a fresh PTY of
    /// `winsize = (cols, rows)`, with `env` applied on top of the inherited
    /// environment.
    ///
    /// Must be called from within a tokio runtime (the master fd registers
    /// with the reactor).
    ///
    /// # Errors
    /// Returns `InvalidInput` for an empty `command` or a NUL byte in any
    /// argument or environment entry, or any OS error from `forkpty`,
    /// setting the master nonblocking, or reactor registration.
    pub fn spawn(
        command: &[String],
        env: &[(String, String)],
        winsize: (u16, u16),
    ) -> io::Result<Self> {
        if command.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty command"));
        }

        // Everything the child touches is allocated before the fork.
        let image = ExecImage::prepare(command, env)?;

        let (cols, rows) = winsize;
        let ws = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };

        match unsafe { pty::forkpty(Some(&ws), None) }.map_err(io::Error::from)? {
            ForkptyResult::Parent { child, master } => {
                set_nonblocking(&master)?;
                let master = AsyncFd::new(master)?;

                Ok(Self {
                    child,
                    master,
                    status: None,
                })
            }

            ForkptyResult::Child => image.exec(),
        }
    }

    /// Reads available output from the child. `Ok(0)` means EOF (the child
    /// has exited and the PTY is drained). Awaits until the fd is readable.
    ///
    /// # Errors
    /// Returns any read error on the master fd (`EIO` maps to EOF, not an
    /// error).
    pub async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.master
            .async_io(Interest::READABLE, |fd| match unistd::read(fd, buf) {
                Ok(n) => Ok(n),
                Err(Errno::EIO) => Ok(0), // child gone: EOF
                Err(e) => Err(e.into()),
            })
            .await
    }

    /// Nonblocking read. Returns `Ok(0)` on EOF and
    /// `Err(kind = WouldBlock)` when no data is currently available.
    ///
    /// # Errors
    /// `WouldBlock` when no data is buffered; any other read error on the
    /// master fd (`EIO` maps to EOF).
    pub fn try_read(&self, buf: &mut [u8]) -> io::Result<usize> {
        match unistd::read(self.master.get_ref(), buf) {
            Ok(n) => Ok(n),
            Err(Errno::EIO) => Ok(0), // child gone: EOF
            Err(e) => Err(e.into()),  // EAGAIN maps to ErrorKind::WouldBlock
        }
    }

    /// Writes all of `bytes` to the child's input, awaiting writability as
    /// needed.
    ///
    /// # Errors
    /// Returns any write error on the master fd, or `WriteZero` if the PTY
    /// stops accepting bytes.
    pub async fn write_all(&self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let n = self
                .master
                .async_io(Interest::WRITABLE, |fd| {
                    unistd::write(fd, bytes).map_err(io::Error::from)
                })
                .await?;

            if n == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }

            bytes = &bytes[n..];
        }

        Ok(())
    }

    /// Updates the PTY window size (TIOCSWINSZ); the child receives SIGWINCH.
    pub fn resize(&self, cols: u16, rows: u16) {
        let ws = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };

        unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &ws) };
    }

    /// The child's process id.
    pub fn pid(&self) -> i32 {
        self.child.as_raw()
    }

    /// Nonblocking check whether the child has exited (reaps it if so).
    /// Returns the cached status on subsequent calls.
    ///
    /// # Errors
    /// Returns an error if `waitpid` fails (e.g. the child was already
    /// reaped elsewhere).
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.status.is_some() {
            return Ok(self.status);
        }

        match wait::waitpid(self.child, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => Ok(None),
            Ok(WaitStatus::Exited(_, code)) => {
                self.status = Some(ExitStatus::Exited(code));
                Ok(self.status)
            }
            Ok(WaitStatus::Signaled(_, sig, _)) => {
                self.status = Some(ExitStatus::Signaled(sig as i32));
                Ok(self.status)
            }
            Ok(_) => Ok(None), // stopped/continued: still our child
            Err(e) => Err(e.into()),
        }
    }

    /// Graceful teardown: SIGTERM, poll for up to ~2s, then SIGKILL and a
    /// blocking reap. Idempotent; returns the child's exit status.
    ///
    /// # Errors
    /// Returns an error if `waitpid` fails while reaping the child.
    pub async fn shutdown(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.status.is_some() {
            return Ok(self.status);
        }

        let _ = signal::kill(self.child, Signal::SIGTERM);

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);

        loop {
            if self.try_wait()?.is_some() {
                return Ok(self.status);
            }

            if tokio::time::Instant::now() >= deadline {
                break;
            }

            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let _ = signal::kill(self.child, Signal::SIGKILL);

        match wait::waitpid(self.child, None) {
            Ok(WaitStatus::Exited(_, code)) => self.status = Some(ExitStatus::Exited(code)),
            Ok(WaitStatus::Signaled(_, sig, _)) => {
                self.status = Some(ExitStatus::Signaled(sig as i32));
            }
            // Anything else: mark as signaled by SIGKILL so Drop won't retry.
            _ => self.status = Some(ExitStatus::Signaled(Signal::SIGKILL as i32)),
        }

        Ok(self.status)
    }
}

impl Drop for Pty {
    /// Best-effort: never leave a zombie, even on error/panic paths.
    fn drop(&mut self) {
        if self.status.is_none() {
            let _ = signal::kill(self.child, Signal::SIGKILL);
            let _ = wait::waitpid(self.child, None);
        }
    }
}

/// Fallback PATH when neither the overrides nor the inherited environment
/// define one (mirrors the spirit of execvp's confstr default).
const DEFAULT_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// Everything the child needs to exec, fully allocated before `forkpty`:
/// argv, the merged environment, the execvp-style PATH candidates, and the
/// NULL-terminated pointer tables `execve` consumes. The child side —
/// [`ExecImage::exec`] — makes only async-signal-safe calls.
struct ExecImage {
    /// Candidate executable paths, tried in order. A `command[0]` containing
    /// `/` yields exactly one; otherwise one per PATH entry.
    candidates: Vec<CString>,
    /// Owning storage for the strings the pointer tables reference. The
    /// `CString` heap buffers never move, so the pointers stay valid for the
    /// life of the image.
    _argv: Vec<CString>,
    _envp: Vec<CString>,
    argv_ptrs: Vec<*const libc::c_char>,
    envp_ptrs: Vec<*const libc::c_char>,
}

impl ExecImage {
    fn prepare(command: &[String], env: &[(String, String)]) -> io::Result<Self> {
        let argv: Vec<CString> = command
            .iter()
            .map(|s| cstring(s.as_bytes(), "argument"))
            .collect::<io::Result<_>>()?;

        // Merged environment: inherited, with `env` overrides applied on top
        // (later overrides win, matching the old set_var loop).
        let mut merged: Vec<(OsString, OsString)> = std::env::vars_os().collect();
        for (k, v) in env {
            let key = OsStr::new(k);
            match merged.iter_mut().find(|(mk, _)| mk == key) {
                Some(slot) => slot.1 = v.into(),
                None => merged.push((k.into(), v.into())),
            }
        }

        let envp: Vec<CString> = merged
            .iter()
            .map(|(k, v)| {
                let mut kv = k.as_bytes().to_vec();
                kv.push(b'=');
                kv.extend_from_slice(v.as_bytes());
                cstring(&kv, "environment entry")
            })
            .collect::<io::Result<_>>()?;

        // execvp-style PATH resolution, done up front. The child just tries
        // each candidate in order; a nonexistent one fails with ENOENT and
        // the loop moves on, exactly like execvp.
        let name = command[0].as_str();
        let candidates: Vec<CString> = if name.contains('/') {
            vec![cstring(name.as_bytes(), "command")?]
        } else {
            let path = merged
                .iter()
                .find(|(k, _)| k == "PATH")
                .map_or_else(|| OsString::from(DEFAULT_PATH), |(_, v)| v.clone());
            std::env::split_paths(&path)
                .map(|dir| cstring(dir.join(name).as_os_str().as_bytes(), "command path"))
                .collect::<io::Result<_>>()?
        };

        let mut argv_ptrs: Vec<*const libc::c_char> = argv.iter().map(|c| c.as_ptr()).collect();
        argv_ptrs.push(std::ptr::null());
        let mut envp_ptrs: Vec<*const libc::c_char> = envp.iter().map(|c| c.as_ptr()).collect();
        envp_ptrs.push(std::ptr::null());

        Ok(Self {
            candidates,
            _argv: argv,
            _envp: envp,
            argv_ptrs,
            envp_ptrs,
        })
    }

    /// Child-side after fork: restore default SIGPIPE, exec the first
    /// candidate that works. Never returns; exits 127 if all fail (shell
    /// convention). Only async-signal-safe calls — no allocation, no env
    /// access.
    fn exec(&self) -> ! {
        let _ = unsafe { signal::signal(Signal::SIGPIPE, SigHandler::SigDfl) };

        for path in &self.candidates {
            // Only returns on failure; try the next PATH candidate.
            unsafe {
                libc::execve(
                    path.as_ptr(),
                    self.argv_ptrs.as_ptr(),
                    self.envp_ptrs.as_ptr(),
                )
            };
        }

        unsafe { libc::_exit(127) }
    }
}

fn cstring(bytes: &[u8], what: &str) -> io::Result<CString> {
    CString::new(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("NUL byte in {what}: {:?}", String::from_utf8_lossy(bytes)),
        )
    })
}

fn set_nonblocking(fd: &OwnedFd) -> io::Result<()> {
    let flags = fcntl::fcntl(fd, FcntlArg::F_GETFL)?;
    let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
    fcntl::fcntl(fd, FcntlArg::F_SETFL(flags))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    async fn read_to_eof(pty: &Pty) -> String {
        let mut buf = [0u8; 4096];
        let mut out = Vec::new();

        loop {
            let n = pty.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }

        String::from_utf8_lossy(&out).into_owned()
    }

    #[tokio::test]
    async fn spawn_read_shutdown_no_zombie() {
        let mut pty = Pty::spawn(&cmd(&["/bin/sh", "-c", "echo hi"]), &[], (80, 24)).unwrap();
        let pid = pty.pid();

        let output = read_to_eof(&pty).await;
        assert!(output.contains("hi"), "output: {output:?}");

        let status = pty.shutdown().await.unwrap();
        assert_eq!(status, Some(ExitStatus::Exited(0)));

        drop(pty);

        // Already reaped: the pid is no longer our child.
        let res = wait::waitpid(Pid::from_raw(pid), Some(WaitPidFlag::WNOHANG));
        assert_eq!(res, Err(Errno::ECHILD));
    }

    #[tokio::test]
    async fn drop_reaps_running_child() {
        let pty = Pty::spawn(&cmd(&["/bin/sh", "-c", "sleep 30"]), &[], (80, 24)).unwrap();
        let pid = pty.pid();

        drop(pty); // SIGKILL + reap

        let res = wait::waitpid(Pid::from_raw(pid), Some(WaitPidFlag::WNOHANG));
        assert_eq!(res, Err(Errno::ECHILD));
    }

    #[tokio::test]
    async fn write_all_roundtrip() {
        let mut pty = Pty::spawn(&cmd(&["/bin/cat"]), &[], (80, 24)).unwrap();

        pty.write_all(b"foobar").await.unwrap();

        let mut buf = [0u8; 4096];
        let mut out = String::new();

        while !out.contains("foobar") {
            let n = pty.read(&mut buf).await.unwrap();
            assert!(n > 0, "unexpected EOF; got: {out:?}");
            out.push_str(&String::from_utf8_lossy(&buf[..n]));
        }

        assert_eq!(pty.try_wait().unwrap(), None);
        let status = pty.shutdown().await.unwrap();
        assert!(status.is_some());
    }

    #[tokio::test]
    async fn bare_names_resolve_via_path() {
        let pty = Pty::spawn(&cmd(&["sh", "-c", "printf via-path"]), &[], (80, 24)).unwrap();

        let output = read_to_eof(&pty).await;
        assert!(output.contains("via-path"), "output: {output:?}");
    }

    #[tokio::test]
    async fn env_path_override_governs_resolution() {
        use std::os::unix::fs::PermissionsExt;

        // A bare name must resolve against the PATH the child will see (the
        // override), not the parent's.
        let dir = std::env::temp_dir().join(format!("vhs_rs-pty-path-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("vhs-rs-test-marker");
        std::fs::write(&bin, "#!/bin/sh\nprintf from-override\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        let env = vec![("PATH".to_string(), dir.display().to_string())];
        let pty = Pty::spawn(&cmd(&["vhs-rs-test-marker"]), &env, (80, 24)).unwrap();

        let output = read_to_eof(&pty).await;
        assert!(output.contains("from-override"), "output: {output:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn missing_binary_exits_127() {
        let mut pty = Pty::spawn(&cmd(&["definitely-not-a-binary-vhs-rs"]), &[], (80, 24)).unwrap();

        read_to_eof(&pty).await;
        let status = pty.shutdown().await.unwrap();
        assert_eq!(status, Some(ExitStatus::Exited(127)));
    }

    #[tokio::test]
    async fn nul_bytes_fail_before_forking() {
        let env = vec![("X".to_string(), "a\0b".to_string())];
        let err = Pty::spawn(&cmd(&["/bin/sh"]), &env, (80, 24)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        let err = Pty::spawn(&cmd(&["/bin/e\0cho"]), &[], (80, 24)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn extra_env_reaches_child() {
        let env = vec![("VHS_RS_TEST_VAR".to_string(), "marker42".to_string())];
        let pty = Pty::spawn(
            &cmd(&["/bin/sh", "-c", "printf %s \"$VHS_RS_TEST_VAR\""]),
            &env,
            (80, 24),
        )
        .unwrap();

        let output = read_to_eof(&pty).await;
        assert!(output.contains("marker42"), "output: {output:?}");
    }
}
