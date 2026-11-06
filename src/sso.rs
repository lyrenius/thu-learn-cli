//! THU unified authentication (`id.tsinghua.edu.cn`) helpers for silent re-login.
//!
//! This mirrors learnX, which keeps students signed in by storing the account
//! password plus a random device fingerprint, and logging in again over plain
//! HTTP (via [thu-learn-lib](https://github.com/robertying/thu-learn-lib)) whenever
//! the Learn session expires:
//!
//! 1. `GET` the ID login form and read the SM2 public key from `#sm2publicKey`.
//! 2. `POST` the login check with `i_pass = "04" || SM2(password)` (C1C3C2) and the
//!    trusted `fingerPrint`; the response links to Learn with a `ticket`.
//! 3. `GET` Learn's `j_spring_security_thauth_roaming_entry?ticket=...` to create
//!    the Learn session, then read `_csrf` from the course page.
//!
//! The fingerprint is only trusted after one interactive browser login in which
//! the user completes two-factor authentication and the "trust this device" step
//! is answered with our fingerprint (see [`crate::browser_login`]).

use anyhow::{anyhow, Context, Result};
use scraper::{Html, Selector};
use sm2::pke::EncryptingKey;

/// Learn's application ID in THU unified authentication.
pub const LEARN_APP_ID: &str = "bb5df85216504820be7bba2b0ae1535b";

/// Why an HTTP login against `id.tsinghua.edu.cn` did not yield a Learn ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SsoFailure {
    /// The login form did not contain an SM2 public key.
    NoPublicKey,
    /// ID asked for two-factor authentication: the saved fingerprint is not trusted.
    NeedsTwoFactor,
    /// ID rendered the login form again (wrong password, captcha, or locked account).
    Rejected(Option<String>),
    /// The response had no ticket and no recognizable failure page.
    NoTicket,
}

impl std::fmt::Display for SsoFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SsoFailure::NoPublicKey => {
                write!(f, "the THU login form did not include an SM2 public key")
            }
            SsoFailure::NeedsTwoFactor => write!(
                f,
                "THU asked for two-factor authentication, so this device is not \
                 (or no longer) trusted"
            ),
            SsoFailure::Rejected(Some(msg)) => {
                write!(f, "THU rejected the saved credentials: {msg}")
            }
            SsoFailure::Rejected(None) => write!(
                f,
                "THU rejected the saved credentials (wrong password, or a captcha is required)"
            ),
            SsoFailure::NoTicket => write!(
                f,
                "THU did not return a Learn ticket; the login page may have changed"
            ),
        }
    }
}

impl std::error::Error for SsoFailure {}

/// Generates a random device fingerprint in learnX's format: a UUIDv4 without dashes.
pub fn generate_fingerprint() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| anyhow!("Failed to generate a device ID: {e}"))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // UUID version 4.
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // RFC 4122 variant.
    Ok(to_hex(&bytes))
}

/// Device name shown in the THU trusted-device list, like learnX's `<model>,learnX/<version>`.
pub fn device_name() -> String {
    let os = match std::env::consts::OS {
        "macos" => "macOS",
        "linux" => "Linux",
        "windows" => "Windows",
        other => other,
    };
    format!("{os},thu-learn-cli/{}", env!("CARGO_PKG_VERSION"))
}

/// Reads the SM2 public key (SEC1 hex) that the ID login form uses to encrypt `i_pass`.
pub fn parse_sm2_public_key(html: &str) -> Option<String> {
    let doc = Html::parse_document(html);
    let sel = Selector::parse("#sm2publicKey").ok()?;
    let el = doc.select(&sel).next()?;
    let text: String = el.text().collect::<String>();
    let key = if text.trim().is_empty() {
        el.value().attr("value").unwrap_or_default().to_string()
    } else {
        text
    };
    let key: String = key.chars().filter(|c| !c.is_whitespace()).collect();
    (!key.is_empty()).then_some(key)
}

/// Encrypts the password like thu-learn-lib: `"04" + sm-crypto doEncrypt(password, key)`.
///
/// sm-crypto's default cipher mode is C1C3C2 and omits the SEC1 `04` tag from C1,
/// so the result is the hex encoding of `04 || x1 || y1 || C3 || C2`, which is
/// exactly the standard uncompressed C1C3C2 layout produced by the `sm2` crate.
pub fn encrypt_password(password: &str, public_key_hex: &str) -> Result<String> {
    let mut key = from_hex(public_key_hex.trim()).context("SM2 public key is not valid hex")?;
    if key.len() == 64 {
        // Raw x || y without the SEC1 uncompressed-point tag.
        key.insert(0, 0x04);
    }
    let encrypting_key = EncryptingKey::from_sec1_bytes(&key)
        .map_err(|_| anyhow!("SM2 public key is not a valid curve point"))?;
    let cipher = encrypting_key
        .encrypt(&mut getrandom::SysRng, password.as_bytes())
        .map_err(|_| anyhow!("SM2 encryption failed"))?;
    Ok(to_hex(&cipher))
}

/// Extracts the Learn roaming ticket from the ID login-check response.
///
/// thu-learn-lib takes the first `<a href>` and keeps the text after its last `=`;
/// this looks for the `ticket` query parameter explicitly and also accepts a
/// script redirect, so an unrelated link cannot be mistaken for a ticket.
pub fn parse_ticket(html: &str) -> Option<String> {
    let doc = Html::parse_document(html);
    if let Ok(sel) = Selector::parse("a[href]") {
        for a in doc.select(&sel) {
            let href = a.value().attr("href").unwrap_or_default();
            if let Some(ticket) = ticket_from_url_like(href) {
                return Some(ticket);
            }
        }
    }
    ticket_from_url_like(html)
}

fn ticket_from_url_like(s: &str) -> Option<String> {
    let re = regex::Regex::new(r#"[?&](?:amp;)?ticket=([^"'&\s<>;\\]+)"#).ok()?;
    let ticket = re.captures(s)?.get(1)?.as_str();
    let ticket = url::form_urlencoded::parse(format!("t={ticket}").as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())?;
    (!ticket.is_empty()).then_some(ticket)
}

/// Classifies a login-check response that did not contain a ticket.
pub fn classify_failure(final_url: &str, html: &str) -> SsoFailure {
    // The two-factor pages call `/b/doubleAuth/...` endpoints, such as the
    // `/b/doubleAuth/personal/saveFinger` "trust this device" request.
    if final_url.contains("doubleAuth") || html.contains("doubleAuth") {
        return SsoFailure::NeedsTwoFactor;
    }
    if html.contains("i_pass") || html.contains("sm2publicKey") {
        return SsoFailure::Rejected(login_error_message(html));
    }
    SsoFailure::NoTicket
}

/// Best-effort extraction of the error text shown above a re-rendered login form.
fn login_error_message(html: &str) -> Option<String> {
    let doc = Html::parse_document(html);
    for css in [
        "#msg_note",
        "#errorMsg",
        ".alert-danger",
        ".alert",
        ".error",
        "font[color=red]",
    ] {
        let Ok(sel) = Selector::parse(css) else {
            continue;
        };
        for el in doc.select(&sel) {
            let text = el
                .text()
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let text: String = text.chars().filter(|c| !c.is_control()).take(160).collect();
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

pub(crate) fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    s
}

fn from_hex(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        anyhow::bail!("odd-length or non-ASCII hex string");
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| anyhow!("{e}")))
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use sm2::pke::{DecryptingKey, Mode};
    use sm2::SecretKey;

    /// Fixture generated with sm-crypto 0.5.8 (the library thu-learn-lib and learnX use):
    /// `'04' + sm2.doEncrypt(plaintext, publicKey)` with the default cipher mode.
    const SM_CRYPTO_PRIVATE_KEY: &str =
        "92bce3b4255c8ecc8b9776cfab2a9835646ebd7786a092ad0b4011efd2de0066";
    const SM_CRYPTO_PUBLIC_KEY: &str = "047aae79854292a59241d924f4014bd75d20cf6a74d7e3c21c28d9d54ef7b64ce5fd00940d6e215f709cf360c18be6439c20e50583393f21b5156fa267905064e7";
    const SM_CRYPTO_PLAINTEXT: &str = "p@ss'w\"rd\\ 中文🙂";
    const SM_CRYPTO_CIPHERTEXT: &str = "04561ae44a70690cbd6cd54541850c6df466e3abd4f1ed8859f9fa7a4e2413bf17e39c40e608c093717a02732f93dfcb760a0fd91f9f4d7eb5c0c33caed334bf4f27c3233b13d29d73a4e5d49b4b575f9422d62121c242f9aed1411bdb361c25de26880cd9a48c495012e09ff0e574dc7965d54e91c6";

    /// Decrypts an `i_pass` value the way the ID server is expected to.
    pub(crate) fn decrypt_i_pass(private_key_hex: &str, i_pass: &str) -> String {
        let sk = SecretKey::from_slice(&from_hex(private_key_hex).unwrap()).unwrap();
        let dk = DecryptingKey::new_with_mode(sk.to_nonzero_scalar(), Mode::C1C3C2);
        let plain = dk.decrypt(&from_hex(i_pass).unwrap()).unwrap();
        String::from_utf8(plain).unwrap()
    }

    #[test]
    fn sm_crypto_fixture_decrypts_with_c1c3c2_layout() {
        assert_eq!(
            decrypt_i_pass(SM_CRYPTO_PRIVATE_KEY, SM_CRYPTO_CIPHERTEXT),
            SM_CRYPTO_PLAINTEXT
        );
    }

    #[test]
    fn encrypt_password_matches_sm_crypto_layout() {
        let c = encrypt_password(SM_CRYPTO_PLAINTEXT, SM_CRYPTO_PUBLIC_KEY).unwrap();
        // 04 || x1 || y1 (65 bytes) || C3 (32 bytes) || C2 (same length as the UTF-8 password).
        assert!(c.starts_with("04"));
        assert_eq!(c.len(), 2 * (65 + 32 + SM_CRYPTO_PLAINTEXT.len()));
        assert_eq!(c, c.to_ascii_lowercase());
        assert_eq!(
            decrypt_i_pass(SM_CRYPTO_PRIVATE_KEY, &c),
            SM_CRYPTO_PLAINTEXT
        );
        // SM2 encryption is randomized, so two ciphertexts differ.
        assert_ne!(
            c,
            encrypt_password(SM_CRYPTO_PLAINTEXT, SM_CRYPTO_PUBLIC_KEY).unwrap()
        );
    }

    #[test]
    fn encrypt_password_accepts_key_without_sec1_tag() {
        let raw = &SM_CRYPTO_PUBLIC_KEY[2..];
        let c = encrypt_password("secret", raw).unwrap();
        assert_eq!(decrypt_i_pass(SM_CRYPTO_PRIVATE_KEY, &c), "secret");
    }

    #[test]
    fn encrypt_password_rejects_bad_keys() {
        assert!(encrypt_password("secret", "zz").is_err());
        assert!(encrypt_password("secret", "04abcd").is_err());
    }

    #[test]
    fn public_key_is_read_from_text_or_value() {
        let html = format!(
            r#"<form id="theform"><div id="sm2publicKey" style="display:none">
                {SM_CRYPTO_PUBLIC_KEY}
            </div><input id="i_pass" name="i_pass"></form>"#
        );
        assert_eq!(
            parse_sm2_public_key(&html).as_deref(),
            Some(SM_CRYPTO_PUBLIC_KEY)
        );
        let html = r#"<input type="hidden" id="sm2publicKey" value="04ab cd">"#;
        assert_eq!(parse_sm2_public_key(html).as_deref(), Some("04abcd"));
        assert!(parse_sm2_public_key("<html></html>").is_none());
    }

    #[test]
    fn ticket_is_read_from_redirect_anchor() {
        let html = r#"<html><body>
            <a href="https://id.tsinghua.edu.cn/help">Help</a>
            <a href="https://learn.tsinghua.edu.cn/f/j_spring_security_thauth_roaming_entry?ticket=ST-123-AbC_d">Continue</a>
        </body></html>"#;
        assert_eq!(parse_ticket(html).as_deref(), Some("ST-123-AbC_d"));
    }

    #[test]
    fn ticket_is_read_from_script_redirect() {
        let html = r#"<script>window.location.href = "https://learn.tsinghua.edu.cn/f/j_spring_security_thauth_roaming_entry?ticket=ST%2D9&x=1";</script>"#;
        assert_eq!(parse_ticket(html).as_deref(), Some("ST-9"));
    }

    #[test]
    fn missing_ticket_is_none() {
        assert!(parse_ticket(r#"<a href="/do/off/ui/auth/login/form/x/0">Back</a>"#).is_none());
    }

    #[test]
    fn failure_pages_are_classified() {
        assert_eq!(
            classify_failure(
                "https://id.tsinghua.edu.cn/do/off/ui/auth/login/check",
                r#"<script>$.post("/b/doubleAuth/personal/send")</script>"#
            ),
            SsoFailure::NeedsTwoFactor
        );
        assert_eq!(
            classify_failure(
                "https://id.tsinghua.edu.cn/do/off/ui/auth/login/check",
                r#"<div id="msg_note"> Wrong
                password </div><input id="i_pass">"#
            ),
            SsoFailure::Rejected(Some("Wrong password".to_string()))
        );
        assert_eq!(
            classify_failure("https://id.tsinghua.edu.cn/x", "<html>maintenance</html>"),
            SsoFailure::NoTicket
        );
    }

    #[test]
    fn fingerprint_looks_like_learnx_uuid() {
        let fp = generate_fingerprint().unwrap();
        assert_eq!(fp.len(), 32);
        assert!(fp
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(&fp[12..13], "4");
        assert_ne!(fp, generate_fingerprint().unwrap());
    }

    #[test]
    fn device_name_names_the_cli() {
        let name = device_name();
        assert!(name.contains(",thu-learn-cli/"));
        assert!(name.ends_with(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn hex_roundtrip() {
        assert_eq!(to_hex(&[0x00, 0xab, 0xff]), "00abff");
        assert_eq!(from_hex("00abff").unwrap(), vec![0x00, 0xab, 0xff]);
        assert!(from_hex("abc").is_err());
    }
}
