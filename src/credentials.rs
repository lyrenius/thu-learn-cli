//! Saved Tsinghua credentials for silent re-login.
//!
//! Like learnX (which keeps them in the iOS Keychain / Android Keystore through
//! expo-secure-store), the CLI stores the username, password, and the trusted
//! device fingerprint in the operating system credential store: macOS Keychain,
//! Windows Credential Manager, or the freedesktop Secret Service on Linux. When
//! no credential store is available, it falls back to an owner-only JSON file.
//!
//! Set `THU_LEARN_CREDENTIAL_STORE=file` to always use the file, or `keyring` to
//! require the system credential store.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::paths;

const KEYRING_SERVICE: &str = "thu-learn-cli";
const KEYRING_ACCOUNT: &str = "sso";
const STORE_ENV: &str = "THU_LEARN_CREDENTIAL_STORE";

/// Everything needed to log in to `id.tsinghua.edu.cn` without a browser.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    /// `i_user`: Tsinghua username or student ID.
    pub username: String,
    /// Account password, sent SM2-encrypted as `i_pass`.
    pub password: String,
    /// `fingerPrint`: random device ID that was trusted during two-factor authentication.
    pub fingerprint: String,
    /// `fingerGenPrint`: page-generated fingerprint captured from the browser login form.
    #[serde(default)]
    pub finger_gen_print: String,
    /// `fingerGenPrint3`: second page-generated fingerprint captured from the login form.
    #[serde(default)]
    pub finger_gen_print3: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("fingerprint", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// Where credentials were saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// The operating system credential store.
    Keyring,
    /// The owner-only fallback file.
    File(PathBuf),
}

impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Location::Keyring => f.write_str(keyring_name()),
            Location::File(path) => write!(f, "{}", path.display()),
        }
    }
}

fn keyring_name() -> &'static str {
    match std::env::consts::OS {
        "macos" => "the macOS Keychain",
        "windows" => "Windows Credential Manager",
        _ => "the system keyring (Secret Service)",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Auto,
    Keyring,
    File,
}

fn mode() -> Result<Mode> {
    match std::env::var(STORE_ENV) {
        Err(_) => Ok(Mode::Auto),
        Ok(v) => match v.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Ok(Mode::Auto),
            "keyring" | "keychain" => Ok(Mode::Keyring),
            "file" => Ok(Mode::File),
            other => bail!("{STORE_ENV} must be `auto`, `keyring`, or `file`, not `{other}`"),
        },
    }
}

/// Loads saved credentials, preferring the system credential store.
pub fn load() -> Result<Option<Credentials>> {
    let file = paths::credentials_path();
    match mode()? {
        Mode::File => load_file(&file),
        Mode::Keyring => keyring_load(),
        Mode::Auto => match keyring_load() {
            Ok(Some(creds)) => Ok(Some(creds)),
            // Not in the keyring, or no usable keyring: try the fallback file.
            Ok(None) | Err(_) => load_file(&file),
        },
    }
}

/// Saves credentials and returns where they went.
pub fn save(creds: &Credentials) -> Result<Location> {
    let file = paths::credentials_path();
    match mode()? {
        Mode::File => {
            save_file(&file, creds)?;
            Ok(Location::File(file))
        }
        Mode::Keyring => {
            keyring_save(creds)?;
            remove_file(&file)?;
            Ok(Location::Keyring)
        }
        Mode::Auto => match keyring_save(creds) {
            Ok(()) => {
                // Do not leave an older plaintext copy behind.
                remove_file(&file)?;
                Ok(Location::Keyring)
            }
            Err(_) => {
                save_file(&file, creds)?;
                Ok(Location::File(file))
            }
        },
    }
}

/// Deletes credentials from every location; returns whether anything was removed.
pub fn delete() -> Result<bool> {
    let mut removed = false;
    if mode()? != Mode::File {
        match keyring_delete() {
            Ok(r) => removed |= r,
            Err(e) if mode()? == Mode::Keyring => return Err(e),
            Err(_) => {}
        }
    }
    let file = paths::credentials_path();
    if file.exists() {
        remove_file(&file)?;
        removed = true;
    }
    Ok(removed)
}

/// Async wrapper: system credential stores can block (for example on an unlock prompt).
pub async fn load_async() -> Result<Option<Credentials>> {
    tokio::task::spawn_blocking(load)
        .await
        .map_err(|e| anyhow!("Credential loading task failed: {e}"))?
}

fn keyring_entry() -> Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)
        .map_err(|e| anyhow!("System credential store is unavailable: {e}"))
}

fn keyring_load() -> Result<Option<Credentials>> {
    match keyring_entry()?.get_password() {
        Ok(json) => Ok(Some(
            serde_json::from_str(&json).context("Saved credentials in the keyring are corrupt")?,
        )),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(anyhow!("Failed to read the system credential store: {e}")),
    }
}

fn keyring_save(creds: &Credentials) -> Result<()> {
    let json = serde_json::to_string(creds)?;
    keyring_entry()?
        .set_password(&json)
        .map_err(|e| anyhow!("Failed to write the system credential store: {e}"))
}

fn keyring_delete() -> Result<bool> {
    match keyring_entry()?.delete_credential() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(e) => Err(anyhow!(
            "Failed to delete from the system credential store: {e}"
        )),
    }
}

fn load_file(path: &Path) -> Result<Option<Credentials>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
            format!("Saved credentials are corrupt: {}", path.display())
        })?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("Failed to read {}", path.display())),
    }
}

fn save_file(path: &Path, creds: &Credentials) -> Result<()> {
    let json = serde_json::to_vec_pretty(creds)?;
    paths::write_private(path, &json).with_context(|| format!("Failed to write {}", path.display()))
}

fn remove_file(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("Failed to remove {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Credentials {
        Credentials {
            username: "2025012345".into(),
            password: "hunter2-secret".into(),
            fingerprint: "0123456789abcdef0123456789abcdef".into(),
            finger_gen_print: "gen".into(),
            finger_gen_print3: String::new(),
        }
    }

    #[test]
    fn debug_output_redacts_secrets() {
        let s = format!("{:?}", sample());
        assert!(s.contains("2025012345"));
        assert!(!s.contains("hunter2-secret"));
        assert!(!s.contains("0123456789abcdef"));
    }

    #[test]
    fn file_store_roundtrip_is_owner_only() {
        let dir = std::env::temp_dir().join(format!(
            "thu-learn-cred-test-{}-{}",
            std::process::id(),
            line!()
        ));
        let path = dir.join("nested").join("credentials.json");
        assert!(load_file(&path).unwrap().is_none());

        save_file(&path, &sample()).unwrap();
        assert_eq!(load_file(&path).unwrap(), Some(sample()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        remove_file(&path).unwrap();
        assert!(load_file(&path).unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn older_files_without_generated_prints_still_load() {
        let json = r#"{"username":"u","password":"p","fingerprint":"f"}"#;
        let creds: Credentials = serde_json::from_str(json).unwrap();
        assert_eq!(creds.finger_gen_print, "");
        assert_eq!(creds.finger_gen_print3, "");
    }
}
