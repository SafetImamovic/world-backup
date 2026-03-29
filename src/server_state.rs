use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result};

pub fn world_appears_running(world_dir: &Path) -> Result<bool> {
    let lock_path = world_dir.join("session.lock");
    let file = match OpenOptions::new().read(true).write(true).open(&lock_path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to open Minecraft lock file '{}'",
                    lock_path.display()
                )
            });
        }
    };

    is_locked(&file).with_context(|| {
        format!(
            "failed to inspect Minecraft lock state for '{}'",
            lock_path.display()
        )
    })
}

/// Minecraft's Java server acquires the lock via `FileChannel.tryLock()`, which
/// maps to POSIX `fcntl()` locks on Unix. The `fs2` crate (and the `flock()`
/// syscall it wraps) uses a completely independent lock namespace on Linux, so
/// an `flock`-based probe never sees the Java lock. We query with `F_GETLK`
/// instead, which operates in the same `fcntl` namespace as Java.
#[cfg(unix)]
fn is_locked(file: &std::fs::File) -> std::io::Result<bool> {
    use std::os::unix::io::AsRawFd;

    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = libc::F_WRLCK as _;
    lock.l_whence = libc::SEEK_SET as _;
    // l_start and l_len are already zero → entire file.

    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &mut lock) } == -1 {
        return Err(std::io::Error::last_os_error());
    }

    // F_GETLK sets l_type to F_UNLCK when no conflicting lock exists.
    Ok(lock.l_type != libc::F_UNLCK as _)
}

/// On Windows, `LockFileEx` is the single mechanism used by both Java and fs2,
/// so an `flock`-style exclusive-lock probe still detects the server's lock.
#[cfg(windows)]
fn is_locked(file: &std::fs::File) -> std::io::Result<bool> {
    use fs2::FileExt;

    match file.try_lock_exclusive() {
        Ok(()) => {
            file.unlock()?;
            Ok(false)
        }
        Err(error)
            if error.kind() == ErrorKind::WouldBlock
                || matches!(error.raw_os_error(), Some(33)) =>
        {
            Ok(true)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::world_appears_running;

    #[test]
    fn missing_session_lock_means_not_running() {
        let root = tempdir().unwrap();
        assert!(!world_appears_running(root.path()).unwrap());
    }

    #[test]
    fn unlocked_session_lock_means_not_running() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("session.lock"), b"lock").unwrap();
        assert!(!world_appears_running(root.path()).unwrap());
    }

    /// `F_GETLK` only reports locks held by *other* processes, so we spawn a
    /// helper that grabs an `fcntl` lock (the same kind Minecraft uses).
    #[test]
    #[cfg(unix)]
    fn locked_session_lock_means_running() {
        use std::io::BufRead;

        let root = tempdir().unwrap();
        let lock_path = root.path().join("session.lock");
        fs::write(&lock_path, b"lock").unwrap();

        let script = format!(
            "import fcntl, sys; f = open({:?}, 'r+b'); fcntl.lockf(f, fcntl.LOCK_EX); \
             sys.stdout.write('locked\\n'); sys.stdout.flush(); sys.stdin.readline()",
            lock_path.to_str().unwrap(),
        );
        let mut child = match std::process::Command::new("python3")
            .args(["-c", &script])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => {
                eprintln!("python3 not found – skipping test");
                return;
            }
        };

        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line.trim(), "locked");

        assert!(world_appears_running(root.path()).unwrap());

        drop(child.stdin.take());
        let _ = child.wait();
    }

    /// On Windows `LockFileEx` is shared between Java and fs2, so a
    /// same-process exclusive lock is visible to the detector.
    #[test]
    #[cfg(windows)]
    fn locked_session_lock_means_running() {
        use fs2::FileExt;
        use std::fs::OpenOptions;

        let root = tempdir().unwrap();
        let lock_path = root.path().join("session.lock");
        fs::write(&lock_path, b"lock").unwrap();

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        file.lock_exclusive().unwrap();

        assert!(world_appears_running(root.path()).unwrap());

        file.unlock().unwrap();
    }
}
