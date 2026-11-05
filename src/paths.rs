//! Cookie, credential, and cache paths, plus private file writes.

use directories::BaseDirs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn home_dir() -> PathBuf {
    BaseDirs::new()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .expect("Could not locate user home directory")
}

fn config_dir() -> PathBuf {
    home_dir().join(".config/thu-learn-cli")
}

fn cache_root() -> PathBuf {
    home_dir().join(".cache/thu-learn-cli")
}

pub fn cookie_path() -> PathBuf {
    config_dir().join("cookies.json")
}

/// Fallback credential file, used only when no system credential store is available.
pub fn credentials_path() -> PathBuf {
    config_dir().join("credentials.json")
}

/// Cache directory for slow-changing semester and course-list data.
pub fn cache_dir() -> PathBuf {
    cache_root()
}

/// Atomically replaces `path` with `bytes`, readable and writable only by the current user.
///
/// Writing to a temporary file and renaming it means concurrent saves and
/// interrupted processes never leave a truncated or interleaved file behind.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    create_private_dir(dir)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let tmp = dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        std::fs::remove_file(&tmp).ok();
    }
    result
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_path_uses_config_dir() {
        let home = home_dir();
        let path = cookie_path();

        assert!(path.is_absolute());
        assert!(path.starts_with(&home));
        assert!(path.ends_with(".config/thu-learn-cli/cookies.json"));
    }

    #[test]
    fn credentials_path_uses_config_dir() {
        let path = credentials_path();
        assert!(path.starts_with(home_dir()));
        assert!(path.ends_with(".config/thu-learn-cli/credentials.json"));
    }

    #[test]
    fn cache_dir_uses_cache_dir() {
        let home = home_dir();
        let path = cache_dir();

        assert!(path.is_absolute());
        assert!(path.starts_with(&home));
        assert!(path.ends_with(".cache/thu-learn-cli"));
        assert!(!path.ends_with("cookies.json"));
    }

    #[test]
    fn write_private_replaces_atomically_with_owner_only_mode() {
        let dir = std::env::temp_dir().join(format!("thu-learn-paths-test-{}", std::process::id()));
        let path = dir.join("a").join("secret.json");
        write_private(&path, b"first, longer content").unwrap();
        write_private(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // No temporary files are left behind.
        let leftovers = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .map(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(leftovers, 0);
        std::fs::remove_dir_all(&dir).ok();
    }
}
