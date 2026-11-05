//! Local stand-ins for Learn and `id.tsinghua.edu.cn`, used by client tests.
//!
//! The mock follows the protocol that thu-learn-lib (and therefore learnX) uses:
//! the ID login form publishes an SM2 public key, the login check decrypts
//! `i_pass` and only issues a ticket for the trusted `fingerPrint`, and Learn's
//! roaming entry turns that ticket into a session with a fresh `_csrf` token.
//! Expired Learn sessions redirect to `login_timeout`; stale tokens get HTTP 403.

use sm2::elliptic_curve::sec1::ToSec1Point;
use sm2::elliptic_curve::Generate;
use sm2::pke::{DecryptingKey, Mode};
use sm2::SecretKey;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::client::Endpoints;
use crate::credentials::Credentials;

/// Learn's session cookie.
const LEARN_COOKIE: &str = "JSESSIONID";
/// The ID mock uses a different name because both mocks share the 127.0.0.1 cookie host.
const ID_COOKIE: &str = "IDSESSION";

pub struct State {
    secret_key: SecretKey,
    public_key_hex: String,
    pub username: String,
    pub password: String,
    pub trusted_fingerprint: String,
    /// Currently valid Learn session cookie value.
    pub session: Option<String>,
    pub csrf: String,
    /// Login checks that issued a ticket.
    pub sso_logins: usize,
    /// All login-check requests.
    pub check_attempts: usize,
    /// Fields of the last login-check request.
    pub last_check: HashMap<String, String>,
    /// Login-form requests received.
    pub form_requests: usize,
    /// Answer this many upcoming login-form requests with HTTP 503.
    pub fail_forms: usize,
    /// Delay before answering login-form requests.
    pub form_delay: Duration,
    tickets: Vec<String>,
    counter: usize,
}

pub struct MockSso {
    pub endpoints: Endpoints,
    pub state: Arc<Mutex<State>>,
}

impl MockSso {
    pub async fn start() -> MockSso {
        let secret_key = SecretKey::generate();
        let public_key_hex =
            crate::sso::to_hex(secret_key.public_key().to_sec1_point(false).as_bytes());
        let state = Arc::new(Mutex::new(State {
            secret_key,
            public_key_hex,
            username: "2025012345".into(),
            password: "correct horse 电池".into(),
            trusted_fingerprint: "0123456789abcdef0123456789abcdef".into(),
            session: None,
            csrf: "csrf-0".into(),
            sso_logins: 0,
            check_attempts: 0,
            last_check: HashMap::new(),
            form_requests: 0,
            fail_forms: 0,
            form_delay: Duration::ZERO,
            tickets: Vec::new(),
            counter: 0,
        }));

        let learn_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let id_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let learn = format!("http://{}", learn_listener.local_addr().unwrap());
        let id = format!("http://{}", id_listener.local_addr().unwrap());

        for (listener, is_learn) in [(learn_listener, true), (id_listener, false)] {
            let state = state.clone();
            let learn = learn.clone();
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let state = state.clone();
                    let learn = learn.clone();
                    tokio::spawn(async move {
                        serve(stream, &state, is_learn, &learn).await;
                    });
                }
            });
        }

        MockSso {
            endpoints: Endpoints { learn, id },
            state,
        }
    }

    /// Credentials that the mock accepts.
    pub fn credentials(&self) -> Credentials {
        let s = self.state.lock().unwrap();
        Credentials {
            username: s.username.clone(),
            password: s.password.clone(),
            fingerprint: s.trusted_fingerprint.clone(),
            finger_gen_print: "gen-print".into(),
            finger_gen_print3: "gen-print-3".into(),
        }
    }

    /// Simulates the server-side Learn session timing out.
    pub fn expire_session(&self) {
        self.state.lock().unwrap().session = None;
    }

    pub fn sso_logins(&self) -> usize {
        self.state.lock().unwrap().sso_logins
    }

    pub fn check_attempts(&self) -> usize {
        self.state.lock().unwrap().check_attempts
    }

    pub fn form_requests(&self) -> usize {
        self.state.lock().unwrap().form_requests
    }
}

struct Request {
    method: String,
    path: String,
    query: HashMap<String, String>,
    cookies: HashMap<String, String>,
    form: HashMap<String, String>,
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    fn html(body: impl Into<String>) -> Reply {
        Reply {
            status: 200,
            headers: vec![("Content-Type".into(), "text/html; charset=utf-8".into())],
            body: body.into(),
        }
    }

    fn json(body: serde_json::Value) -> Reply {
        Reply {
            status: 200,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: body.to_string(),
        }
    }

    fn redirect(location: &str) -> Reply {
        Reply {
            status: 302,
            headers: vec![("Location".into(), location.into())],
            body: String::new(),
        }
    }

    fn status(status: u16) -> Reply {
        Reply {
            status,
            headers: Vec::new(),
            body: String::new(),
        }
    }

    fn cookie(mut self, name: &str, value: &str) -> Reply {
        self.headers.push((
            "Set-Cookie".into(),
            format!("{name}={value}; Path=/; HttpOnly"),
        ));
        self
    }
}

async fn serve(mut stream: TcpStream, state: &Mutex<State>, is_learn: bool, learn: &str) {
    let Some(req) = read_request(&mut stream).await else {
        return;
    };
    let delay = if is_learn || !req.path.starts_with("/do/off/ui/auth/login/form/") {
        Duration::ZERO
    } else {
        state.lock().unwrap().form_delay
    };
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let reply = {
        let mut s = state.lock().unwrap();
        if is_learn {
            learn_route(&mut s, &req)
        } else {
            id_route(&mut s, &req, learn)
        }
    };
    let reason = match reply.status {
        200 => "OK",
        302 => "Found",
        403 => "Forbidden",
        503 => "Service Unavailable",
        _ => "Status",
    };
    let mut head = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        reply.body.len()
    );
    for (k, v) in &reply.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(reply.body.as_bytes()).await;
    let _ = stream.shutdown().await;
}

fn learn_route(s: &mut State, req: &Request) -> Reply {
    let logged_in = s.session.is_some() && req.cookies.get(LEARN_COOKIE) == s.session.as_ref();
    let csrf_ok = req.query.get("_csrf") == Some(&s.csrf);
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/f/wlxt/index/course/student/") if logged_in => Reply::html(format!(
            r#"<a href="/f/wlxt/index/course/student/course?wlkcid=1&_csrf={}">Course</a>"#,
            s.csrf
        )),
        ("GET", "/f/wlxt/index/course/student/") => {
            Reply::html("<html><a href=\"/f/login\">Log in</a></html>")
        }
        ("GET", "/f/login_timeout") => Reply::html("<html>Login timed out</html>"),
        ("GET", "/b/j_spring_security_thauth_roaming_entry") => {
            let ticket = req.query.get("ticket").cloned().unwrap_or_default();
            let Some(pos) = s.tickets.iter().position(|t| *t == ticket) else {
                return Reply::html("<html>Invalid ticket</html>");
            };
            s.tickets.remove(pos);
            s.counter += 1;
            let session = format!("learn-session-{}", s.counter);
            s.session = Some(session.clone());
            s.csrf = format!("csrf-{}", s.counter);
            Reply::redirect("/f/wlxt/index/course/student/").cookie(LEARN_COOKIE, &session)
        }
        (_, "/b/test/json" | "/b/test/form") if !logged_in => Reply::redirect("/f/login_timeout"),
        (_, "/b/test/json" | "/b/test/form") if !csrf_ok => Reply::status(403),
        ("GET", "/b/test/json") => Reply::json(serde_json::json!({ "ok": true })),
        ("POST", "/b/test/form") => Reply::json(serde_json::json!({
            "echo": req.form.get("x").cloned().unwrap_or_default()
        })),
        _ => Reply::status(404),
    }
}

fn id_route(s: &mut State, req: &Request, learn: &str) -> Reply {
    let form_path = format!("/do/off/ui/auth/login/form/{}/0", crate::sso::LEARN_APP_ID);
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", p) if p == form_path => {
            s.form_requests += 1;
            if s.fail_forms > 0 {
                s.fail_forms -= 1;
                return Reply::status(503);
            }
            s.counter += 1;
            Reply::html(login_form(&s.public_key_hex, None))
                .cookie(ID_COOKIE, &format!("id-{}", s.counter))
        }
        ("POST", "/do/off/ui/auth/login/check") => {
            s.check_attempts += 1;
            s.last_check = req.form.clone();
            let password = req
                .form
                .get("i_pass")
                .and_then(|c| decrypt(&s.secret_key, c));
            if req.form.get("i_user") != Some(&s.username) || password.as_ref() != Some(&s.password)
            {
                return Reply::html(login_form(
                    &s.public_key_hex,
                    Some("Wrong username or password"),
                ));
            }
            if req.form.get("fingerPrint") != Some(&s.trusted_fingerprint) {
                return Reply::html(
                    r#"<html><script src="/b/doubleAuth/personal/js"></script>Verify your identity</html>"#,
                );
            }
            s.counter += 1;
            let ticket = format!("ST-{}-mock", s.counter);
            s.tickets.push(ticket.clone());
            s.sso_logins += 1;
            Reply::html(format!(
                r#"<html><body><a href="{learn}/f/j_spring_security_thauth_roaming_entry?ticket={ticket}">Continue</a></body></html>"#
            ))
        }
        _ => Reply::status(404),
    }
}

fn login_form(public_key_hex: &str, error: Option<&str>) -> String {
    let error = error
        .map(|e| format!(r#"<div id="msg_note">{e}</div>"#))
        .unwrap_or_default();
    format!(
        r#"<html><body>{error}<div id="sm2publicKey" style="display:none">{public_key_hex}</div>
        <form id="theform"><input id="i_user" name="i_user"><input id="i_pass" name="i_pass">
        <input name="fingerPrint" type="hidden"></form></body></html>"#
    )
}

fn decrypt(secret_key: &SecretKey, i_pass_hex: &str) -> Option<String> {
    let bytes = (0..i_pass_hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(i_pass_hex.get(i..i + 2)?, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    let dk = DecryptingKey::new_with_mode(secret_key.to_nonzero_scalar(), Mode::C1C3C2);
    String::from_utf8(dk.decrypt(&bytes).ok()?).ok()
}

async fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(pos) = find(&buf, b"\r\n\r\n") {
            break pos;
        }
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split_whitespace();
    let method = request_line.next()?.to_string();
    let target = request_line.next()?.to_string();
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();

    let mut body = buf[head_end + 4..].to_vec();
    if let Some(len) = headers
        .get("content-length")
        .and_then(|l| l.parse::<usize>().ok())
    {
        while body.len() < len {
            let n = stream.read(&mut chunk).await.ok()?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
    } else if headers
        .get("transfer-encoding")
        .is_some_and(|t| t.contains("chunked"))
    {
        while find(&body, b"0\r\n\r\n").is_none() {
            let n = stream.read(&mut chunk).await.ok()?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
        body = dechunk(&body);
    }

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), parse_query(q)),
        None => (target, HashMap::new()),
    };
    let cookies = headers
        .get("cookie")
        .map(|c| {
            c.split(';')
                .filter_map(|kv| kv.trim().split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let form = headers
        .get("content-type")
        .and_then(|ct| ct.split("boundary=").nth(1))
        .map(|boundary| parse_multipart(&body, boundary.trim_matches('"')))
        .unwrap_or_default();
    Some(Request {
        method,
        path,
        query,
        cookies,
        form,
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn dechunk(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(line_end) = find(rest, b"\r\n") {
        let size = usize::from_str_radix(
            String::from_utf8_lossy(&rest[..line_end])
                .split(';')
                .next()
                .unwrap_or("0")
                .trim(),
            16,
        )
        .unwrap_or(0);
        if size == 0 {
            break;
        }
        let start = line_end + 2;
        out.extend_from_slice(&rest[start..(start + size).min(rest.len())]);
        rest = &rest[(start + size + 2).min(rest.len())..];
    }
    out
}

fn parse_query(q: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(q.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

fn parse_multipart(body: &[u8], boundary: &str) -> HashMap<String, String> {
    let text = String::from_utf8_lossy(body);
    let delimiter = format!("--{boundary}");
    let mut out = HashMap::new();
    for part in text.split(delimiter.as_str()) {
        let Some((head, value)) = part.split_once("\r\n\r\n") else {
            continue;
        };
        let Some(name) = head
            .split("name=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
        else {
            continue;
        };
        let value = value.strip_suffix("\r\n").unwrap_or(value);
        out.insert(name.to_string(), value.to_string());
    }
    out
}
