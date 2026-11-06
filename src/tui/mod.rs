//! Interactive terminal UI for humans. Scripts and agents use the CLI subcommands.
//! Login stays in `thu-learn login`; the TUI reuses the saved session, which
//! [`Client`] renews silently when credentials are saved.
//!
//! The event loop draws [`app::App`], feeds it key presses, timer ticks, and
//! background results, and runs the [`app::Effect`]s it returns as tokio tasks.

mod app;
mod input;
mod picker;
mod ui;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyEventKind, MouseEventKind,
};
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::client::{AuthError, Client};
use crate::models::{Course, CourseFile, HomeworkAttachment};
use crate::paths;
use app::{App, CourseData, Effect, HomeworkDetail, Msg};
use input::{claim_unique_path, safe_file_name};

pub struct Options {
    /// Directory for downloads; can be changed inside the TUI.
    pub download_dir: PathBuf,
}

const TICK: Duration = Duration::from_millis(100);
/// Files fetched at once by a batch download, so Learn never gets dozens of requests together.
const DOWNLOAD_CONCURRENCY: usize = 4;

/// Messages from background tasks. A new client is handed to the runtime,
/// everything else goes to the app state.
enum Incoming {
    Client(Arc<Client>),
    App(Msg),
}

type Tx = mpsc::UnboundedSender<Incoming>;

fn send(tx: &Tx, msg: Msg) {
    // The receiver only closes when the TUI exits; dropping the result is fine then.
    let _ = tx.send(Incoming::App(msg));
}

fn err_text(e: anyhow::Error) -> String {
    format!("{e:#}")
}

fn download_failed(e: String) -> Msg {
    Msg::Downloaded {
        saved: Vec::new(),
        failed: vec![e],
    }
}

/// Collects labelled download results; a failure is reported as `label: error`.
fn downloaded(results: impl IntoIterator<Item = (String, Result<PathBuf>)>) -> Msg {
    let mut saved = Vec::new();
    let mut failed = Vec::new();
    for (label, result) in results {
        match result {
            Ok(path) => saved.push(path),
            Err(e) => failed.push(format!("{label}: {}", err_text(e))),
        }
    }
    Msg::Downloaded { saved, failed }
}

pub async fn run(options: Options) -> Result<()> {
    let mut terminal = ratatui::try_init().context("Failed to initialize the terminal")?;
    // ratatui's panic hook restores the screen but not mouse reporting, which
    // would otherwise keep printing escape codes in the shell after a crash.
    let restore_screen = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        crossterm::execute!(
            std::io::stdout(),
            DisableMouseCapture,
            DisableBracketedPaste
        )
        .ok();
        restore_screen(info);
    }));
    crossterm::execute!(std::io::stdout(), EnableBracketedPaste, EnableMouseCapture).ok();
    let result = event_loop(&mut terminal, options).await;
    crossterm::execute!(
        std::io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        SetCursorStyle::DefaultUserShape
    )
    .ok();
    ratatui::restore();
    result
}

async fn event_loop(terminal: &mut ratatui::DefaultTerminal, options: Options) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut runtime = Runtime { tx, client: None };
    let mut app = App::new(options.download_dir, chrono::Local::now());
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    runtime.dispatch(app.init());
    let mut shape = None;
    loop {
        terminal.draw(|f| ui::draw(f, &mut app))?;
        if app.cursor_shape() != shape {
            shape = app.cursor_shape();
            let style = match shape {
                Some(picker::CursorShape::Bar) => SetCursorStyle::SteadyBar,
                Some(picker::CursorShape::Block) => SetCursorStyle::SteadyBlock,
                None => SetCursorStyle::DefaultUserShape,
            };
            crossterm::execute!(std::io::stdout(), style).ok();
        }
        if app.should_quit {
            break;
        }
        let effects = tokio::select! {
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => app.on_key(key),
                Some(Ok(Event::Paste(text))) => {
                    app.on_paste(&text);
                    Vec::new()
                }
                // Mouse reporting also sends every move; skip redrawing for those.
                Some(Ok(Event::Mouse(m))) => match m.kind {
                    MouseEventKind::Down(_) | MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                        app.on_mouse(m)
                    }
                    _ => continue,
                },
                Some(Ok(_)) => Vec::new(),
                Some(Err(e)) => return Err(e).context("Failed to read terminal input"),
                None => break,
            },
            Some(incoming) = rx.recv() => match incoming {
                Incoming::Client(client) => {
                    runtime.client = Some(client);
                    Vec::new()
                }
                Incoming::App(msg) => app.on_msg(msg),
            },
            _ = tick.tick() => app.on_tick(Instant::now(), chrono::Local::now()),
        };
        runtime.dispatch(effects);
    }
    Ok(())
}

struct Runtime {
    tx: Tx,
    client: Option<Arc<Client>>,
}

impl Runtime {
    fn dispatch(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            self.run(effect);
        }
    }

    /// Returns the logged-in client, or reports the problem to the app.
    fn client(&self, on_missing: impl FnOnce(String) -> Msg) -> Option<Arc<Client>> {
        if self.client.is_none() {
            send(
                &self.tx,
                on_missing("Not logged in; run `thu-learn login`, then press r.".into()),
            );
        }
        self.client.clone()
    }

    fn run(&mut self, effect: Effect) {
        let tx = self.tx.clone();
        match effect {
            Effect::CheckSession => {
                tokio::spawn(async move {
                    match check_session().await {
                        Ok(client) => {
                            let _ = tx.send(Incoming::Client(Arc::new(client)));
                            send(&tx, Msg::SessionReady);
                        }
                        Err(reason) => send(&tx, Msg::SessionFailed(reason)),
                    }
                });
            }
            Effect::LoadAll => {
                let Some(client) = self.client(|e| Msg::Courses(Err(e))) else {
                    return;
                };
                tokio::spawn(load_all(client, tx));
            }
            Effect::LoadHomework { courses } => {
                let Some(client) = self.client(|e| Msg::Homework(Err(e))) else {
                    return;
                };
                tokio::spawn(async move {
                    let result = client.homework_list(&courses).await.map_err(err_text);
                    send(&tx, Msg::Homework(result));
                });
            }
            Effect::LoadHomeworkDetail {
                course_id,
                student_homework_id,
                base_id,
            } => {
                let id = student_homework_id.clone();
                let Some(client) = self.client(|e| Msg::HomeworkDetail { id, result: Err(e) })
                else {
                    return;
                };
                tokio::spawn(async move {
                    let (description, attachments) = tokio::join!(
                        client.homework_detail(&base_id),
                        client.homework_attachments(&course_id, &student_homework_id),
                    );
                    let result = match (description, attachments) {
                        (Err(e), Err(_)) => Err(err_text(e)),
                        (description, attachments) => Ok(HomeworkDetail {
                            description: description.unwrap_or_default(),
                            attachments: attachments.unwrap_or_default(),
                        }),
                    };
                    send(
                        &tx,
                        Msg::HomeworkDetail {
                            id: student_homework_id,
                            result,
                        },
                    );
                });
            }
            Effect::DownloadFile { file, dir } => {
                let Some(client) = self.client(download_failed) else {
                    return;
                };
                tokio::spawn(async move {
                    let msg = match download_file(&client, &file, &dir).await {
                        Ok(path) => Msg::Downloaded {
                            saved: vec![path],
                            failed: Vec::new(),
                        },
                        Err(e) => download_failed(err_text(e)),
                    };
                    send(&tx, msg);
                });
            }
            Effect::DownloadFiles { files, dir } => {
                let Some(client) = self.client(download_failed) else {
                    return;
                };
                tokio::spawn(async move {
                    let msg = download_files(&client, &files, &dir, &tx).await;
                    send(&tx, msg);
                });
            }
            Effect::DownloadAttachments { attachments, dir } => {
                let Some(client) = self.client(download_failed) else {
                    return;
                };
                tokio::spawn(async move {
                    let msg = download_attachments(&client, &attachments, &dir).await;
                    send(&tx, msg);
                });
            }
            Effect::Submit {
                student_homework_id,
                file,
                comment,
            } => {
                let id = student_homework_id.clone();
                let Some(client) = self.client(|e| Msg::Submitted { id, result: Err(e) }) else {
                    return;
                };
                tokio::spawn(async move {
                    let result = client
                        .submit_homework(&student_homework_id, &file, &comment)
                        .await
                        .map_err(err_text);
                    send(
                        &tx,
                        Msg::Submitted {
                            id: student_homework_id,
                            result,
                        },
                    );
                });
            }
            Effect::Copy(text) => {
                // The escape is written here, between frames on the UI thread,
                // so it cannot interleave with ratatui's output.
                write_osc52(&text);
                std::thread::spawn(move || copy_with_tools(&text));
            }
            Effect::OpenPath(path) => {
                if let Err(e) = open_path(&path) {
                    send(&tx, Msg::OpenFailed(e));
                }
            }
        }
    }
}

async fn check_session() -> Result<Client, String> {
    // Like the CLI, a missing or expired session is renewed with saved credentials,
    // so a missing cookie file is not an error by itself.
    let client = Client::new(paths::cookie_path()).map_err(err_text)?;
    match client.confirm_session().await {
        Ok(()) => Ok(client),
        Err(e) => Err(session_failure(&e)),
    }
}

/// Explains a failed session check. The session screen already says to run
/// `thu-learn login` and press `r`, so authentication errors leave that out.
fn session_failure(e: &anyhow::Error) -> String {
    match e.downcast_ref::<AuthError>() {
        Some(AuthError::SessionExpired) => {
            "Not logged in, or the session expired and no credentials are saved.".into()
        }
        Some(AuthError::ReloginFailed(reason)) => {
            format!("The session expired and automatic re-login failed: {reason}.")
        }
        None => format!("The saved session could not be verified ({e:#})."),
    }
}

/// Loads courses first, then homework, announcements, and files concurrently.
/// Each list is reported as soon as it arrives.
async fn load_all(client: Arc<Client>, tx: Tx) {
    let courses = async {
        let semester = client.current_semester().await?;
        let current = client.course_list(&semester).await?;
        let previous = match crate::cli::prev_semester(&semester) {
            Some(prev) => client.course_list(&prev).await.unwrap_or_default(),
            None => Vec::new(),
        };
        anyhow::Ok(CourseData {
            semester,
            current,
            previous,
        })
    }
    .await;
    let data = match courses {
        Ok(data) => data,
        Err(e) => {
            send(&tx, Msg::Courses(Err(err_text(e))));
            return;
        }
    };
    let all: Vec<Course> = data.current.iter().chain(&data.previous).cloned().collect();
    let current = data.current.clone();
    send(&tx, Msg::Courses(Ok(data)));

    let client = &*client;
    let homework = async {
        let result = client.homework_list(&all).await.map_err(err_text);
        send(&tx, Msg::Homework(result));
    };
    let announcements = async {
        let result = client.notification_list(&current).await.map_err(err_text);
        send(&tx, Msg::Announcements(result));
    };
    let files = async {
        let results = futures::future::join_all(
            current
                .iter()
                .map(|c| async move { (c, client.file_list(c).await) }),
        )
        .await;
        let mut files = Vec::new();
        let mut failures = Vec::new();
        for (course, result) in results {
            match result {
                Ok(mut list) => files.append(&mut list),
                Err(e) => failures.push(format!("{}: {}", course.name, err_text(e))),
            }
        }
        send(&tx, Msg::Files { files, failures });
    };
    tokio::join!(homework, announcements, files);
}

async fn download_file(client: &Client, file: &CourseFile, dir: &Path) -> Result<PathBuf> {
    let url = client.file_download_url(&file.id);
    let (bytes, server_name) = client.fetch_download(&url).await?;
    let name = server_name.unwrap_or_else(|| {
        let ext = file.file_type.trim();
        if ext.is_empty() || file.title.ends_with(&format!(".{ext}")) {
            file.title.clone()
        } else {
            format!("{}.{ext}", file.title)
        }
    });
    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("Could not create {}", dir.display()))?;
    let dest = claim_unique_path(dir, &safe_file_name(&name))
        .with_context(|| format!("Could not create a file in {}", dir.display()))?;
    if let Err(e) = tokio::fs::write(&dest, &bytes).await {
        tokio::fs::remove_file(&dest).await.ok();
        return Err(e).with_context(|| format!("Could not write {}", dest.display()));
    }
    Ok(dest)
}

/// Batch downloads go to one folder per course.
fn course_folder(file: &CourseFile) -> String {
    let name = if file.course_name.trim().is_empty() {
        &file.course_id
    } else {
        &file.course_name
    };
    safe_file_name(name)
}

/// Downloads course files into one folder per course, a few at a time, and
/// reports progress after each file.
async fn download_files(client: &Client, files: &[CourseFile], dir: &Path, tx: &Tx) -> Msg {
    let total = files.len();
    // Owned items: with borrowed ones, rustc cannot show that the spawned
    // future is `Send` ("implementation of `FnOnce` is not general enough").
    let mut downloads = futures::stream::iter(files.to_vec())
        .map(|file| async move {
            let result = download_file(client, &file, &dir.join(course_folder(&file))).await;
            (file.title, result)
        })
        .buffer_unordered(DOWNLOAD_CONCURRENCY);
    let mut results = Vec::with_capacity(total);
    while let Some(result) = downloads.next().await {
        results.push(result);
        send(
            tx,
            Msg::DownloadProgress {
                done: results.len(),
                total,
            },
        );
    }
    downloaded(results)
}

/// Downloads attachments concurrently without overwriting existing files.
async fn download_attachments(
    client: &Client,
    attachments: &[HomeworkAttachment],
    dir: &Path,
) -> Msg {
    if let Err(e) = tokio::fs::create_dir_all(dir).await {
        return download_failed(format!("Could not create {}: {e}", dir.display()));
    }
    let results = futures::future::join_all(attachments.iter().map(|a| async move {
        let result = async {
            // Claimed in order on the first poll, so equal names number predictably.
            let dest = claim_unique_path(dir, &safe_file_name(&a.filename))
                .with_context(|| format!("Could not create a file in {}", dir.display()))?;
            if let Err(e) = client.download(&a.download_path, &dest).await {
                tokio::fs::remove_file(&dest).await.ok();
                return Err(e);
            }
            Ok(dest)
        }
        .await;
        (a.filename.clone(), result)
    }))
    .await;
    downloaded(results)
}

/// Opens a file or folder with the desktop's default application.
fn open_path(path: &Path) -> Result<(), String> {
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(target_os = "windows") {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Copies like yazi, first with an OSC 52 escape for the terminal (this also
/// works over SSH).
fn write_osc52(text: &str) {
    use std::io::Write;
    let osc = format!("\x1b]52;c;{}\x1b\\", base64(text.as_bytes()));
    let mut out = std::io::stdout();
    out.write_all(osc.as_bytes()).ok();
    out.flush().ok();
}

/// Then with the first desktop clipboard tool that succeeds. A tool that has
/// not exited after two seconds is killed, so it cannot hang the app on exit.
fn copy_with_tools(text: &str) {
    use std::io::Write;
    let tools: [(&str, &[&str]); 5] = [
        ("pbcopy", &[]),
        ("termux-clipboard-set", &[]),
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["-ib"]),
    ];
    for (tool, args) in tools {
        let child = std::process::Command::new(tool)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        let Ok(mut child) = child else {
            continue;
        };
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes()).ok();
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    child.kill().ok();
                    child.wait().ok();
                    break None;
                }
            }
        };
        if status.is_some_and(|s| s.success()) {
            return;
        }
    }
}

/// Standard base64 with padding, for OSC 52.
fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(super::base64("作业".as_bytes()), "5L2c5Lia");
    }

    #[test]
    fn session_failure_leaves_login_advice_to_the_screen() {
        use super::{session_failure, AuthError};

        let expired = session_failure(&AuthError::SessionExpired.into());
        assert!(expired.contains("no credentials are saved"));
        assert!(!expired.contains("thu-learn login"));

        let relogin = session_failure(&AuthError::ReloginFailed("timed out".into()).into());
        assert_eq!(
            relogin,
            "The session expired and automatic re-login failed: timed out."
        );

        let network = session_failure(&anyhow::anyhow!("connection refused"));
        assert_eq!(
            network,
            "The saved session could not be verified (connection refused)."
        );
    }

    #[test]
    fn batch_downloads_use_a_safe_course_folder() {
        use super::course_folder;
        use crate::models::CourseFile;

        let file = |course_id: &str, course_name: &str| CourseFile {
            id: "f".into(),
            course_id: course_id.into(),
            course_name: course_name.into(),
            title: "slides".into(),
            size: String::new(),
            file_type: "pdf".into(),
            upload_time: String::new(),
            description: String::new(),
        };
        assert_eq!(course_folder(&file("c1", "数据结构")), "数据结构");
        assert_eq!(
            course_folder(&file("c1", "C/C++ 程序设计")),
            "C_C++ 程序设计"
        );
        assert_eq!(course_folder(&file("c1", "  ")), "c1");
    }

    #[test]
    fn download_results_keep_failure_labels() {
        use super::{downloaded, Msg};
        use std::path::PathBuf;

        let msg = downloaded([
            ("a.pdf".to_string(), Ok(PathBuf::from("/d/a.pdf"))),
            ("b.pdf".to_string(), Err(anyhow::anyhow!("HTTP 404"))),
        ]);
        let Msg::Downloaded { saved, failed } = msg else {
            panic!("expected Downloaded");
        };
        assert_eq!(saved, vec![PathBuf::from("/d/a.pdf")]);
        assert_eq!(failed, vec!["b.pdf: HTTP 404".to_string()]);
    }
}
