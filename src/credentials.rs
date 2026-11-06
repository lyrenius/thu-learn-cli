//! Saved THU credentials for silent re-login.
//!
//! Like learnX (which keeps them in the iOS Keychain / Android Keystore through
//! expo-secure-store), the CLI stores the username, password, and the trusted
//! device fingerprint in the operating system credential store: macOS Keychain,
//! Windows Credential Manager, or the freedesktop Secret Service on Linux. Only
//! when no credential store exists at all does it fall back to an owner-only
//! JSON file; a locked or denied store is reported instead.
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
    /// `i_user`: THU username or student ID.
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
    /// The owner-only file; `fallback_reason` explains why the keyring was not used.
    File {
        path: PathBuf,
        fallback_reason: Option<String>,
    },
}

impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Location::Keyring => f.write_str(keyring_name()),
            Location::File { path, .. } => write!(f, "{}", path.display()),
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
        Ok(v) => parse_mode(&v),
    }
}

fn parse_mode(v: &str) -> Result<Mode> {
    match v.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => Ok(Mode::Auto),
        "keyring" | "keychain" => Ok(Mode::Keyring),
        "file" => Ok(Mode::File),
        other => bail!("{STORE_ENV} must be `auto`, `keyring`, or `file`, not `{other}`"),
    }
}

/// Fails early when `THU_LEARN_CREDENTIAL_STORE` has an invalid value.
pub fn check_config() -> Result<()> {
    mode().map(|_| ())
}

/// Loads saved credentials, preferring the system credential store.
pub fn load() -> Result<Option<Credentials>> {
    let file = paths::credentials_path();
    match mode()? {
        Mode::File => load_file(&file),
        Mode::Keyring => keyring_load().map_err(|e| e.into_anyhow("read")),
        Mode::Auto => match keyring_load() {
            Ok(Some(creds)) => Ok(Some(creds)),
            Ok(None) | Err(KeyringError::Unavailable(_)) => load_file(&file),
            // A locked or denied keyring: use a fallback file if one exists, otherwise report it.
            Err(e) => match load_file(&file)? {
                Some(creds) => Ok(Some(creds)),
                None => Err(e.into_anyhow("read")),
            },
        },
    }
}

/// Saves credentials and returns where they went.
pub fn save(creds: &Credentials) -> Result<Location> {
    let file = paths::credentials_path();
    match mode()? {
        Mode::File => {
            save_file(&file, creds)?;
            // Never keep a second, possibly stale copy in the keyring.
            let _ = keyring_delete();
            Ok(Location::File {
                path: file,
                fallback_reason: None,
            })
        }
        Mode::Keyring => {
            keyring_save(creds).map_err(|e| e.into_anyhow("write"))?;
            remove_file(&file)?;
            Ok(Location::Keyring)
        }
        Mode::Auto => match keyring_save(creds) {
            Ok(()) => {
                // Do not leave an older plaintext copy behind.
                remove_file(&file)?;
                Ok(Location::Keyring)
            }
            Err(KeyringError::Unavailable(reason)) => {
                save_file(&file, creds)?;
                Ok(Location::File {
                    path: file,
                    fallback_reason: Some(reason),
                })
            }
            Err(e) => Err(e.into_anyhow("write")),
        },
    }
}

/// Deletes credentials from every location; returns whether anything was removed.
///
/// The file is removed first. A keyring entry that cannot be removed is an error,
/// so callers never report credentials as deleted while a copy remains.
pub fn delete() -> Result<bool> {
    let mut removed = false;
    let file = paths::credentials_path();
    if file.exists() {
        remove_file(&file)?;
        removed = true;
    }
    match keyring_delete() {
        Ok(r) => removed |= r,
        // No credential store on this machine means nothing can be stored there.
        Err(KeyringError::Unavailable(_)) => {}
        Err(e) => return Err(e.into_anyhow("delete from")),
    }
    Ok(removed)
}

/// Async wrapper: system credential stores can block (for example on an unlock prompt).
pub async fn load_async() -> Result<Option<Credentials>> {
    tokio::task::spawn_blocking(load)
        .await
        .map_err(|e| anyhow!("Credential loading task failed: {e}"))?
}

/// Keyring failures, split by what the CLI should do about them.
#[derive(Debug)]
enum KeyringError {
    /// No usable credential store on this machine (none installed, no session bus, ...).
    Unavailable(String),
    /// The store exists but is locked, or access was denied.
    Denied(String),
    Other(String),
}

impl KeyringError {
    fn classify(e: keyring::Error) -> Self {
        match e {
            keyring::Error::NoDefaultStore | keyring::Error::PlatformFailure(_) => {
                KeyringError::Unavailable(e.to_string())
            }
            keyring::Error::NoStorageAccess(_) => KeyringError::Denied(e.to_string()),
            other => KeyringError::Other(other.to_string()),
        }
    }

    fn into_anyhow(self, action: &str) -> anyhow::Error {
        match self {
            KeyringError::Unavailable(e) | KeyringError::Other(e) => {
                anyhow!("Failed to {action} {}: {e}", keyring_name())
            }
            KeyringError::Denied(e) => anyhow!(
                "Could not {action} {} because it is locked or access was denied ({e}). \
                 Unlock it and try again, or set {STORE_ENV}=file to use an owner-only file.",
                keyring_name()
            ),
        }
    }
}

fn keyring_entry() -> std::result::Result<keyring::Entry, KeyringError> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT).map_err(KeyringError::classify)
}

fn keyring_load() -> std::result::Result<Option<Credentials>, KeyringError> {
    match keyring_entry()?.get_password() {
        Ok(json) => serde_json::from_str(&json).map(Some).map_err(|_| {
            KeyringError::Other("the saved credentials in the keyring are corrupt".into())
        }),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(KeyringError::classify(e)),
    }
}

fn keyring_save(creds: &Credentials) -> std::result::Result<(), KeyringError> {
    let json = serde_json::to_string(creds).map_err(|e| KeyringError::Other(e.to_string()))?;
    keyring_entry()?
        .set_password(&json)
        .map_err(KeyringError::classify)
}

fn keyring_delete() -> std::result::Result<bool, KeyringError> {
    match keyring_entry()?.delete_credential() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(e) => Err(KeyringError::classify(e)),
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

    #[test]
    fn store_mode_values_are_validated() {
        assert_eq!(parse_mode("").unwrap(), Mode::Auto);
        assert_eq!(parse_mode(" File ").unwrap(), Mode::File);
        assert_eq!(parse_mode("keychain").unwrap(), Mode::Keyring);
        let err = parse_mode("keychian").unwrap_err().to_string();
        assert!(err.contains("THU_LEARN_CREDENTIAL_STORE"));
    }

    #[test]
    fn only_missing_stores_allow_the_file_fallback() {
        assert!(matches!(
            KeyringError::classify(keyring::Error::NoDefaultStore),
            KeyringError::Unavailable(_)
        ));
        assert!(matches!(
            KeyringError::classify(keyring::Error::NoStorageAccess(Box::new(
                std::io::Error::other("locked")
            ))),
            KeyringError::Denied(_)
        ));
        let msg = KeyringError::Denied("locked".into())
            .into_anyhow("write")
            .to_string();
        assert!(msg.contains("THU_LEARN_CREDENTIAL_STORE=file"));
    }
}
