//! Tsinghua Learn HTTP client: session reuse, CSRF handling, and silent re-login.
//!
//! Learn sessions live on the server and expire after a period of inactivity,
//! so saved cookies alone keep a login alive only briefly. Like learnX (through
//! thu-learn-lib), this client renews an expired session by logging in to
//! `id.tsinghua.edu.cn` again over plain HTTP with the saved credentials and the
//! trusted device fingerprint ([`crate::sso`]), then retries the request once.
//! Browser login is handled by [`crate::browser_login`].

use anyhow::{anyhow, bail, Context, Result};
use reqwest::multipart::Form;
use reqwest::{Client as HttpClient, RequestBuilder, Response, StatusCode};
use reqwest_cookie_store::{CookieStore, CookieStoreMutex};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::credentials::Credentials;
use crate::sso::{self, SsoFailure};

const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/133.0.0.0 Safari/537.36";

/// Learn and unified-authentication origins. Tests point these at local mock servers.
#[derive(Debug, Clone)]
pub struct Endpoints {
    /// Learn origin, such as `https://learn.tsinghua.edu.cn`.
    pub learn: String,
    /// Unified authentication origin, such as `https://id.tsinghua.edu.cn`.
    pub id: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            learn: "https://learn.tsinghua.edu.cn".into(),
            id: "https://id.tsinghua.edu.cn".into(),
        }
    }
}

impl Endpoints {
    fn course_list_page(&self) -> String {
        format!("{}/f/wlxt/index/course/student/", self.learn)
    }

    /// Learn's login form on unified authentication (the page learnX's SSO WebView opens).
    pub fn id_login_form(&self) -> String {
        format!(
            "{}/do/off/ui/auth/login/form/{}/0",
            self.id,
            sso::LEARN_APP_ID
        )
    }

    fn id_login_check(&self) -> String {
        format!("{}/do/off/ui/auth/login/check", self.id)
    }

    fn roaming(&self, ticket: &str) -> Result<url::Url> {
        let mut url = url::Url::parse(&format!(
            "{}/b/j_spring_security_thauth_roaming_entry",
            self.learn
        ))?;
        url.query_pairs_mut().append_pair("ticket", ticket);
        Ok(url)
    }

    fn logout(&self) -> String {
        format!("{}/f/j_spring_security_logout", self.learn)
    }

    fn same_origin(base: &str, url: &url::Url) -> bool {
        url::Url::parse(base)
            .map(|b| b.origin() == url.origin())
            .unwrap_or(false)
    }
}

/// Authentication problems that need the user to log in again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// The session expired and no credentials are saved for silent re-login.
    SessionExpired,
    /// Saved credentials exist, but logging in with them failed.
    ReloginFailed(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::SessionExpired => write!(
                f,
                "Not logged in or the session has expired. Run `thu-learn login` first."
            ),
            AuthError::ReloginFailed(reason) => write!(
                f,
                "The session expired and automatic re-login failed: {reason}. \
                 Run `thu-learn login` to sign in again."
            ),
        }
    }
}

impl std::error::Error for AuthError {}

enum CredentialSlot {
    /// Read the saved credentials the first time a re-login is needed.
    Unloaded,
    Loaded(Option<Credentials>),
}

struct Session {
    csrf: Option<String>,
    /// Incremented whenever a different session or `_csrf` token is installed.
    generation: u64,
    /// Why re-login failed for `generation`; concurrent requests reuse it instead of
    /// hammering `id.tsinghua.edu.cn` (repeated failures can trigger a captcha).
    relogin_error: Option<String>,
}

pub struct Client {
    http: HttpClient,
    cookie_store: Arc<CookieStoreMutex>,
    cookie_path: PathBuf,
    endpoints: Endpoints,
    session: tokio::sync::Mutex<Session>,
    credentials: tokio::sync::Mutex<CredentialSlot>,
    save_lock: std::sync::Mutex<()>,
}

impl Client {
    /// Builds a client and loads saved cookies from disk when present.
    ///
    /// Saved credentials are only read when the session turns out to be expired.
    pub fn new(cookie_path: PathBuf) -> Result<Self> {
        Self::with_endpoints(cookie_path, Endpoints::default())
    }

    /// Builds a client for custom origins.
    pub fn with_endpoints(cookie_path: PathBuf, endpoints: Endpoints) -> Result<Self> {
        let store = load_cookie_store(&cookie_path);
        let cookie_store = Arc::new(CookieStoreMutex::new(store));

        let http = HttpClient::builder()
            .user_agent(UA)
            .cookie_provider(cookie_store.clone())
            .build()
            .context("Failed to build HTTP client")?;

        Ok(Self {
            http,
            cookie_store,
            cookie_path,
            endpoints,
            session: tokio::sync::Mutex::new(Session {
                csrf: None,
                generation: 0,
                relogin_error: None,
            }),
            credentials: tokio::sync::Mutex::new(CredentialSlot::Unloaded),
            save_lock: std::sync::Mutex::new(()),
        })
    }

    /// Uses `creds` for silent re-login instead of the saved credentials; `None` disables it.
    pub async fn set_credentials(&self, creds: Option<Credentials>) {
        *self.credentials.lock().await = CredentialSlot::Loaded(creds);
        self.session.lock().await.relogin_error = None;
    }

    /// Confirms that the session works, renewing it with saved credentials when it expired.
    pub async fn confirm_session(&self) -> Result<()> {
        let observed = self.session.lock().await.generation;
        match self.fetch_csrf().await? {
            Some(token) => {
                self.install_csrf(token).await;
                self.save_cookies()
            }
            None => self.renew_session(observed).await,
        }
    }

    /// Logs in over HTTP with `creds` now, replacing the current session.
    ///
    /// Errors keep their cause (for example [`SsoFailure`]) for callers to inspect.
    pub async fn login_with(&self, creds: &Credentials) -> Result<()> {
        let mut session = self.session.lock().await;
        let token = self.sso_login(creds).await?;
        session.csrf = Some(token);
        session.generation += 1;
        session.relogin_error = None;
        drop(session);
        self.save_cookies()
    }

    /// Ends the Learn session (best effort) and deletes the local session cookies.
    pub async fn logout(&self) -> Result<()> {
        // thu-learn-lib logs out with a plain POST; failure only means the session was already gone.
        let _ = self.http.post(self.endpoints.logout()).send().await;
        self.cookie_store.lock().unwrap().clear();
        {
            let mut session = self.session.lock().await;
            session.csrf = None;
            session.generation += 1;
        }
        match std::fs::remove_file(&self.cookie_path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => {
                Err(e).with_context(|| format!("Failed to remove {}", self.cookie_path.display()))
            }
        }
    }

    /// Imports cookies captured from the browser and returns the inserted count.
    pub fn import_cookies(&self, cookies: &[(String, String, String, String)]) -> Result<usize> {
        let mut store = self.cookie_store.lock().unwrap();
        let mut n = 0;
        for (name, value, domain, path) in cookies {
            let bare = domain.trim_start_matches('.');
            // Broad cookie domains need a concrete host URL for insertion.
            let host = if bare == "tsinghua.edu.cn" {
                "learn.tsinghua.edu.cn"
            } else {
                bare
            };
            let Ok(url) = url::Url::parse(&format!("https://{host}/")) else {
                continue;
            };
            let mut raw = cookie_store::RawCookie::new(name.clone(), value.clone());
            raw.set_domain(domain.clone());
            raw.set_path(path.clone());
            if store.insert_raw(&raw, &url).is_ok() {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Fetches the course page and extracts `_csrf`; `None` means not logged in.
    async fn fetch_csrf(&self) -> Result<Option<String>> {
        let html = self
            .http
            .get(self.endpoints.course_list_page())
            .send()
            .await
            .context("Failed to reach Tsinghua Learn")?
            .text()
            .await?;
        Ok(extract_csrf(&html))
    }

    async fn install_csrf(&self, token: String) {
        let mut session = self.session.lock().await;
        if session.csrf.as_deref() != Some(token.as_str()) {
            session.csrf = Some(token);
            session.generation += 1;
        }
        session.relogin_error = None;
    }

    /// Re-establishes the session after a request made under `observed` found it expired.
    async fn renew_session(&self, observed: u64) -> Result<()> {
        let mut session = self.session.lock().await;
        if session.generation != observed && session.csrf.is_some() {
            // Another request already renewed the session while this one waited.
            return Ok(());
        }
        if let Some(reason) = &session.relogin_error {
            return Err(AuthError::ReloginFailed(reason.clone()).into());
        }
        let creds = match self.credentials().await {
            Ok(Some(creds)) => creds,
            Ok(None) => return Err(AuthError::SessionExpired.into()),
            Err(e) => {
                let reason = format!("could not read saved credentials: {e:#}");
                session.relogin_error = Some(reason.clone());
                return Err(AuthError::ReloginFailed(reason).into());
            }
        };
        match self.sso_login(&creds).await {
            Ok(token) => {
                session.csrf = Some(token);
                session.generation += 1;
                session.relogin_error = None;
                drop(session);
                self.save_cookies()
            }
            Err(e) => {
                let reason = format!("{e:#}");
                session.relogin_error = Some(reason.clone());
                Err(AuthError::ReloginFailed(reason).into())
            }
        }
    }

    async fn credentials(&self) -> Result<Option<Credentials>> {
        let mut slot = self.credentials.lock().await;
        if let CredentialSlot::Loaded(creds) = &*slot {
            return Ok(creds.clone());
        }
        let creds = crate::credentials::load_async().await?;
        *slot = CredentialSlot::Loaded(creds.clone());
        Ok(creds)
    }

    /// Logs in through `id.tsinghua.edu.cn` like thu-learn-lib and returns the new `_csrf`.
    async fn sso_login(&self, creds: &Credentials) -> Result<String> {
        // Always start from the login form rather than an older ID session.
        self.forget_id_session();

        let form_page = self
            .http
            .get(self.endpoints.id_login_form())
            .send()
            .await
            .context("Failed to open the Tsinghua login page")?
            .error_for_status()
            .context("The Tsinghua login page returned an error")?;
        if Endpoints::same_origin(&self.endpoints.learn, form_page.url()) {
            // Already signed in to ID: the form redirected straight back to Learn.
            if let Some(token) = self.fetch_csrf().await? {
                return Ok(token);
            }
        }
        let form_html = form_page.text().await?;
        let public_key = sso::parse_sm2_public_key(&form_html).ok_or(SsoFailure::NoPublicKey)?;
        let encrypted_password = sso::encrypt_password(&creds.password, &public_key)?;

        let form = Form::new()
            .text("i_user", creds.username.clone())
            .text("i_pass", encrypted_password)
            .text("singleLogin", "on")
            .text("fingerPrint", creds.fingerprint.clone())
            .text("fingerGenPrint", creds.finger_gen_print.clone())
            .text("fingerGenPrint3", creds.finger_gen_print3.clone())
            .text("i_captcha", "");
        let check = self
            .http
            .post(self.endpoints.id_login_check())
            .multipart(form)
            .send()
            .await
            .context("Failed to submit the Tsinghua login form")?;
        let landed_on_learn = Endpoints::same_origin(&self.endpoints.learn, check.url());
        let check_url = check.url().to_string();
        let check_html = check.text().await?;

        match sso::parse_ticket(&check_html) {
            Some(ticket) => {
                let roam = self
                    .http
                    .get(self.endpoints.roaming(&ticket)?)
                    .send()
                    .await
                    .context("Failed to open Learn with the login ticket")?;
                if !roam.status().is_success() {
                    bail!("Learn rejected the login ticket (HTTP {})", roam.status());
                }
            }
            // A redirect may already have carried the ticket to Learn.
            None if landed_on_learn => {}
            None => return Err(sso::classify_failure(&check_url, &check_html).into()),
        }

        self.fetch_csrf()
            .await?
            .ok_or_else(|| anyhow!("Learn did not return a _csrf token after login"))
    }

    /// Drops the unified-authentication session cookie, like thu-learn-lib does before login.
    fn forget_id_session(&self) {
        let Some(id_host) = url::Url::parse(&self.endpoints.id)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
        else {
            return;
        };
        let mut store = self.cookie_store.lock().unwrap();
        let stale: Vec<(String, String)> = store
            .iter_any()
            .filter(|c| c.name() == "JSESSIONID" && String::from(&c.domain) == id_host)
            .map(|c| (String::from(&c.domain), String::from(&c.path)))
            .collect();
        for (domain, path) in stale {
            store.remove(&domain, &path, "JSESSIONID");
        }
    }

    /// Returns the current `_csrf` and session generation, logging in first if needed.
    async fn session_token(&self) -> Result<(String, u64)> {
        {
            let session = self.session.lock().await;
            if let Some(token) = &session.csrf {
                return Ok((token.clone(), session.generation));
            }
        }
        self.confirm_session().await?;
        let session = self.session.lock().await;
        let token = session.csrf.clone().ok_or(AuthError::SessionExpired)?;
        Ok((token, session.generation))
    }

    /// Whether Learn answered as if the session were gone (thu-learn-lib's `noLogin`).
    fn login_required(&self, resp: &Response) -> bool {
        matches!(
            resp.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ) || resp.url().as_str().contains("login_timeout")
            || Endpoints::same_origin(&self.endpoints.id, resp.url())
    }

    /// Sends an authenticated request built for a `_csrf` token, re-logging in once when needed.
    async fn send_authed(&self, build: impl Fn(&str) -> RequestBuilder) -> Result<Response> {
        let (token, generation) = self.session_token().await?;
        let resp = build(&token).send().await?;
        if !self.login_required(&resp) {
            self.save_cookies()?;
            return Ok(resp);
        }

        self.renew_session(generation).await?;
        let (token, _) = self.session_token().await?;
        let resp = build(&token).send().await?;
        if self.login_required(&resp) {
            bail!(
                "Learn rejected the request even after logging in again (HTTP {}, {})",
                resp.status(),
                resp.url()
            );
        }
        self.save_cookies()?;
        Ok(resp)
    }

    /// Sends a GET with `_csrf` and parses the response body as JSON.
    pub async fn get_json(&self, url: &str) -> Result<serde_json::Value> {
        let resp = self
            .send_authed(|csrf| self.http.get(url).query(&[("_csrf", csrf)]))
            .await?;
        let text = resp.text().await?;
        serde_json::from_str(&text).with_context(|| {
            format!(
                "Failed to parse JSON; first 200 response chars: {}",
                snippet(&text)
            )
        })
    }

    /// Sends a GET with `_csrf` and returns the raw response text.
    pub async fn get_text(&self, url: &str) -> Result<String> {
        let resp = self
            .send_authed(|csrf| self.http.get(url).query(&[("_csrf", csrf)]))
            .await?;
        Ok(resp.text().await?)
    }

    /// Sends a multipart POST with `_csrf` and parses the response as JSON.
    ///
    /// `form` builds the body; it is called again if the request is retried after re-login.
    pub async fn post_multipart_json(
        &self,
        url: &str,
        form: impl Fn() -> Form,
    ) -> Result<serde_json::Value> {
        let resp = self
            .send_authed(|csrf| {
                self.http
                    .post(url)
                    .query(&[("_csrf", csrf)])
                    .multipart(form())
            })
            .await?;
        let text = resp.text().await?;
        serde_json::from_str(&text).with_context(|| {
            format!(
                "Failed to parse JSON; first 200 response chars: {}",
                snippet(&text)
            )
        })
    }

    /// Downloads a file to the requested path with `_csrf` included.
    pub async fn download(&self, url: &str, dest: &Path) -> Result<u64> {
        let resp = self
            .send_authed(|csrf| self.http.get(url).query(&[("_csrf", csrf)]))
            .await?
            .error_for_status()?;
        let bytes = resp.bytes().await?;
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        tokio::fs::write(dest, &bytes).await?;
        Ok(bytes.len() as u64)
    }

    /// Downloads bytes and returns the file name suggested by `Content-Disposition`.
    pub async fn fetch_download(&self, url: &str) -> Result<(Vec<u8>, Option<String>)> {
        let resp = self
            .send_authed(|csrf| self.http.get(url).query(&[("_csrf", csrf)]))
            .await?
            .error_for_status()?;
        let cd = resp.headers().get(reqwest::header::CONTENT_DISPOSITION);
        let name = cd.and_then(|v| filename_from_content_disposition(v.as_bytes()));
        let bytes = resp.bytes().await?.to_vec();
        Ok((bytes, name))
    }

    /// Saves current cookies to disk for later reuse, atomically and owner-only.
    #[allow(deprecated)]
    pub fn save_cookies(&self) -> Result<()> {
        let _guard = self.save_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut buf = Vec::new();
        {
            let store = self.cookie_store.lock().unwrap();
            // Learn session cookies are nonpersistent, so ordinary save_json would drop them.
            store
                .save_incl_expired_and_nonpersistent_json(&mut buf)
                .map_err(|e| anyhow!("Failed to save cookies: {e}"))?;
        }
        crate::paths::write_private(&self.cookie_path, &buf)
            .with_context(|| format!("Failed to write {}", self.cookie_path.display()))
    }
}

#[allow(deprecated)]
fn load_cookie_store(path: &Path) -> CookieStore {
    let Ok(file) = std::fs::File::open(path) else {
        return CookieStore::default();
    };
    let reader = std::io::BufReader::new(file);
    // load_json_all restores session cookies; expired cookies are filtered on use.
    CookieStore::load_json_all(reader).unwrap_or_default()
}

/// Extracts `_csrf` from course-page links without assuming a token alphabet.
fn extract_csrf(html: &str) -> Option<String> {
    let re = regex::Regex::new(r#"_csrf=([^"'&\s]+)"#).ok()?;
    re.captures(html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

fn snippet(s: &str) -> String {
    s.chars().take(200).collect()
}

/// Percent-decodes a string into raw bytes.
fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(x) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(x);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Replaces path separators so server file names cannot escape the target directory.
fn sanitize_filename(name: &str) -> String {
    name.trim().trim_matches('"').replace(['/', '\\'], "_")
}

/// Parses a file name from `Content-Disposition`, preferring RFC5987 `filename*`.
fn filename_from_content_disposition(raw: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(raw);

    if let Some(idx) = s.find("filename*=") {
        let rest = &s[idx + "filename*=".len()..];
        let val = rest.split(';').next().unwrap_or(rest).trim();
        let mut parts = val.splitn(3, '\'');
        let _charset = parts.next();
        let _lang = parts.next();
        if let Some(pct) = parts.next() {
            if let Ok(name) = String::from_utf8(percent_decode(pct)) {
                let name = sanitize_filename(&name);
                if !name.is_empty() {
                    return Some(name);
                }
            }
        }
    }

    if let Some(idx) = s.find("filename=") {
        let rest = &s[idx + "filename=".len()..];
        let val = rest.split(';').next().unwrap_or(rest);
        let name = sanitize_filename(val);
        // Replacement characters mean the raw header was probably not UTF-8, so ignore it.
        if !name.is_empty() && !name.contains('\u{FFFD}') {
            return Some(name);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::{extract_csrf, filename_from_content_disposition, AuthError, Client};
    use crate::mock_sso::MockSso;
    use crate::sso::SsoFailure;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn temp_cookie_path(tag: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!(
                "thu-learn-client-test-{}-{tag}",
                std::process::id()
            ))
            .join("cookies.json")
    }

    async fn client_for(mock: &MockSso, tag: &str) -> Client {
        let path = temp_cookie_path(tag);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
        let client = Client::with_endpoints(path, mock.endpoints.clone()).unwrap();
        client.set_credentials(Some(mock.credentials())).await;
        client
    }

    fn json_url(mock: &MockSso) -> String {
        format!("{}/b/test/json", mock.endpoints.learn)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fresh_client_logs_in_silently_like_thu_learn_lib() {
        let mock = MockSso::start().await;
        let client = client_for(&mock, "fresh").await;

        let v = client.get_json(&json_url(&mock)).await.unwrap();

        assert_eq!(v["ok"], true);
        assert_eq!(mock.sso_logins(), 1);
        let fields = mock.state.lock().unwrap().last_check.clone();
        assert_eq!(fields["i_user"], "2025012345");
        assert!(fields["i_pass"].starts_with("04"));
        assert_eq!(fields["singleLogin"], "on");
        assert_eq!(fields["fingerPrint"], "0123456789abcdef0123456789abcdef");
        assert_eq!(fields["fingerGenPrint"], "gen-print");
        assert_eq!(fields["fingerGenPrint3"], "gen-print-3");
        assert_eq!(fields["i_captcha"], "");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn expired_session_is_renewed_once_for_concurrent_requests() {
        let mock = MockSso::start().await;
        let client = Arc::new(client_for(&mock, "concurrent").await);
        client.confirm_session().await.unwrap();
        assert_eq!(mock.sso_logins(), 1);

        mock.expire_session();
        let url = json_url(&mock);
        let tasks = (0..8).map(|_| {
            let client = client.clone();
            let url = url.clone();
            tokio::spawn(async move { client.get_json(&url).await })
        });
        for result in futures::future::join_all(tasks).await {
            assert_eq!(result.unwrap().unwrap()["ok"], true);
        }
        assert_eq!(mock.sso_logins(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stale_csrf_token_triggers_relogin() {
        let mock = MockSso::start().await;
        let client = client_for(&mock, "stale-csrf").await;
        client.confirm_session().await.unwrap();
        mock.state.lock().unwrap().csrf = "rotated".into();

        assert_eq!(client.get_json(&json_url(&mock)).await.unwrap()["ok"], true);
        assert_eq!(mock.sso_logins(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn multipart_body_is_rebuilt_when_retried() {
        let mock = MockSso::start().await;
        let client = client_for(&mock, "multipart").await;
        client.confirm_session().await.unwrap();
        mock.expire_session();

        let url = format!("{}/b/test/form", mock.endpoints.learn);
        let v = client
            .post_multipart_json(&url, || reqwest::multipart::Form::new().text("x", "42"))
            .await
            .unwrap();

        assert_eq!(v["echo"], "42");
        assert_eq!(mock.sso_logins(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn untrusted_device_fails_once_without_retry_storm() {
        let mock = MockSso::start().await;
        let client = Arc::new(client_for(&mock, "untrusted").await);
        let mut creds = mock.credentials();
        creds.fingerprint = "ffffffffffffffffffffffffffffffff".into();
        client.set_credentials(Some(creds)).await;

        let url = json_url(&mock);
        let tasks = (0..5).map(|_| {
            let client = client.clone();
            let url = url.clone();
            tokio::spawn(async move { client.get_json(&url).await })
        });
        for result in futures::future::join_all(tasks).await {
            let err = result.unwrap().unwrap_err();
            let auth = err.downcast_ref::<AuthError>().expect("auth error");
            assert!(matches!(auth, AuthError::ReloginFailed(r) if r.contains("two-factor")));
        }
        assert_eq!(mock.check_attempts(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wrong_password_is_reported_with_the_page_message() {
        let mock = MockSso::start().await;
        let client = client_for(&mock, "wrong-password").await;
        let mut creds = mock.credentials();
        creds.password = "nope".into();

        let err = client.login_with(&creds).await.unwrap_err();

        assert_eq!(
            err.downcast_ref::<SsoFailure>(),
            Some(&SsoFailure::Rejected(Some(
                "Wrong username or password".into()
            )))
        );
        assert_eq!(mock.sso_logins(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_credentials_report_session_expired() {
        let mock = MockSso::start().await;
        let client = client_for(&mock, "no-creds").await;
        client.set_credentials(None).await;

        let err = client.get_json(&json_url(&mock)).await.unwrap_err();

        assert_eq!(
            err.downcast_ref::<AuthError>(),
            Some(&AuthError::SessionExpired)
        );
        assert_eq!(mock.check_attempts(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn renewed_session_cookies_are_reused_by_the_next_process() {
        let mock = MockSso::start().await;
        let client = client_for(&mock, "persist").await;
        client.get_json(&json_url(&mock)).await.unwrap();
        assert_eq!(mock.sso_logins(), 1);

        // A later command starts from the saved cookie file only.
        let next =
            Client::with_endpoints(temp_cookie_path("persist"), mock.endpoints.clone()).unwrap();
        next.set_credentials(None).await;
        assert_eq!(next.get_json(&json_url(&mock)).await.unwrap()["ok"], true);
        assert_eq!(mock.sso_logins(), 1);
        std::fs::remove_dir_all(temp_cookie_path("persist").parent().unwrap()).ok();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn logout_forgets_the_session() {
        let mock = MockSso::start().await;
        let client = client_for(&mock, "logout").await;
        client.confirm_session().await.unwrap();
        let path = temp_cookie_path("logout");
        assert!(path.exists());

        client.logout().await.unwrap();
        client.set_credentials(None).await;

        assert!(!path.exists());
        let err = client.get_json(&json_url(&mock)).await.unwrap_err();
        assert_eq!(
            err.downcast_ref::<AuthError>(),
            Some(&AuthError::SessionExpired)
        );
    }

    #[test]
    fn cd_filename_ascii() {
        let cd = br#"attachment; filename="Physics(1)_Mock Exam.pdf""#;
        assert_eq!(
            filename_from_content_disposition(cd).as_deref(),
            Some("Physics(1)_Mock Exam.pdf")
        );
    }

    #[test]
    fn cd_filename_utf8_bytes() {
        // Realistic Learn fixture: filename="<UTF-8 bytes>".
        let mut cd = b"attachment; filename=\"".to_vec();
        cd.extend_from_slice("数分2作业13.pdf".as_bytes());
        cd.push(b'"');
        assert_eq!(
            filename_from_content_disposition(&cd).as_deref(),
            Some("数分2作业13.pdf")
        );
    }

    #[test]
    fn cd_filename_rfc5987() {
        let cd = b"attachment; filename*=UTF-8''%E6%95%B0.pdf";
        assert_eq!(
            filename_from_content_disposition(cd).as_deref(),
            Some("数.pdf")
        );
    }

    #[test]
    fn cd_filename_sanitizes_slash() {
        let cd = br#"attachment; filename="a/b.pdf""#;
        assert_eq!(
            filename_from_content_disposition(cd).as_deref(),
            Some("a_b.pdf")
        );
    }

    #[test]
    fn csrf_extracted() {
        let html = r#"<a href="/x?course=1&_csrf=abc-123-def">link</a>"#;
        assert_eq!(extract_csrf(html).as_deref(), Some("abc-123-def"));
    }
}
