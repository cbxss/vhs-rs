//! Unix PTY engine: forkpty + tokio `AsyncFd` (ported from asciinema's
//! `src/pty.rs` pattern).
//!
//! The child process runs on the slave side of the PTY; the parent keeps the
//! master fd nonblocking and drives it through tokio's reactor. `EIO` on read
//! is mapped to EOF (`Ok(0)`) — that is how Linux reports "child gone" on a
//! PTY master.
//!
//! Lifecycle guarantees:
//! - [`Pty::shutdown`] terminates gracefully: SIGTERM, up to ~2s of polling,
//!   then SIGKILL + reap.
//! - [`Drop`] is a best-effort SIGKILL + blocking reap, so no zombie children
//!   survive even on panic/error paths.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
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
pub struct Pty {
    child: Pid,
    master: AsyncFd<OwnedFd>,
    /// Cached wait status once the child has been reaped (waitpid can only
    /// succeed once per child).
    status: Option<ExitStatus>,
}

impl Pty {
    /// Forks a child running `command` (via `execvp`, so `command[0]` is
    /// looked up on `PATH`) on a fresh PTY of `winsize = (cols, rows)`, with
    /// `env` applied on top of the inherited environment.
    ///
    /// Must be called from within a tokio runtime (the master fd registers
    /// with the reactor).
    pub fn spawn(
        command: &[String],
        env: &[(String, String)],
        winsize: (u16, u16),
    ) -> io::Result<Self> {
        if command.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty command"));
        }

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

            ForkptyResult::Child => exec_child(command, env),
        }
    }

    /// Reads available output from the child. `Ok(0)` means EOF (the child
    /// has exited and the PTY is drained). Awaits until the fd is readable.
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
    pub fn try_read(&self, buf: &mut [u8]) -> io::Result<usize> {
        match unistd::read(self.master.get_ref(), buf) {
            Ok(n) => Ok(n),
            Err(Errno::EIO) => Ok(0), // child gone: EOF
            Err(e) => Err(e.into()),  // EAGAIN maps to ErrorKind::WouldBlock
        }
    }

    /// Writes all of `bytes` to the child's input, awaiting writability as
    /// needed.
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

/// Child-side setup after fork: apply env, restore default SIGPIPE, exec.
/// Never returns; exits 127 if exec fails (shell convention).
fn exec_child(command: &[String], env: &[(String, String)]) -> ! {
    for (k, v) in env {
        // Safety: single-threaded child between fork and exec.
        unsafe { std::env::set_var(k, v) };
    }

    let _ = unsafe { signal::signal(Signal::SIGPIPE, SigHandler::SigDfl) };

    let args: Vec<CString> = match command
        .iter()
        .map(|s| CString::new(s.as_str()))
        .collect::<Result<_, _>>()
    {
        Ok(args) => args,
        Err(_) => unsafe { libc::_exit(127) },
    };

    let _ = unistd::execvp(&args[0], &args);
    unsafe { libc::_exit(127) }
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
