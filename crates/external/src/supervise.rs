//! Running an external program under supervision: its own process group, no inherited output, a
//! deadline and a cancel flag, and the whole process tree ended when the run stops early.

use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::ExternalError;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// How a supervised run ended.
#[derive(Debug)]
pub(crate) enum Ended {
    /// The program exited on its own.
    Exited(ExitStatus),
    /// The caller's `done` check passed; the program was ended.
    Done,
    TimedOut,
    Cancelled,
}

/// Start `cmd` in a new process group, without a console window and with its output discarded.
/// The caller chooses standard input.
pub(crate) fn spawn(mut cmd: Command, what: &str) -> Result<Child, ExternalError> {
    cmd.stdout(Stdio::null());
    spawn_with_stdout(cmd, what)
}

/// [`spawn`], keeping the standard output the caller set on `cmd` (a file, for a short answer).
pub(crate) fn spawn_with_stdout(mut cmd: Command, what: &str) -> Result<Child, ExternalError> {
    cmd.stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    cmd.spawn().map_err(|e| ExternalError::ProcessFailed(format!("couldn't start {what}: {e}")))
}

/// Wait for `child` until it exits, `done` returns true, `cancel` is set or `deadline` passes. In
/// every case but a normal exit the whole process tree is ended and reaped before returning.
pub(crate) fn wait(child: &mut Child, deadline: Instant, cancel: &AtomicBool, mut done: impl FnMut() -> bool) -> Ended {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ended::Exited(status),
            Ok(None) => {}
            Err(_) => {
                end_tree(child);
                return Ended::Cancelled;
            }
        }
        let ended = if done() {
            Some(Ended::Done)
        } else if cancel.load(Ordering::Relaxed) {
            Some(Ended::Cancelled)
        } else if Instant::now() >= deadline {
            Some(Ended::TimedOut)
        } else {
            None
        };
        if let Some(e) = ended {
            end_tree(child);
            return e;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// End `child` and every process it started, then reap it. Only for a child that is still running:
/// its process group then still exists, so the group id can't belong to anything else.
pub(crate) fn end_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        if let Some(pid) = i32::try_from(child.id()).ok().and_then(rustix::process::Pid::from_raw) {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // taskkill ends the tree (/T) by force (/F). It is part of every Windows installation.
        let system = std::env::var_os("SystemRoot").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"));
        let _ = Command::new(system.join("System32").join("taskkill.exe"))
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A program that starts a grandchild and then hangs: on timeout the grandchild must be gone too.
    #[test]
    fn timeout_ends_the_whole_tree() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("grandchild.pid");
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(format!("sleep 30 & echo $! > '{}'; wait", marker.display())).stdin(Stdio::null());
        let mut child = spawn(cmd, "sh").unwrap();
        let cancel = AtomicBool::new(false);
        let ended = wait(&mut child, Instant::now() + Duration::from_millis(800), &cancel, || false);
        assert!(matches!(ended, Ended::TimedOut), "{ended:?}");
        let pid: i32 = std::fs::read_to_string(&marker).unwrap().trim().parse().unwrap();
        // kill -0: the grandchild no longer exists (it may linger a moment as a zombie of init).
        let gone = (0..50).any(|_| {
            let alive = rustix::process::Pid::from_raw(pid).is_some_and(|p| rustix::process::test_kill_process(p).is_ok());
            if alive {
                thread::sleep(Duration::from_millis(50));
            }
            !alive
        });
        assert!(gone, "grandchild {pid} survived");
    }

    #[test]
    fn cancel_and_done_stop_the_program() {
        for use_done in [false, true] {
            let mut cmd = Command::new("/bin/sh");
            cmd.arg("-c").arg("sleep 30").stdin(Stdio::null());
            let mut child = spawn(cmd, "sh").unwrap();
            let cancel = AtomicBool::new(!use_done);
            let started = Instant::now();
            let ended = wait(&mut child, Instant::now() + Duration::from_secs(20), &cancel, || use_done);
            assert!(started.elapsed() < Duration::from_secs(5));
            assert!(matches!(ended, Ended::Done | Ended::Cancelled), "{ended:?}");
        }
    }
}
