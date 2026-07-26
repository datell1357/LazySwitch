use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Write file atomically: temp write + rename, so no reader ever sees a
/// half-written file (auth.json, credentials.json, config.json, …).
pub fn atomic_write(dest: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = tmp_path(dest);
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, dest)
}

/// Copy then atomically rename into place, so `dest` is never observed
/// half-copied.
pub fn atomic_copy(src: &Path, dest: &Path) -> std::io::Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = tmp_path(dest);
    fs::copy(src, &tmp)?;
    fs::rename(&tmp, dest)
}

fn tmp_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    s.push(format!(".tmp-{}-{now}", std::process::id()));
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_creates_parent_and_writes_contents() {
        let dir = std::env::temp_dir().join(format!("lazyswitch-atomic-{}", std::process::id()));
        let dest = dir.join("nested").join("file.json");
        atomic_write(&dest, b"hello").unwrap();
        assert_eq!(fs::read_to_string(&dest).unwrap(), "hello");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_copy_copies_contents() {
        let dir = std::env::temp_dir().join(format!("lazyswitch-atomiccopy-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.json");
        let dest = dir.join("out").join("dest.json");
        fs::write(&src, b"copied").unwrap();
        atomic_copy(&src, &dest).unwrap();
        assert_eq!(fs::read_to_string(&dest).unwrap(), "copied");
        let _ = fs::remove_dir_all(&dir);
    }
}
