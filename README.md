# thu-learn-cli

`thu-learn-cli` is a Rust client for Tsinghua University's Web Learning (<https://learn.tsinghua.edu.cn>). It helps students inspect homework deadlines, read announcements, download course files, and submit homework from a terminal.

It has two front ends over the same Learn client:

* **TUI for humans**: run `thu-learn` with no arguments for an interactive, keyboard-driven interface.
* **CLI for scripts and agents**: subcommands such as `thu-learn hw --json` print stable, scriptable output.

## Features

* Interactive TUI with homework, announcement, file, and course tabs, live search, course filters, downloads, and submission.
* Browser-based login through the official Tsinghua authentication flow.
* Stays logged in: like learnX, an expired Learn session is renewed automatically in the background.
* Homework listing, detail view, attachment download, and submission.
* Course announcement listing and detail view.
* Course file listing, detail view, and download.
* Structured JSON output for scriptable commands.

## Installation

Build the release binary from the repository root:

```bash
cargo build --release
```

The binary is written to `target/release/thu-learn`.

## TUI

```bash
thu-learn            # in a terminal; same as `thu-learn tui`
thu-learn tui -d ~/Downloads
```

The screen shows a list on the left and details for the selected item on the right (stacked on narrow terminals). The TUI reuses the session saved by `thu-learn login` and, like the CLI, renews it automatically when it expires. If there is no usable session (or automatic re-login fails), it asks you to run `thu-learn login` and press `r`.

| Key | Action |
| --- | --- |
| `1`-`4`, `Tab`, `←`/`→` | Switch between Homework, Announcements, Files, and Courses |
| `↑`/`↓`, `j`/`k`, `PgUp`/`PgDn`, `g`/`G` | Move the selection |
| `Enter` | Open details full screen (in Courses: show that course's homework) |
| `J`/`K` | Scroll details; `n`/`p` move to the next/previous item while details are open |
| `/` | Search the current list; `Esc` clears the search, then the course filter |
| `c` | Choose a course for every tab |
| `f` | Homework: cycle pending, overdue, done, all. Announcements: toggle unread only |
| `d` | Download the selected homework's attachments or the selected file |
| `s` | Submit a file for the selected homework through the file picker (below) |
| `D` / `o` | Change the download folder / open the last download |
| `r` | Refresh from Learn |
| `?` / `q` | Help / quit |

### Submission picker

Submitting opens a file picker that looks and behaves like [yazi](https://github.com/sxyazi/yazi) with its default keymap: parent, current, and preview columns (1:4:3), yazi's header and status bar, and the same keys. Press `~` or `F1` inside the picker for the full list.

| Keys | Action |
| --- | --- |
| `j`/`k`, `↑`/`↓` | Next/previous file (wraps around) |
| `gg`/`G`, `C-u`/`C-d`, `C-b`/`C-f` | Top/bottom, half page, full page |
| `h`/`l`, `←`/`→` | Parent folder / enter folder |
| `H`/`L` | Back/forward in folder history |
| `Enter`, `o` | Choose the selected or hovered file (on a folder: enter it) |
| `Space`, `C-a`, `C-r` | Toggle selection, select all, invert |
| `f` | Filter the folder (regex, smart case, remembered per folder) |
| `/`, `?`, `n`, `N` | Find next/previous, jump to next/previous match |
| `Esc` | Clear the find, then the filter, then the selection |
| `.` | Show hidden files |
| `,m` `,b` `,e` `,a` `,n` `,s` `,r` (uppercase reverses) | Sort by mtime, birth time, extension, name, natural, size, random |
| `ms` `mp` `mb` `mm` `mo` `mn` | Linemode: size, permissions, birth time, mtime, owner, none |
| `gh` `gc` `gd` `g<Space>` `gf` | Go home, `~/.config`, `~/Downloads`, typed path (with completion), follow symlink |
| `cc` `cd` `cf` `cn` | Copy path, folder, file name, name without extension |
| `J`/`K` | Scroll the preview |
| `Tab` | File details (spot) |
| `q`, `C-c` | Cancel the submission |

Input popups work like yazi's: they open in insert mode, `Esc` switches to a vi-style normal mode, and a second `Esc` closes them. Yanked or cut text goes to the system clipboard, and `p`/`P` paste text yanked, cut, or copied inside the picker (they do not read the system clipboard). After choosing a file, type an optional comment and press `Enter`; the confirmation shows the course, homework, file, and size before anything is sent, and `n` returns to the picker as you left it. Dragging a file into the terminal chooses it.

Differences from yazi, because the picker uploads exactly one file: `Enter` on a folder enters it instead of choosing it, choosing more than one file is refused, and keys that change files, run shell commands, use tabs, or call plugins (fd, rg, fzf, zoxide) show a notice instead. Folder listings refresh about once a second while the picker is open. The picker opens in the download folder and then remembers the last folder you submitted from.

Downloads go to the current directory unless `--dir` or `D` says otherwise, and never overwrite an existing file (a ` (1)` suffix is added). Detail panes show each item's short ID so it can be passed to CLI commands.

When stdout is not a terminal, `thu-learn` without a command prints help instead of starting the TUI, so scripts and agents never get stuck in an interactive screen.

## Authentication

Run:

```bash
thu-learn login
```

The command asks for your Tsinghua username and password, then opens a Google Chrome window on Tsinghua's normal login page with both fields filled in. Complete any captcha and two-factor check there. When Tsinghua asks whether to trust the device, the CLI answers "yes" for its own random device ID. After the Learn home page is reached, the CLI checks that it can log in again over plain HTTP, and only then saves the credentials.

### Why you stay logged in

Learn sessions live on the server and expire after a period of inactivity, so saved cookies alone stop working quickly. thu-learn-cli handles this the way [learnX](https://github.com/robertying/learnX) does:

1. The first login trusts a random device ID (`fingerPrint`) during two-factor authentication.
2. The username, password, and that device ID are saved in the system credential store.
3. When a request finds the session expired (Learn redirects to `login_timeout` or answers HTTP 403), the CLI logs in to `id.tsinghua.edu.cn` again over HTTP. The password is SM2-encrypted exactly like the official login form does it. The CLI then opens Learn with the returned ticket and retries the request. Because the device ID is trusted, no two-factor prompt is needed.

You only need to run `thu-learn login` again if your password changes, or if Tsinghua stops trusting the device (for example, after you remove it from your account). The next `thu-learn login` reuses the saved device ID, so it usually succeeds without opening Chrome; use `thu-learn login --browser` to go through Chrome anyway, for example when Tsinghua asks for a captcha.

Failed logins are not repeated blindly, because repeated failures can make Tsinghua require a captcha. A network problem is retried after 30 seconds. Credentials that Tsinghua rejects (wrong password, or the device is no longer trusted) are not tried again, not even by later commands, until the next successful `thu-learn login`. This is recorded in `~/.config/thu-learn-cli/relogin-blocked.json`, which holds only a hash of the rejected credentials.

### Storage and options

* Credentials are stored in the macOS Keychain, Windows Credential Manager, or the Secret Service keyring on Linux. Only if no such store exists (for example, a headless Linux machine) do they go to `~/.config/thu-learn-cli/credentials.json`, which only your user can read. A locked keyring, or a denied access prompt, is reported as an error instead. Set `THU_LEARN_CREDENTIAL_STORE=file` to always use the file, or `keyring` to require the system store.
* `thu-learn login --no-remember` keeps the old behavior: only browser cookies are saved, no password is stored, and any previously saved credentials are removed.
* `thu-learn logout` ends the Learn session and deletes saved credentials (from the keyring and the fallback file), cookies, and cache. If any copy cannot be removed, it says so and exits with an error. Tsinghua may keep listing the device as trusted until you remove it from your account.

Requirements and storage behavior:

* Google Chrome must be installed locally.
* Chromedriver is downloaded and managed by `thirtyfour::WebDriver::managed`.
* Session cookies are stored at `~/.config/thu-learn-cli/cookies.json` with owner-only permissions.
* Semester and course-list cache files are stored under `~/.cache/thu-learn-cli/`.
* Running `thu-learn login` clears the cache after logging in.

## CLI Usage

List entries show a seven-character short ID. Use that short ID, a unique short-ID prefix, or a full server ID for detail, download, and submit commands where supported.

```bash
thu-learn login

# Courses
thu-learn courses

# Homework
thu-learn hw
thu-learn hw -a
thu-learn hw --overdue
thu-learn hw -c <course>
thu-learn hw <id>
thu-learn hw <id> -d ./dir

# Announcements
thu-learn ann
thu-learn ann -c <course>
thu-learn ann <id>

# Course files
thu-learn f ls
thu-learn f ls -c <course>
thu-learn f show <id>
thu-learn f get <id>

# Homework submission
thu-learn submit <id> ./hw.pdf -c "submission note"

# JSON output
thu-learn hw --json
```

Notes:

* Human output uses color when stdout supports it; `--json` output is plain structured data.
* The current semester and course list are cached for six hours.
* Homework, announcement, and file requests are intentionally concurrent.
* Expired sessions are renewed automatically when credentials are saved. If automatic re-login fails, or after `login --no-remember`, commands ask you to run `thu-learn login` again.

## Development

Recommended local checks:

```bash
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo build --release
```

Focused examples:

```bash
cargo test api::tests::parse_deadline_invalid
cargo test client::tests::csrf_extracted
cargo test cli::tests::prev_semester_invalid
cargo test client::tests::expired_session_is_renewed_once_for_concurrent_requests
cargo test sso::tests::sm_crypto_fixture_decrypts_with_c1c3c2_layout
```

Re-login tests run against a local mock of Learn and `id.tsinghua.edu.cn` (`src/mock_sso.rs`), so they need no network access or account.

The hidden `thu-learn debug` command prints selected raw Learn API responses for field investigation. It requires a valid login session.

## Data Safety

`~/.config/thu-learn-cli/cookies.json` contains session credentials, and the `thu-learn-cli` entry in the system credential store (or the fallback `~/.config/thu-learn-cli/credentials.json`) contains your Tsinghua password and trusted device ID. Do not print them, copy them into a repository, commit them, or share them. Rebuildable cache data is stored under `~/.cache/thu-learn-cli/`. Automated tests should not require a live Web Learning account, Chrome, chromedriver, the system credential store, or an existing cookie file.

## Acknowledgements

* Learn API behavior references [thu-learn-lib](https://github.com/Harry-Chen/thu-learn-lib) (MIT, copyright 2019-2022 Harry Chen).
* Browser-login behavior references [reserves-lib-tsinghua-downloader](https://github.com/libthu/reserves-lib-tsinghua-downloader) (GPL-3.0).
* Silent re-login behavior (trusted device ID, SM2 password encryption, ticket roaming) references [learnX](https://github.com/robertying/learnX) and its [thu-learn-lib fork](https://github.com/robertying/thu-learn-lib) (MIT).
* The submission picker follows [yazi](https://github.com/sxyazi/yazi)'s default keymap and behavior, and ports some of its code (MIT, copyright 2023 sxyazi). See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## License

This project is licensed under [GPL-3.0-or-later](LICENSE).

Use this tool only with your own Tsinghua account and follow the Learn platform terms of service. Do not use it for bulk scraping or abuse.
