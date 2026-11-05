# AGENTS.md

## Project Facts

* Single Rust 2021 binary crate named `thu-learn`; `src/main.rs` is the only binary entrypoint and release output is `target/release/thu-learn`.
* The CLI talks to Tsinghua Learn at `learn.tsinghua.edu.cn`; project-owned docs, comments, help text, and runtime output should be English. Preserve Chinese only for Learn protocol literals and realistic fixtures, and document those cases when they appear.

## Module Boundaries

* `src/cli.rs`: clap command surface, aliases, human vs `--json` output, login/logout prompts, cache clearing, and command orchestration.
* `src/client.rs`: reqwest client, cookie store, `_csrf` extraction, authenticated GET/POST/download helpers, cookie persistence, and silent re-login (`send_authed`, `renew_session`, `sso_login`).
* `src/api.rs`: Learn API endpoints plus JSON/HTML parsing for courses, homework, announcements, files, and submit.
* `src/auth.rs`: remembered-login orchestration for `thu-learn login`: saved device trust first, then the browser, then an HTTP re-login check before saving credentials.
* `src/sso.rs`: `id.tsinghua.edu.cn` helpers: SM2 password encryption, public-key/ticket parsing, failure classification, device fingerprint and name.
* `src/credentials.rs`: saved credentials in the system credential store (`keyring` crate) with an owner-only file fallback.
* `src/browser_login.rs` and `src/sso_inject.js`: Chrome WebDriver login; the injected script pre-fills credentials and trusts the CLI device ID like learnX's `sso.js`. `thirtyfour::WebDriver::managed` downloads/starts chromedriver, but local Google Chrome is still required.
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
* Do not make automated tests require live `learn.tsinghua.edu.cn`, a real Tsinghua account, Chrome, chromedriver, or an existing `cookies.json`.

## Learn API Quirks

* Network Learning JSON fields are pinyin abbreviations and can vary; keep the `serde_json::Value` plus candidate-key parsing style in `src/api.rs`.
* Explain pinyin API fields near parsing/model code with both Chinese and English names, for example `xszyid`: 学生作业 ID / student homework ID.
* Session cookies are nonpersistent. `src/client.rs` intentionally saves with `save_incl_expired_and_nonpersistent_json` and loads with `load_json_all`; replacing these with ordinary cookie-store save/load breaks login reuse.
* Authenticated GET, POST, and download requests need `_csrf` from the course page before calling Learn endpoints.
* Silent re-login follows thu-learn-lib (robertying fork): clear the ID `JSESSIONID`, read `#sm2publicKey` from the login form, POST `i_user`, `i_pass` = `04` + SM2 C1C3C2 hex, `singleLogin=on`, `fingerPrint`, `fingerGenPrint`, `fingerGenPrint3`, `i_captcha` to `/do/off/ui/auth/login/check`, then GET `/b/j_spring_security_thauth_roaming_entry?ticket=...`. Learn reports an expired session by redirecting to `login_timeout` or answering 403.
* `Client` performs at most one re-login per expired session generation and caches a failure, so concurrent requests never retry a rejected password (repeated failures can trigger a captcha). POST bodies are passed as `Fn() -> Form` because a retry needs a fresh multipart form.
* Course and announcement/file fetching is intentionally concurrent in several paths; avoid serializing it unless debugging or fixing a measured issue.

## Testing And CLI Behavior

* Existing tests are inline unit tests in `src/api.rs`, `src/client.rs`, `src/sso.rs`, `src/credentials.rs`, `src/browser_login.rs`, and `src/cli.rs`; add pure parser/helper tests near the code being changed.
* HTTP auth flows are tested against `src/mock_sso.rs` on 127.0.0.1; the SM2 fixture in `src/sso.rs` was generated with sm-crypto 0.5.8, the library thu-learn-lib uses.
* Use `--json` for scriptable CLI checks when supported; human output uses colors only when stdout supports them.
* Hidden command `thu-learn debug` prints raw JSON for field checks, but it requires a valid login session.
