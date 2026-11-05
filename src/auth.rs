//! Remembered login and logout, following learnX's model.
//!
//! learnX keeps a student signed in by remembering the password plus a random
//! device fingerprint that the student trusted once during two-factor
//! authentication; whenever the Learn session expires it logs in again over HTTP.
//! This module runs that one-time setup: it tries the saved device trust first,
//! falls back to the interactive browser flow, verifies that an HTTP login works,
//! and only then saves the credentials.

use anyhow::{anyhow, Result};
use tokio::sync::mpsc;

use crate::browser_login::{self, SsoInjection};
use crate::client::Client;
use crate::credentials::{self, Credentials, Location};
use crate::sso;

/// How [`login`] finished.
#[derive(Debug)]
pub enum LoginOutcome {
    /// Logged in over HTTP with the saved device trust; no browser was needed.
    SavedTrust(Location),
    /// Logged in through the browser; HTTP re-login was verified and credentials saved.
    Browser(Location),
    /// The browser session works, but HTTP re-login failed, so nothing was saved.
    BrowserSessionOnly { reason: String },
}

/// Logs in with `username`/`password` and remembers them for silent re-login.
///
/// `saved` is the currently saved credential set, if any: its device fingerprint
/// is reused for the same username so repeated logins do not register new
/// trusted devices. `manual_continue` creates the browser's "I am done" signal.
pub async fn login(
    client: &Client,
    username: String,
    password: String,
    saved: Option<&Credentials>,
    manual_continue: impl FnOnce() -> mpsc::Receiver<()>,
    progress: &(dyn Fn(&str) + Sync),
) -> Result<LoginOutcome> {
    let reusable = saved.filter(|c| c.username == username);
    let mut creds = Credentials {
        username,
        password,
        fingerprint: match reusable {
            Some(c) => c.fingerprint.clone(),
            None => sso::generate_fingerprint()?,
        },
        finger_gen_print: reusable
            .map(|c| c.finger_gen_print.clone())
            .unwrap_or_default(),
        finger_gen_print3: reusable
            .map(|c| c.finger_gen_print3.clone())
            .unwrap_or_default(),
    };

    if reusable.is_some() {
        progress("Signing in with this device's saved trust...");
        match client.login_with(&creds).await {
            Ok(()) => return Ok(LoginOutcome::SavedTrust(save(&creds).await?)),
            Err(e) => progress(&format!(
                "That did not work ({e:#}); opening Chrome instead."
            )),
        }
    }

    let injection = SsoInjection {
        username: creds.username.clone(),
        password: creds.password.clone(),
        fingerprint: creds.fingerprint.clone(),
        device_name: sso::device_name(),
    };
    let browser = browser_login::run(Some(&injection), manual_continue(), progress).await?;
    if !browser.finger_gen_print.is_empty() {
        creds.finger_gen_print = browser.finger_gen_print.clone();
    }
    if !browser.finger_gen_print3.is_empty() {
        creds.finger_gen_print3 = browser.finger_gen_print3.clone();
    }

    progress("Checking that automatic re-login works...");
    match client.login_with(&creds).await {
        Ok(()) => Ok(LoginOutcome::Browser(save(&creds).await?)),
        Err(e) => {
            let mut reason = format!("{e:#}");
            if !browser.fingerprint_submitted {
                reason.push_str(
                    " (the login form was not seen using the CLI's device ID; the Tsinghua \
                     login page may have changed)",
                );
            }
            if browser.cookies.is_empty() {
                return Err(anyhow!(
                    "No cookies were captured and logging in over HTTP failed: {reason}. \
                     Login may be incomplete or timed out; try again."
                ));
            }
            // Fall back to the browser's own session, without silent re-login.
            client.set_credentials(None).await;
            client.import_cookies(&browser.cookies)?;
            client.save_cookies()?;
            client.confirm_session().await.map_err(|err| {
                anyhow!(
                    "Logging in over HTTP failed ({reason}), and the browser cookies could \
                     not verify a session either: {err:#}"
                )
            })?;
            Ok(LoginOutcome::BrowserSessionOnly { reason })
        }
    }
}

async fn save(creds: &Credentials) -> Result<Location> {
    let creds = creds.clone();
    tokio::task::spawn_blocking(move || credentials::save(&creds))
        .await
        .map_err(|e| anyhow!("Credential saving task failed: {e}"))?
}

/// Removes saved credentials; returns whether any were stored.
pub async fn forget_credentials() -> Result<bool> {
    tokio::task::spawn_blocking(credentials::delete)
        .await
        .map_err(|e| anyhow!("Credential removal task failed: {e}"))?
}

/// Ends the Learn session and removes cookies, saved credentials, and cache.
///
/// Returns whether saved credentials were removed.
pub async fn logout(client: &Client) -> Result<bool> {
    client.logout().await?;
    let removed = forget_credentials().await?;
    crate::cache::clear();
    Ok(removed)
}
