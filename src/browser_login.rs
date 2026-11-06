//! Browser login through Chrome WebDriver, followed by session cookie capture.
//!
//! Chrome handles everything interactive on `id.tsinghua.edu.cn`: captcha,
//! two-factor authentication, and device checks. For a remembered login, an
//! injected script ([`sso_inject.js`](../sso_inject.js)) does what learnX's SSO
//! WebView does: it pre-fills the credentials typed in the terminal, submits the
//! form with the CLI's random device fingerprint, and answers the two-factor
//! "trust this device" request with that fingerprint. Later logins with the same
//! fingerprint can then run over plain HTTP ([`crate::sso`]).
//!
//! The login session is stored in httpOnly cookies, so JavaScript cannot read
//! it; WebDriver's `get_all_cookies` can retrieve it through the driver protocol.
//!
//! `WebDriver::managed` downloads and starts a matching chromedriver, while a
//! local Google Chrome installation is still required.

use anyhow::{Context, Result};
use std::time::{Duration, Instant};
use thirtyfour::error::WebDriverErrorInner;
use thirtyfour::prelude::*;
use tokio::sync::mpsc;

/// Start page for cookie-only login, which redirects to unified authentication when needed.
const LEARN_HOME: &str = "https://learn.tsinghua.edu.cn/f/wlxt/index/course/student/";
/// Unified authentication host.
const AUTH_HOST: &str = "id.tsinghua.edu.cn";
/// The only origin the injected script acts on (https only).
const AUTH_ORIGIN: &str = "https://id.tsinghua.edu.cn";
/// Cookie in which the injected script also leaves its record, so it can be read
/// even when the login check redirects straight to Learn.
const RECORD_COOKIE: &str = "thu_learn_cli_sso";
/// Learn host reached after successful login.
const LEARN_HOST: &str = "learn.tsinghua.edu.cn";

const POLL_INTERVAL_MS: u64 = 500;
const MAX_WAIT_MS: u64 = 300_000; // 5 minutes.

/// Script injected into every page of the login tab; it only acts on the ID origin.
const INJECT_JS: &str = include_str!("sso_inject.js");

/// Reads what the injected script recorded, plus the live page-generated fingerprints.
const CAPTURE_JS: &str = r#"
const out = { saved: {} };
try { out.saved = JSON.parse(window.localStorage.getItem(arguments[0]) || '{}') || {}; } catch (e) {}
const field = (name) => {
  const el = document.querySelector('[name="' + name + '"]');
  return el && el.value ? String(el.value) : '';
};
out.fingerGenPrint = field('fingerGenPrint');
out.fingerGenPrint3 = field('fingerGenPrint3');
return out;
"#;

/// Captured cookie tuple: name, value, domain, and path.
pub type RawCookie = (String, String, String, String);

/// Credentials and device identity to inject into the THU login page.
pub struct SsoInjection {
    pub username: String,
    pub password: String,
    /// Random device fingerprint submitted as `fingerPrint` and trusted during two-factor authentication.
    pub fingerprint: String,
    /// Name shown in the THU trusted-device list.
    pub device_name: String,
}

/// How waiting for the browser login ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserEnd {
    /// Chrome reached Learn after unified authentication.
    ReachedLearn,
    /// The user pressed Enter before Learn was reached.
    Confirmed,
    /// The Chrome window or driver went away.
    Closed,
    /// Nothing happened before the time limit.
    TimedOut,
}

/// What a browser login produced.
#[derive(Debug)]
pub struct BrowserLogin {
    pub end: BrowserEnd,
    pub cookies: Vec<RawCookie>,
    /// `fingerGenPrint` from the login form, when observed.
    pub finger_gen_print: String,
    /// `fingerGenPrint3` from the login form, when observed.
    pub finger_gen_print3: String,
    /// Whether the login form was seen being submitted with the injected fingerprint.
    pub fingerprint_submitted: bool,
    /// Whether the two-factor "trust this device" request was answered with it.
    pub device_trusted: bool,
}

impl BrowserEnd {
    /// Why a login that did not reach Learn stopped.
    pub fn describe(self) -> &'static str {
        match self {
            BrowserEnd::ReachedLearn => "Learn was reached",
            BrowserEnd::Confirmed => "Enter was pressed before Learn loaded",
            BrowserEnd::Closed => "Chrome was closed before the login reached Learn",
            BrowserEnd::TimedOut => "the login did not reach Learn within 5 minutes",
        }
    }
}

impl BrowserLogin {
    fn new() -> Self {
        Self {
            end: BrowserEnd::Closed,
            cookies: Vec::new(),
            finger_gen_print: String::new(),
            finger_gen_print3: String::new(),
            fingerprint_submitted: false,
            device_trusted: false,
        }
    }

    /// Whether the login actually finished on Learn.
    pub fn reached_learn(&self) -> bool {
        self.end == BrowserEnd::ReachedLearn
    }
}

/// Opens Chrome, waits for login, and returns captured session cookies.
///
/// Nothing is injected, so this only yields a browser session that expires as usual.
pub async fn login_via_browser() -> Result<Vec<RawCookie>> {
    let progress = |msg: &str| eprintln!("{msg}");
    let login = run(None, stdin_enter_signal(), &progress).await?;
    if !login.reached_learn() {
        anyhow::bail!("Login did not complete: {}.", login.end.describe());
    }
    Ok(login.cookies)
}

/// Turns the next line typed on stdin into a "continue" signal for [`run`].
pub fn stdin_enter_signal() -> mpsc::Receiver<()> {
    let (tx, rx) = mpsc::channel::<()>(1);
    std::thread::spawn(move || {
        let mut line = String::new();
        // EOF (for example, stdin redirected from /dev/null) is not a confirmation.
        if matches!(std::io::stdin().read_line(&mut line), Ok(n) if n > 0) {
            let _ = tx.blocking_send(());
        }
    });
    rx
}

/// Opens Chrome on the THU login page and waits until Learn is reached.
///
/// With `injection`, the ID login page is opened directly and patched as described
/// in the module docs. `manual_continue` lets the user finish waiting early, and
/// `progress` receives human-readable status lines.
pub async fn run(
    injection: Option<&SsoInjection>,
    manual_continue: mpsc::Receiver<()>,
    progress: &(dyn Fn(&str) + Sync),
) -> Result<BrowserLogin> {
    let mut caps = DesiredCapabilities::chrome();
    // Leave JavaScript dialogs (captcha errors, "code sent" notices) for the user instead of
    // letting WebDriver dismiss them while the login is being polled.
    caps.set("unhandledPromptBehavior", "ignore")?;
    // managed resolves, downloads, starts, and later cleans up chromedriver.
    let driver = WebDriver::managed(caps)
        .await
        .context("Failed to start managed chromedriver; make sure Google Chrome is installed")?;

    let result = drive(&driver, injection, manual_continue, progress).await;
    driver.quit().await.ok();
    result
}

async fn drive(
    driver: &WebDriver,
    injection: Option<&SsoInjection>,
    manual_continue: mpsc::Receiver<()>,
    progress: &(dyn Fn(&str) + Sync),
) -> Result<BrowserLogin> {
    let mut storage_key = None;
    let start_url = match injection {
        Some(injection) => {
            let key = format!("thu-learn-cli-sso-{}", injection.fingerprint);
            driver
                .cdp()
                .send_raw(
                    "Page.addScriptToEvaluateOnNewDocument",
                    serde_json::json!({ "source": injection_script(injection, &key) }),
                )
                .await
                .context("Failed to prepare the THU login page in Chrome")?;
            storage_key = Some(key);
            crate::client::Endpoints::default().id_login_form()
        }
        None => LEARN_HOME.to_string(),
    };

    driver
        .goto(&start_url)
        .await
        .context("Failed to open the THU login page")?;

    progress("A Chrome window is open. Complete THU authentication there.");
    if injection.is_some() {
        progress(
            "Your username and password are filled in. Finish any captcha or two-factor \
             check; the CLI answers \"trust this device\" for you.",
        );
    }
    progress(
        "After the Learn home page loads, this command continues automatically. \
         If it does not, return here and press Enter.",
    );

    Ok(wait_for_login(driver, storage_key.as_deref(), manual_continue).await)
}

/// Builds the injected script with its configuration embedded as a JSON literal.
fn injection_script(injection: &SsoInjection, storage_key: &str) -> String {
    let config = serde_json::json!({
        "idOrigin": AUTH_ORIGIN,
        "recordCookie": RECORD_COOKIE,
        "username": injection.username,
        "password": injection.password,
        "fingerprint": injection.fingerprint,
        "deviceName": injection.device_name,
        "storageKey": storage_key,
    });
    // JSON is a valid JavaScript expression, so every value is safely quoted.
    INJECT_JS.replace("__THU_LEARN_CONFIG__", &config.to_string())
}

/// Waits for the authentication redirect cycle, with Enter as a manual fallback.
async fn wait_for_login(
    driver: &WebDriver,
    storage_key: Option<&str>,
    mut manual_continue: mpsc::Receiver<()>,
) -> BrowserLogin {
    let mut outcome = BrowserLogin::new();
    let mut seen_auth = false;
    let deadline = Instant::now() + Duration::from_millis(MAX_WAIT_MS);

    loop {
        tokio::select! {
            // Manual Enter fallback; it only counts as finished when Chrome shows Learn.
            Some(()) = manual_continue.recv() => {
                let on_learn = matches!(
                    driver.current_url().await,
                    Ok(u) if u.host_str() == Some(LEARN_HOST)
                );
                let end = if on_learn { BrowserEnd::ReachedLearn } else { BrowserEnd::Confirmed };
                return finish(driver, outcome, end).await;
            }
            // Poll the current URL periodically.
            _ = tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)) => {
                match driver.current_url().await {
                    Ok(u) => {
                        if u.host_str() == Some(AUTH_HOST) {
                            // The flow really reached the authentication page.
                            seen_auth = true;
                            if let Some(key) = storage_key {
                                if let Ok(ret) = driver.execute(CAPTURE_JS, vec![key.into()]).await {
                                    merge_capture(&mut outcome, ret.json());
                                }
                            }
                        } else if seen_auth && u.host_str() == Some(LEARN_HOST) {
                            // After returning to Learn, wait briefly for cookies to settle.
                            tokio::time::sleep(Duration::from_millis(800)).await;
                            return finish(driver, outcome, BrowserEnd::ReachedLearn).await;
                        }
                    }
                    Err(e) if browser_gone(&e) => {
                        return finish(driver, outcome, BrowserEnd::Closed).await;
                    }
                    // A dialog is open or a page is loading; keep waiting.
                    Err(_) => {}
                }
                if Instant::now() >= deadline {
                    return finish(driver, outcome, BrowserEnd::TimedOut).await;
                }
            }
        }
    }
}

/// Whether a WebDriver error means the window or the driver is gone, rather than a
/// dialog or a page that is still loading.
fn browser_gone(e: &WebDriverError) -> bool {
    matches!(
        &**e,
        WebDriverErrorInner::NoSuchWindow(_)
            | WebDriverErrorInner::InvalidSessionId(_)
            | WebDriverErrorInner::SessionNotCreated(_)
            | WebDriverErrorInner::RequestFailed(_)
            | WebDriverErrorInner::HttpError(_)
            | WebDriverErrorInner::IoError(_)
            | WebDriverErrorInner::CommandSendError(_)
            | WebDriverErrorInner::CommandRecvError(_)
            | WebDriverErrorInner::FatalError(_)
    )
}

/// Captures cookies, reads the injected script's cookie record, and records how the wait ended.
async fn finish(driver: &WebDriver, mut outcome: BrowserLogin, end: BrowserEnd) -> BrowserLogin {
    let mut cookies = capture_cookies(driver).await;
    if let Some(record) = cookies
        .iter()
        .rev()
        .find(|(name, ..)| name == RECORD_COOKIE)
        .and_then(|(_, value, ..)| decode_record_cookie(value))
    {
        merge_capture(&mut outcome, &serde_json::json!({ "saved": record }));
    }
    // The record is only for the CLI; it is not part of any session.
    cookies.retain(|(name, ..)| name != RECORD_COOKIE);
    outcome.cookies = cookies;
    outcome.end = end;
    outcome
}

/// Decodes the `encodeURIComponent(JSON.stringify(record))` cookie value.
fn decode_record_cookie(value: &str) -> Option<serde_json::Value> {
    // encodeURIComponent never emits a bare `+`, so form decoding is plain percent decoding here.
    let decoded = url::form_urlencoded::parse(format!("v={value}").as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())?;
    serde_json::from_str(&decoded).ok()
}

/// Merges one [`CAPTURE_JS`] result; values recorded at submit time win over live ones,
/// and the latest live value is kept while nothing was recorded.
fn merge_capture(outcome: &mut BrowserLogin, value: &serde_json::Value) {
    let text = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
    let saved = &value["saved"];
    if saved["submitted"].as_bool() == Some(true) {
        outcome.fingerprint_submitted = true;
    }
    if saved["deviceTrusted"].as_bool() == Some(true) {
        outcome.device_trusted = true;
    }
    for (target, saved_key, live_key) in [
        (
            &mut outcome.finger_gen_print,
            "fingerGenPrint",
            "fingerGenPrint",
        ),
        (
            &mut outcome.finger_gen_print3,
            "fingerGenPrint3",
            "fingerGenPrint3",
        ),
    ] {
        let saved_value = text(&saved[saved_key]);
        let live_value = text(&value[live_key]);
        if !saved_value.is_empty() {
            *target = saved_value;
        } else if !live_value.is_empty() {
            *target = live_value;
        }
    }
}

async fn capture_cookies(driver: &WebDriver) -> Vec<RawCookie> {
    if let Ok(value) = driver.cdp().send_raw("Network.getAllCookies", ()).await {
        let cookies = cdp_cookies_to_raw(&value);
        if !cookies.is_empty() {
            return cookies;
        }
    }
    driver
        .get_all_cookies()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(webdriver_cookie_to_raw)
        .collect()
}

fn webdriver_cookie_to_raw(c: Cookie) -> RawCookie {
    (
        c.name,
        c.value,
        c.domain.unwrap_or_else(|| LEARN_HOST.into()),
        c.path.unwrap_or_else(|| "/".into()),
    )
}

fn cdp_cookies_to_raw(value: &serde_json::Value) -> Vec<RawCookie> {
    value
        .get("cookies")
        .and_then(|cookies| cookies.as_array())
        .into_iter()
        .flatten()
        .filter_map(|c| {
            Some((
                c.get("name")?.as_str()?.to_string(),
                c.get("value")?.as_str()?.to_string(),
                c.get("domain")
                    .and_then(|d| d.as_str())
                    .unwrap_or(LEARN_HOST)
                    .to_string(),
                c.get("path")
                    .and_then(|p| p.as_str())
                    .unwrap_or("/")
                    .to_string(),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn injection() -> SsoInjection {
        SsoInjection {
            username: "2025012345".into(),
            password: "a'b\"c\\d\n</script>\u{2028}".into(),
            fingerprint: "0123456789abcdef0123456789abcdef".into(),
            device_name: "macOS,thu-learn-cli/0.1.0".into(),
        }
    }

    #[test]
    fn cdp_cookie_capture_keeps_cross_domain_cookies() {
        let value = serde_json::json!({
            "cookies": [
                {
                    "name": "LEARN",
                    "value": "learn-session",
                    "domain": "learn.tsinghua.edu.cn",
                    "path": "/"
                },
                {
                    "name": "JSESSIONID",
                    "value": "id-session",
                    "domain": "id.tsinghua.edu.cn",
                    "path": "/"
                }
            ]
        });

        let cookies = cdp_cookies_to_raw(&value);

        assert_eq!(cookies.len(), 2);
        assert!(cookies
            .iter()
            .any(|(_, _, domain, _)| domain == "learn.tsinghua.edu.cn"));
        assert!(cookies
            .iter()
            .any(|(_, _, domain, _)| domain == "id.tsinghua.edu.cn"));
    }

    #[test]
    fn injection_script_embeds_config_as_json() {
        let inj = injection();
        let script = injection_script(&inj, "thu-learn-cli-sso-key");

        assert!(!script.contains("__THU_LEARN_CONFIG__"));
        let start = script.find("const config = ").unwrap() + "const config = ".len();
        let end = start + script[start..].find(";\n").unwrap();
        let config: serde_json::Value = serde_json::from_str(&script[start..end]).unwrap();
        assert_eq!(config["password"], inj.password.as_str());
        assert_eq!(config["username"], inj.username.as_str());
        assert_eq!(config["fingerprint"], inj.fingerprint.as_str());
        assert_eq!(config["deviceName"], inj.device_name.as_str());
        assert_eq!(config["idOrigin"], "https://id.tsinghua.edu.cn");
        assert_eq!(config["recordCookie"], "thu_learn_cli_sso");
        assert_eq!(config["storageKey"], "thu-learn-cli-sso-key");
        // The raw password never appears unquoted in the script.
        assert!(!script.contains("a'b\"c"));
    }

    #[test]
    fn capture_prefers_values_recorded_at_submit_time() {
        let mut outcome = BrowserLogin::new();
        merge_capture(
            &mut outcome,
            &serde_json::json!({"saved": {}, "fingerGenPrint": "live1", "fingerGenPrint3": ""}),
        );
        assert_eq!(outcome.finger_gen_print, "live1");
        assert!(!outcome.fingerprint_submitted);

        merge_capture(
            &mut outcome,
            &serde_json::json!({
                "saved": {"submitted": true, "deviceTrusted": true,
                          "fingerGenPrint": "sub1", "fingerGenPrint3": "sub3"},
                "fingerGenPrint": "other", "fingerGenPrint3": "other3"
            }),
        );
        assert_eq!(outcome.finger_gen_print, "sub1");
        assert_eq!(outcome.finger_gen_print3, "sub3");
        assert!(outcome.fingerprint_submitted);
        assert!(outcome.device_trusted);

        // Later pages without the form do not erase what was captured.
        merge_capture(
            &mut outcome,
            &serde_json::json!({"saved": {"submitted": true}}),
        );
        assert_eq!(outcome.finger_gen_print, "sub1");
    }

    #[test]
    fn live_values_fill_in_when_the_record_has_none() {
        let mut outcome = BrowserLogin::new();
        merge_capture(
            &mut outcome,
            &serde_json::json!({"saved": {"submitted": true, "fingerGenPrint": ""},
                                "fingerGenPrint": "GEN1", "fingerGenPrint3": "GEN3"}),
        );
        assert_eq!(outcome.finger_gen_print, "GEN1");
        assert_eq!(outcome.finger_gen_print3, "GEN3");
    }

    #[test]
    fn record_cookie_is_decoded() {
        // encodeURIComponent(JSON.stringify({submitted: true, fingerGenPrint: "a b+c"}))
        let value = "%7B%22submitted%22%3Atrue%2C%22fingerGenPrint%22%3A%22a%20b%2Bc%22%7D";
        let record = decode_record_cookie(value).unwrap();
        assert_eq!(record["submitted"], true);
        assert_eq!(record["fingerGenPrint"], "a b+c");
        assert!(decode_record_cookie("not-json").is_none());
    }
}
