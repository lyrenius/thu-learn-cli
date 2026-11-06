# AGENTS.md

## Project Facts

* Single Rust 2021 binary crate named `thu-learn`; `src/main.rs` is the only binary entrypoint and release output is `target/release/thu-learn`.
* The CLI talks to THU Learn at `learn.tsinghua.edu.cn`; project-owned docs, comments, help text, and runtime output should be English. Preserve Chinese only for Learn protocol literals and realistic fixtures, and document those cases when they appear.

## Module Boundaries

* `src/cli.rs`: clap command surface, aliases, human vs `--json` output, login/logout prompts, cache clearing, and command orchestration. Bare `thu-learn` starts the TUI only when stdin and stdout are terminals; otherwise it prints help.
* `src/tui/`: interactive ratatui front end for humans. `app.rs` is state that turns keys and background results into `Effect`s (unit-testable, no network I/O); `mod.rs` runs the event loop and executes effects with the shared `Client`; `ui.rs` renders; `picker/` is the submission file picker that mirrors yazi's default keymap and behavior (`mod.rs` state and actions, `keymap.rs` bindings, `view.rs` rendering, `editor.rs` vi-style input, `fs.rs` listing/sorting/metadata, `scroll.rs` and `natsort.rs` ported from yazi); `input.rs` holds the text field and path helpers. The TUI never asks for credentials: it reuses the saved session, which `Client` renews silently when credentials are saved, and otherwise points to `thu-learn login`. The CLI stays the interface for scripts and agents, so keep its output and flags stable when changing the TUI.
* `src/client.rs`: reqwest client, cookie store, `_csrf` extraction, authenticated GET/POST/download helpers, cookie persistence, and silent re-login (`send_authed`, `renew_session`, `sso_login`).
* `src/api.rs`: Learn API endpoints plus JSON/HTML parsing for courses, homework, announcements, files, and submit.
* `src/auth.rs`: remembered-login orchestration for `thu-learn login`: saved device trust first, then the browser, then an HTTP re-login check before saving credentials.
* `src/sso.rs`: `id.tsinghua.edu.cn` helpers: SM2 password encryption, public-key/ticket parsing, failure classification, device fingerprint and name.
* `src/credentials.rs`: saved credentials in the system credential store (`keyring` crate) with an owner-only file fallback.
* `src/browser_login.rs` and `src/sso_inject.js`: Chrome WebDriver login; the injected script acts only on `https://id.tsinghua.edu.cn`, pre-fills credentials, and trusts the CLI device ID like learnX's `sso.js`. It records the submit in localStorage and in a `thu_learn_cli_sso` cookie, because the check can redirect straight to Learn. `unhandledPromptBehavior` is `ignore` so page dialogs stay with the user, and only a reached Learn page counts as a finished login. `thirtyfour::WebDriver::managed` downloads/starts chromedriver, but local Google Chrome is still required.
* `src/cache.rs` and `src/paths.rs`: current-semester/course cache, `cookies.json`/`credentials.json` locations, and atomic owner-only writes; `src/models.rs` owns serialized output models and short ids.
* `src/mock_sso.rs` (tests only): local Learn and ID mock servers for re-login tests.

## Local Checks

* Run from the repo root: `cargo test`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, then `cargo build --release`.
* Focused examples: `cargo test api::tests::parse_deadline_invalid`, `cargo test client::tests::csrf_extracted`, `cargo test cli::tests::prev_semester_invalid`.
* There is no CI, task runner, rustfmt config, or clippy config in this repo; trust Cargo commands and source tests over assumptions.

## Runtime And Data Safety

* Session cookies are stored at `~/.config/thu-learn-cli/cookies.json`; this file contains session credentials and must never be printed, copied into the repo, committed, shared, or exposed.
* Saved credentials (username, password, trusted `fingerPrint`) live in the system credential store under service `thu-learn-cli`, or in `~/.config/thu-learn-cli/credentials.json` when no store is available (`THU_LEARN_CREDENTIAL_STORE=auto|keyring|file`). Never print or log them; `Credentials` has a redacting `Debug`.
* Tests must not touch the real credential store: build clients with `Client::with_endpoints` and inject credentials with `set_credentials`.
* The cache is under `~/.cache/thu-learn-cli/`; `thu-learn login` clears it after importing cookies.
* The old `./cookies.json` file and old macOS app support cache are not migrated automatically, so users may need to log in again.
* Do not make automated tests require live `learn.tsinghua.edu.cn`, a real THU account, Chrome, chromedriver, or an existing `cookies.json`.

## Learn API Quirks

* Network Learning JSON fields are pinyin abbreviations and can vary; keep the `serde_json::Value` plus candidate-key parsing style in `src/api.rs`.
* Explain pinyin API fields near parsing/model code with both Chinese and English names, for example `xszyid`: 学生作业 ID / student homework ID.
* Session cookies are nonpersistent. `src/client.rs` intentionally saves with `save_incl_expired_and_nonpersistent_json` and loads with `load_json_all`; replacing these with ordinary cookie-store save/load breaks login reuse.
* Authenticated GET, POST, and download requests need `_csrf` from the course page before calling Learn endpoints.
* Silent re-login follows thu-learn-lib (robertying fork): clear the ID `JSESSIONID`, read `#sm2publicKey` from the login form, POST `i_user`, `i_pass` = `04` + SM2 C1C3C2 hex, `singleLogin=on`, `fingerPrint`, `fingerGenPrint`, `fingerGenPrint3`, `i_captcha` to `/do/off/ui/auth/login/check`, then GET `/b/j_spring_security_thauth_roaming_entry?ticket=...`. Learn reports an expired session by redirecting to `login_timeout` or answering 403.
* `Client` performs at most one re-login per expired session generation. A transient failure is shared by all requests for 30 s, then retried. A definitive rejection (`SsoFailure::Rejected`, `NeedsTwoFactor`, `NoTicket`) is never retried for the same credentials: it is cached in memory and, for rejected passwords and untrusted devices, written as a credential hash to `relogin-blocked.json` next to `cookies.json`, so later processes do not retry it either (repeated failures can trigger a captcha). A successful login or logout clears the marker. POST bodies are passed as `Fn() -> Form` because a retry needs a fresh multipart form.
* By default `Client` re-reads the credential store on each renewal, so `login`/`logout` in another process take effect; `set_credentials` pins credentials (tests, login verification), `use_saved_credentials` returns to the store, and `logout` disables renewal. Logins are bounded by connect/read timeouts and a 60 s overall timeout, and auth-path errors drop request URLs (they can carry the ticket or `_csrf`).
* Course and announcement/file fetching is intentionally concurrent in several paths; avoid serializing it unless debugging or fixing a measured issue.

## Testing And CLI Behavior

* Existing tests are inline unit tests in `src/api.rs`, `src/client.rs`, `src/sso.rs`, `src/credentials.rs`, `src/browser_login.rs`, `src/cli.rs`, and `src/tui/`; add pure parser/helper tests near the code being changed.
* HTTP auth flows are tested against `src/mock_sso.rs` on 127.0.0.1; the SM2 fixture in `src/sso.rs` was generated with sm-crypto 0.5.8, the library thu-learn-lib uses.
* Code ported from yazi keeps its MIT notice in the file header and is listed in `THIRD_PARTY_NOTICES.md`; keep both in sync when porting more. Picker behavior should match yazi's defaults unless the README lists the difference.
* TUI tests drive `App` with key events and render `ui::draw` into ratatui's `TestBackend`; they use fixtures from `tui::app::tests`, never a live session.
* All server text shown in the TUI goes through `cli::strip_terminal_controls` (via `ui::clean`/`ui::clean_lines`) so Learn content cannot inject terminal escape sequences.
* Use `--json` for scriptable CLI checks when supported; human output uses colors only when stdout supports them.
* Hidden command `thu-learn debug` prints raw JSON for field checks, but it requires a valid login session.
