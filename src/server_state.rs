use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result};
use fs2::FileExt;

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

    match file.try_lock_exclusive() {
        Ok(()) => {
            file.unlock().with_context(|| {
                format!(
                    "failed to release temporary lock on '{}'",
                    lock_path.display()
                )
            })?;
            Ok(false)
        }
        Err(error) if is_lock_contention(&error) => Ok(true),
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to inspect Minecraft lock state for '{}'",
                lock_path.display()
            )
        }),
    }
}

fn is_lock_contention(error: &std::io::Error) -> bool {
    error.kind() == ErrorKind::WouldBlock || matches!(error.raw_os_error(), Some(11 | 33 | 35))
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};

    use fs2::FileExt;
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

    #[test]
    fn locked_session_lock_means_running() {
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
