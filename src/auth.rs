//! Remembered login and logout, following learnX's model.
//!
//! learnX keeps a student signed in by remembering the password plus a random
//! device fingerprint that the student trusted once during two-factor
//! authentication; whenever the Learn session expires it logs in again over HTTP.
//! This module runs that one-time setup: it tries the saved device trust first,
//! falls back to the interactive browser flow, verifies that an HTTP login works,
//! and only then saves the credentials.

use anyhow::{anyhow, bail, Result};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::browser_login::{self, SsoInjection};
use crate::client::{is_definitive_rejection, Client};
use crate::credentials::{self, Credentials, Location};
use crate::sso::{self, SsoFailure};

/// How [`login`] finished.
#[derive(Debug)]
pub enum LoginOutcome {
    /// Logged in over HTTP with the saved device trust; no browser was needed.
    SavedTrust(Location),
    /// Logged in through the browser; HTTP re-login was verified and credentials saved.
    Browser(Location),
    /// The browser session works, but HTTP re-login failed, so nothing was saved and
    /// previously saved credentials were removed (`removed_saved`).
    BrowserSessionOnly { reason: String, removed_saved: bool },
}

/// Logs in with `username`/`password` and remembers them for silent re-login.
///
/// `saved` is the currently saved credential set, if any: its device fingerprint
/// is reused for the same username so repeated logins do not register new
/// trusted devices. `force_browser` skips the HTTP attempt with that trust.
/// `manual_continue` creates the browser's "I am done" signal.
pub async fn login(
    client: &Client,
    username: String,
    password: String,
    saved: Option<&Credentials>,
    force_browser: bool,
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

    if reusable.is_some() && !force_browser {
        progress("Signing in with this device's saved trust...");
        match client.login_with(&creds).await {
            Ok(()) => return remember(client, &creds).await.map(LoginOutcome::SavedTrust),
            // Sending the same password again (in Chrome or here) would only add failed attempts.
            Err(e)
                if matches!(
                    e.downcast_ref::<SsoFailure>(),
                    Some(SsoFailure::Rejected(_))
                ) =>
            {
                bail!(
                    "{e:#}. Check the password and run `thu-learn login` again; if THU asks \
                     for a captcha, run `thu-learn login --browser`."
                );
            }
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
    if !browser.reached_learn() {
        bail!(
            "Login did not complete: {}. If THU said the password was wrong, run \
             `thu-learn login` again and retype it.",
            browser.end.describe()
        );
    }
    if !browser.finger_gen_print.is_empty() {
        creds.finger_gen_print = browser.finger_gen_print.clone();
    }
    if !browser.finger_gen_print3.is_empty() {
        creds.finger_gen_print3 = browser.finger_gen_print3.clone();
    }

    progress("Checking that automatic re-login works...");
    match verify(client, &creds).await {
        Ok(()) => remember(client, &creds).await.map(LoginOutcome::Browser),
        Err(e) => {
            let mut reason = format!("{e:#}");
            if !browser.fingerprint_submitted {
                reason.push_str(
                    " (the login form was not seen using the CLI's device ID; the THU \
                     login page may have changed)",
                );
            }
            // Older saved credentials (another account, an old password, or the same
            // failing ones) must not be used once this browser session expires.
            client.set_credentials(None).await;
            let removed_saved = match forget_credentials().await {
                Ok(removed) => removed,
                Err(err) => {
                    progress(&format!(
                        "Warning: previously saved credentials could not be removed: {err:#}"
                    ));
                    false
                }
            };
            client.import_cookies(&browser.cookies)?;
            client.save_cookies()?;
            client.confirm_session().await.map_err(|err| {
                anyhow!(
                    "Logging in over HTTP failed ({reason}), and the browser cookies could \
                     not verify a session either: {err:#}"
                )
            })?;
            Ok(LoginOutcome::BrowserSessionOnly {
                reason,
                removed_saved,
            })
        }
    }
}

/// Logs in over HTTP, retrying network hiccups (not rejections) like learnX's `retry()`.
async fn verify(client: &Client, creds: &Credentials) -> Result<()> {
    let mut delay = Duration::from_secs(1);
    let mut attempt = 1;
    loop {
        match client.login_with(creds).await {
            Ok(()) => return Ok(()),
            Err(e) if attempt < 3 && !is_definitive_rejection(&e) => {
                tokio::time::sleep(delay).await;
                delay *= 2;
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Saves verified credentials and makes `client` renew with them.
async fn remember(client: &Client, creds: &Credentials) -> Result<Location> {
    let to_save = creds.clone();
    let saved = tokio::task::spawn_blocking(move || credentials::save(&to_save))
        .await
        .map_err(|e| anyhow!("Credential saving task failed: {e}"))?;
    match saved {
        Ok(location) => {
            client.use_saved_credentials().await;
            Ok(location)
        }
        Err(e) => {
            // The session works, but do not renew with whatever was saved before.
            client.set_credentials(None).await;
            Err(e.context("Logged in, but the credentials could not be saved"))
        }
    }
}

/// Removes saved credentials; returns whether any were stored.
pub async fn forget_credentials() -> Result<bool> {
    tokio::task::spawn_blocking(credentials::delete)
        .await
        .map_err(|e| anyhow!("Credential removal task failed: {e}"))?
}

/// Ends the Learn session and removes cookies, saved credentials, and cache.
///
/// Every step runs even if an earlier one fails. Returns whether saved
/// credentials were removed.
pub async fn logout(client: &Client) -> Result<bool> {
    let session = client.logout().await;
    let creds = forget_credentials().await;
    crate::cache::clear();
    match (session, creds) {
        (Ok(()), Ok(removed)) => Ok(removed),
        (Err(e), Ok(_)) | (Ok(()), Err(e)) => Err(e),
        (Err(a), Err(b)) => Err(anyhow!("{a:#}; {b:#}")),
    }
}
