//! The homework submission file picker, modelled on yazi: parent, current, and
//! preview columns, yazi's default keymap, per-directory memory of cursor and
//! filter, back/forward history, selection, filter and find, sorting,
//! linemodes, a spot popup, and vi-style input popups.
//!
//! Deliberate differences from yazi, because the picker chooses one file to
//! upload: `Enter` on a directory enters it (yazi would choose the directory),
//! choosing more than one file is refused, file-changing actions, shell
//! commands, tabs, and plugins are unavailable, and dragging a file into the
//! terminal chooses it.

pub mod editor;
pub mod fs;
pub mod keymap;
mod natsort;
pub mod scroll;
pub mod view;

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use editor::{Edit, Editor, Mode};
use fs::{Entry, Preview, SortBy, Sorter};
use keymap::{Action, CopyWhat, Goto, Key, Linemode, Resolved};
use scroll::{folder_scrolloff, Scroll, Step};

/// Directories whose cursor and filter are remembered.
const MEMO_MAX: usize = 100;
const NOTICE_TTL: Duration = Duration::from_secs(5);
/// Completion candidates shown at most, and rows visible at once.
const CMP_MAX: usize = 30;
pub const CMP_ROWS: usize = 10;

/// A smart-case regex: case-insensitive unless the query has an uppercase letter.
#[derive(Debug, Clone)]
pub struct Matcher {
    pub raw: String,
    re: regex::Regex,
}

impl Matcher {
    pub fn new(raw: &str) -> Option<Self> {
        let sensitive = raw.chars().any(char::is_uppercase);
        let re = regex::RegexBuilder::new(raw)
            .case_insensitive(!sensitive)
            .size_limit(1 << 20)
            .build()
            .ok()?;
        Some(Self {
            raw: raw.to_string(),
            re,
        })
    }

    pub fn is_match(&self, name: &str) -> bool {
        self.re.is_match(name)
    }

    /// Byte range of the first match, for highlighting.
    pub fn find(&self, name: &str) -> Option<std::ops::Range<usize>> {
        self.re.find(name).map(|m| m.range())
    }
}

/// One directory: its entries, the visible subset, and cursor state.
#[derive(Debug, Clone)]
pub struct Folder {
    pub path: PathBuf,
    /// All entries, sorted.
    all: Vec<Entry>,
    /// Entries after the hidden-file rule and the filter.
    pub items: Vec<Entry>,
    pub filter: Option<Matcher>,
    pub scroll: Scroll,
    /// Name of the entry last hovered, followed across re-sorts and reloads.
    trace: Option<OsString>,
    pub error: Option<String>,
    /// Directory modification time when listed, to notice changes.
    mtime: Option<std::time::SystemTime>,
}

fn dir_mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl Folder {
    pub fn hovered(&self) -> Option<&Entry> {
        self.items.get(self.scroll.cursor)
    }

    fn rebuild(&mut self, show_hidden: bool) {
        let filter = self.filter.as_ref();
        self.items = self
            .all
            .iter()
            .filter(|e| show_hidden || !e.hidden)
            .filter(|e| filter.is_none_or(|f| f.is_match(&e.name)))
            .cloned()
            .collect();
    }

    fn arrow(&mut self, step: Step, limit: usize) -> bool {
        self.scroll
            .arrow(step, self.items.len(), limit, folder_scrolloff(limit))
    }

    fn retrace(&mut self) {
        if let Some(h) = self.hovered() {
            self.trace = Some(h.os_name.clone());
        }
    }

    /// Moves the cursor to `name`, or keeps the index when it is not listed.
    fn hover(&mut self, name: &OsString, limit: usize) {
        let target = self
            .items
            .iter()
            .position(|e| &e.os_name == name)
            .unwrap_or(self.scroll.cursor);
        self.arrow(
            Step::Offset(target as isize - self.scroll.cursor as isize),
            limit,
        );
        self.retrace();
    }

    /// Re-hovers the traced entry after the list changed.
    fn repos(&mut self, limit: usize) {
        match self.trace.clone() {
            Some(t) => {
                self.hover(&t, limit);
                self.trace = Some(t);
            }
            None => {
                self.arrow(Step::Offset(0), limit);
            }
        }
    }
}

#[derive(Debug, Clone)]
struct Memo {
    scroll: Scroll,
    trace: Option<OsString>,
    filter: Option<Matcher>,
    seq: u64,
}

/// Back/forward history, ported from yazi's `Backstack`
/// (yazi-core/src/tab/backstack.rs, MIT, Copyright (c) 2023 - sxyazi).
#[derive(Debug, Clone)]
pub struct Backstack {
    stack: Vec<PathBuf>,
    cursor: usize,
}

impl Backstack {
    fn new(start: PathBuf) -> Self {
        Self {
            stack: vec![start],
            cursor: 0,
        }
    }

    fn push(&mut self, path: &Path) {
        if self.stack.get(self.cursor).is_some_and(|p| p == path) {
            return;
        }
        self.stack.truncate(self.cursor + 1);
        self.stack.push(path.to_path_buf());
        self.cursor = self.stack.len() - 1;
        if self.stack.len() > 60 {
            let drop = self.cursor.saturating_sub(30);
            self.stack.drain(..drop);
            self.cursor -= drop;
        }
    }

    fn back(&mut self) -> Option<PathBuf> {
        self.cursor = self.cursor.checked_sub(1)?;
        self.stack.get(self.cursor).cloned()
    }

    fn forward(&mut self) -> Option<PathBuf> {
        let next = self.cursor + 1;
        let path = self.stack.get(next)?.clone();
        self.cursor = next;
        Some(path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
}

#[derive(Debug, Clone)]
pub struct Notice {
    pub title: String,
    pub message: String,
    pub level: Level,
    at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputKind {
    Filter,
    Find {
        previous: bool,
    },
    Cd,
    /// The optional submission comment for the chosen file.
    Comment {
        file: PathBuf,
    },
}

impl InputKind {
    pub fn title(&self) -> &'static str {
        match self {
            InputKind::Filter => "Filter:",
            InputKind::Find { previous: false } => "Find next:",
            InputKind::Find { previous: true } => "Find previous:",
            InputKind::Cd => "Change directory:",
            InputKind::Comment { .. } => "Comment (optional):",
        }
    }
}

/// Completion state for the change-directory input.
#[derive(Debug, Clone, Default)]
pub struct Cmp {
    /// (name, is_dir) candidates that match the typed word.
    pub items: Vec<(String, bool)>,
    pub scroll: Scroll,
    cache: HashMap<PathBuf, Vec<(String, bool)>>,
    /// Text before the last separator, kept literally when completing.
    prefix: String,
}

/// Recursive size of a directory, computed off the UI thread for the spot popup.
#[derive(Debug, Default)]
pub struct SizeJob {
    pub bytes: AtomicU64,
    pub done: AtomicBool,
    cancel: AtomicBool,
}

#[derive(Debug, Clone)]
pub enum Popup {
    Input {
        kind: InputKind,
        editor: Editor,
        cmp: Option<Cmp>,
        invalid: bool,
    },
    Help {
        editor: Editor,
        scroll: Scroll,
        /// Set when the help belongs to the spot popup; closing returns to it.
        spot: Option<SpotState>,
    },
    Spot {
        row: usize,
        size: Option<Arc<SizeJob>>,
        pending_c: bool,
    },
}

/// Preview of the hovered entry, cached until the hover changes.
#[derive(Debug, Clone)]
pub enum PreviewContent {
    Dir(Box<Folder>),
    File(Preview),
}

#[derive(Debug, Clone)]
pub struct PreviewState {
    pub path: PathBuf,
    pub content: PreviewContent,
    /// Lines (files) or entries (directories) scrolled off the top.
    pub skip: usize,
}

/// What the picker asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Cancel,
    Submit { file: PathBuf, comment: String },
    Copy(String),
}

#[derive(Debug, Clone)]
pub struct Picker {
    pub cwd: PathBuf,
    pub current: Folder,
    pub parent: Option<Folder>,
    memos: HashMap<PathBuf, Memo>,
    seq: u64,
    back: Backstack,
    /// Selected paths in selection order.
    pub selected: Vec<PathBuf>,
    pub finder: Option<Matcher>,
    pub sorter: Sorter,
    pub show_hidden: bool,
    pub linemode: Linemode,
    seed: u64,
    pub popup: Option<Popup>,
    pub pending: Vec<Key>,
    pub preview: Option<PreviewState>,
    /// Rows of the current column and of the preview, set by the view.
    pub limit: usize,
    pub preview_limit: usize,
    pub notices: Vec<Notice>,
    history: Vec<String>,
    /// Visible entry counts of directories listed so far, for the size linemode.
    pub counts: HashMap<PathBuf, usize>,
    /// Rows of the help list, set by the view.
    pub help_limit: usize,
    /// Text shared by input yank/cut/paste and the copy keys.
    clipboard: String,
    last_refresh: Instant,
}

/// Terminal cursor shape for the active input: a bar in insert mode and a
/// block in normal mode, as yazi shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Bar,
    Block,
}

/// The spot popup's state, kept while its help is open.
#[derive(Debug, Clone)]
pub struct SpotState {
    pub row: usize,
    pub size: Option<Arc<SizeJob>>,
}

fn absolute(path: &Path) -> PathBuf {
    let p = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    fs::clean(&p)
}

fn seed_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64 | 1)
        .unwrap_or(0x9e37_79b9_7f4a_7c15)
}

impl Picker {
    pub fn open(dir: &Path) -> Self {
        let mut start = absolute(dir);
        while !start.is_dir() && start.pop() {}
        if !start.is_dir() {
            start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        }
        let mut picker = Self {
            current: Folder {
                path: start.clone(),
                all: Vec::new(),
                items: Vec::new(),
                filter: None,
                scroll: Scroll::default(),
                trace: None,
                error: None,
                mtime: None,
            },
            back: Backstack::new(start.clone()),
            cwd: start.clone(),
            parent: None,
            memos: HashMap::new(),
            seq: 0,
            selected: Vec::new(),
            finder: None,
            sorter: Sorter::default(),
            show_hidden: false,
            linemode: Linemode::None,
            seed: seed_now(),
            popup: None,
            pending: Vec::new(),
            preview: None,
            limit: 20,
            preview_limit: 20,
            notices: Vec::new(),
            history: Vec::new(),
            counts: HashMap::new(),
            help_limit: HELP_ROWS,
            clipboard: String::new(),
            last_refresh: Instant::now(),
        };
        picker.current = picker.load(&start);
        picker.parent = picker.load_parent();
        picker
    }

    // ---------- Folders ----------

    fn load(&mut self, path: &Path) -> Folder {
        let (mut all, error) = match fs::list_dir(path) {
            Ok(all) => (all, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        self.sorter.sort(&mut all, &mut self.seed);
        let memo = self.memos.get(path).cloned();
        let mut folder = Folder {
            path: path.to_path_buf(),
            all,
            items: Vec::new(),
            filter: memo.as_ref().and_then(|m| m.filter.clone()),
            scroll: memo.as_ref().map(|m| m.scroll).unwrap_or_default(),
            trace: memo.and_then(|m| m.trace),
            error,
            mtime: dir_mtime(path),
        };
        folder.rebuild(self.show_hidden);
        folder.repos(self.limit);
        if folder.error.is_none() {
            self.counts.insert(path.to_path_buf(), folder.items.len());
        }
        folder
    }

    fn load_parent(&mut self) -> Option<Folder> {
        let parent = self.cwd.parent()?.to_path_buf();
        let mut folder = self.load(&parent);
        if let Some(name) = self.cwd.file_name().map(|n| n.to_os_string()) {
            folder.hover(&name, self.limit);
        }
        Some(folder)
    }

    fn memo(&mut self, folder: &Folder) {
        self.seq += 1;
        self.memos.insert(
            folder.path.clone(),
            Memo {
                scroll: folder.scroll,
                trace: folder.trace.clone(),
                filter: folder.filter.clone(),
                seq: self.seq,
            },
        );
        if self.memos.len() > MEMO_MAX {
            if let Some(oldest) = self
                .memos
                .iter()
                .min_by_key(|(_, m)| m.seq)
                .map(|(p, _)| p.clone())
            {
                self.memos.remove(&oldest);
            }
        }
    }

    /// Changes directory. A missing target falls back to its nearest existing
    /// ancestor, as yazi does after it notices the directory is gone.
    fn cd(&mut self, target: &Path, record: bool) {
        let mut target = absolute(target);
        while !target.is_dir() && target.pop() {}
        if target == self.cwd || target.as_os_str().is_empty() {
            return;
        }
        if record {
            self.back.push(&target);
        }
        let current = self.current.clone();
        self.memo(&current);
        if let Some(parent) = self.parent.clone() {
            self.memo(&parent);
        }
        self.cwd = target.clone();
        self.current = self.load(&target);
        self.parent = self.load_parent();
        self.preview = None;
    }

    /// Goes to a path's directory and hovers it.
    fn reveal(&mut self, path: &Path) {
        let path = absolute(path);
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return;
        };
        let name = name.to_os_string();
        self.cd(parent, true);
        if self.cwd == parent {
            self.current.hover(&name, self.limit);
        }
    }

    /// Re-sorts and re-filters every loaded folder after a preference change.
    fn refresh_prefs(&mut self) {
        let (show_hidden, limit) = (self.show_hidden, self.limit);
        let sorter = self.sorter;
        let mut seed = self.seed;
        for folder in std::iter::once(&mut self.current).chain(self.parent.as_mut()) {
            sorter.sort(&mut folder.all, &mut seed);
            folder.rebuild(show_hidden);
            folder.repos(limit);
        }
        self.seed = seed;
        self.rehover_parent();
        self.preview = None;
    }

    /// The parent column always tracks the current directory, as in yazi.
    fn rehover_parent(&mut self) {
        let name = self.cwd.file_name().map(|n| n.to_os_string());
        if let (Some(parent), Some(name)) = (self.parent.as_mut(), name) {
            parent.hover(&name, self.limit);
        }
    }

    /// Re-lists a folder in place, keeping its filter, cursor, and trace.
    fn relist(&mut self, folder: &mut Folder) {
        let (mut all, error) = match fs::list_dir(&folder.path) {
            Ok(all) => (all, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        self.sorter.sort(&mut all, &mut self.seed);
        folder.all = all;
        folder.error = error;
        folder.mtime = dir_mtime(&folder.path);
        folder.rebuild(self.show_hidden);
        folder.repos(self.limit);
        if folder.error.is_none() {
            self.counts.insert(folder.path.clone(), folder.items.len());
        }
    }

    /// Picks up files created, removed, or changed since the folders were listed.
    fn refresh_changed(&mut self) {
        let hovered_changed = self.hovered().is_some_and(|e| {
            Entry::from_path(&e.path).is_none_or(|now| now.len != e.len || now.mtime != e.mtime)
        });
        if hovered_changed || dir_mtime(&self.current.path) != self.current.mtime {
            let mut current = std::mem::replace(
                &mut self.current,
                Folder {
                    path: PathBuf::new(),
                    all: Vec::new(),
                    items: Vec::new(),
                    filter: None,
                    scroll: Scroll::default(),
                    trace: None,
                    error: None,
                    mtime: None,
                },
            );
            self.relist(&mut current);
            self.current = current;
            self.preview = None;
        }
        if let Some(mut parent) = self.parent.take() {
            if dir_mtime(&parent.path) != parent.mtime {
                self.relist(&mut parent);
            }
            self.parent = Some(parent);
            self.rehover_parent();
        }
        if let Some(PreviewContent::Dir(folder)) = self.preview.as_ref().map(|p| &p.content) {
            if dir_mtime(&folder.path) != folder.mtime {
                self.preview = None;
            }
        }
    }

    /// Called by the view with the current layout.
    pub fn set_limits(&mut self, limit: usize, preview_limit: usize) {
        let limit = limit.max(1);
        if limit != self.limit {
            self.limit = limit;
            self.current.arrow(Step::Offset(0), limit);
            if let Some(p) = self.parent.as_mut() {
                p.arrow(Step::Offset(0), limit);
            }
        }
        self.preview_limit = preview_limit.max(1);
    }

    pub fn hovered(&self) -> Option<&Entry> {
        self.current.hovered()
    }

    fn arrow(&mut self, step: Step) {
        if self.current.arrow(step, self.limit) {
            self.current.retrace();
        }
    }

    // ---------- Preview ----------

    /// Loads the preview for the hovered entry when the hover changed.
    pub fn sync_preview(&mut self) {
        let Some(entry) = self.hovered().cloned() else {
            self.preview = None;
            return;
        };
        if self.preview.as_ref().is_some_and(|p| p.path == entry.path) {
            return;
        }
        let (content, skip) = if entry.is_dir() {
            let folder = self.load(&entry.path);
            let skip = folder.scroll.offset;
            (PreviewContent::Dir(Box::new(folder)), skip)
        } else {
            (PreviewContent::File(fs::preview_file(&entry)), 0)
        };
        self.preview = Some(PreviewState {
            path: entry.path,
            content,
            skip,
        });
    }

    /// yazi's seek: one unit is a tenth of the preview height.
    fn seek(&mut self, units: i32) {
        self.sync_preview();
        let h = self.preview_limit;
        let Some(p) = self.preview.as_mut() else {
            return;
        };
        let raw = (units as isize * h as isize).div_euclid(10);
        match &p.content {
            PreviewContent::File(Preview::Text(lines)) => {
                let step = if raw == 0 {
                    units.signum() as isize
                } else {
                    raw
                };
                let mut skip = p.skip.saturating_add_signed(step);
                if skip > 0 && lines.len() < skip + h {
                    skip = lines.len().saturating_sub(h);
                }
                p.skip = skip;
            }
            PreviewContent::Dir(folder) => {
                let max = folder.items.len().saturating_sub(h);
                p.skip = p.skip.saturating_add_signed(raw).min(max);
            }
            PreviewContent::File(_) => {}
        }
    }

    // ---------- Selection, filter, find ----------

    fn toggle(&mut self, path: &Path) -> bool {
        if let Some(i) = self.selected.iter().position(|p| p == path) {
            self.selected.swap_remove(i);
            return true;
        }
        if self
            .selected
            .iter()
            .any(|s| path.starts_with(s) || s.starts_with(path))
        {
            return false;
        }
        self.selected.push(path.to_path_buf());
        true
    }

    fn escape(&mut self) {
        if self.finder.take().is_some() {
            return;
        }
        if self.current.filter.is_some() {
            self.apply_filter("");
            return;
        }
        self.selected.clear();
    }

    /// Applies a live filter; returns false when the pattern is invalid.
    fn apply_filter(&mut self, raw: &str) -> bool {
        let filter = if raw.is_empty() {
            None
        } else {
            match Matcher::new(raw) {
                Some(m) => Some(m),
                None => return false,
            }
        };
        if self.current.filter.as_ref().map(|f| &f.raw) == filter.as_ref().map(|f| &f.raw) {
            return true;
        }
        let old = self.current.hovered().map(|e| e.os_name.clone());
        self.current.filter = filter;
        self.current.rebuild(self.show_hidden);
        match old {
            Some(name) => self.current.hover(&name, self.limit),
            None => self.current.repos(self.limit),
        }
        true
    }

    /// Live find: jumps to the first match at or after (or before) the cursor.
    fn apply_find(&mut self, raw: &str, previous: bool) -> bool {
        if raw.is_empty() {
            self.finder = None;
            return true;
        }
        let Some(m) = Matcher::new(raw) else {
            return false;
        };
        if self.finder.as_ref().is_some_and(|f| f.raw == m.raw) {
            return true;
        }
        self.jump_to_match(&m, previous, true);
        self.finder = Some(m);
        true
    }

    /// Moves to the next match after (or before) the cursor, wrapping around.
    /// `inclusive` also accepts the hovered entry, as live typing does.
    fn jump_to_match(&mut self, m: &Matcher, previous: bool, inclusive: bool) {
        let len = self.current.items.len();
        if len == 0 {
            return;
        }
        let cursor = self.current.scroll.cursor;
        let first = usize::from(!inclusive);
        let found = (first..len)
            .map(|i| {
                if previous {
                    (cursor + len - i) % len
                } else {
                    (cursor + i) % len
                }
            })
            .find(|&idx| m.is_match(&self.current.items[idx].name));
        if let Some(idx) = found {
            self.arrow(Step::Offset(idx as isize - cursor as isize));
        }
    }

    /// 1-based index of the hovered entry among find matches, and the count.
    pub fn found_position(&self) -> Option<(usize, usize)> {
        let finder = self.finder.as_ref()?;
        let matches: Vec<usize> = self
            .current
            .items
            .iter()
            .enumerate()
            .filter(|(_, e)| finder.is_match(&e.name))
            .map(|(i, _)| i)
            .collect();
        let k = matches
            .iter()
            .position(|&i| i == self.current.scroll.cursor)?;
        Some((k + 1, matches.len()))
    }

    // ---------- Notices ----------

    fn notify(&mut self, title: &str, message: impl Into<String>, level: Level) {
        self.notices.push(Notice {
            title: title.to_string(),
            message: message.into(),
            level,
            at: Instant::now(),
        });
        if self.notices.len() > 3 {
            self.notices.remove(0);
        }
    }

    pub fn tick(&mut self, now: Instant) {
        self.notices
            .retain(|n| now.duration_since(n.at) < NOTICE_TTL);
        if now.duration_since(self.last_refresh) >= Duration::from_secs(1) {
            self.last_refresh = now;
            self.refresh_changed();
        }
    }

    /// A warning notice, for the app to report problems found after choosing.
    pub fn warn(&mut self, title: &str, message: impl Into<String>) {
        self.notify(title, message, Level::Warn);
    }

    pub fn cursor_shape(&self) -> Option<CursorShape> {
        let editor = match &self.popup {
            Some(Popup::Input { editor, .. }) | Some(Popup::Help { editor, .. }) => editor,
            _ => return None,
        };
        Some(match editor.mode {
            Mode::Insert => CursorShape::Bar,
            Mode::Normal => CursorShape::Block,
        })
    }

    fn editor(&self, value: &str) -> Editor {
        let mut editor = Editor::new(value);
        editor.register = self.clipboard.clone();
        editor
    }

    /// Takes yanked or cut input text into the shared clipboard.
    fn sync_register(&mut self, editor: &Editor) -> Option<String> {
        (editor.register != self.clipboard).then(|| {
            self.clipboard = editor.register.clone();
            self.clipboard.clone()
        })
    }

    // ---------- Keys ----------

    pub fn on_paste(&mut self, text: &str) {
        self.pending.clear();
        match &mut self.popup {
            Some(Popup::Input { editor, .. }) => {
                editor.insert_str(text);
                self.after_edit();
            }
            Some(Popup::Help {
                editor,
                scroll,
                spot,
            }) => {
                editor.insert_str(text);
                let total = keymap::help_items(spot.is_some(), &editor.value()).len();
                let rows = self.help_limit;
                scroll.cursor = scroll.cursor.min(total.saturating_sub(1));
                scroll.arrow(Step::Offset(0), total, rows, rows / 2);
            }
            Some(Popup::Spot { .. }) => {}
            None => {
                // A dragged-in file is revealed and chosen.
                let raw = crate::tui::input::normalize_pasted_path(text);
                let path = fs::expand(&raw, &self.cwd);
                if path.is_file() {
                    self.reveal(&path);
                    self.choose(path);
                } else if path.is_dir() {
                    self.cd(&path, true);
                } else {
                    self.notify(
                        "Paste",
                        format!("No such file or folder: {}", path.display()),
                        Level::Warn,
                    );
                }
            }
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        match self.popup.take() {
            Some(Popup::Input {
                kind,
                editor,
                cmp,
                invalid,
            }) => self.on_input_key(kind, editor, cmp, invalid, key),
            Some(Popup::Help {
                editor,
                scroll,
                spot,
            }) => self.on_help_key(editor, scroll, spot, key),
            Some(Popup::Spot {
                row,
                size,
                pending_c,
            }) => self.on_spot_key(row, size, pending_c, key),
            None => match keymap::resolve(&mut self.pending, Key::from_event(key)) {
                Resolved::Run(binding) => self.run(binding.action, binding.desc),
                Resolved::Pending | Resolved::Unbound => Outcome::Continue,
            },
        }
    }

    fn run(&mut self, action: Action, desc: &str) -> Outcome {
        match action {
            Action::Escape => self.escape(),
            Action::Quit => return Outcome::Cancel,
            Action::Arrow(step) => self.arrow(step),
            Action::Leave => {
                if let Some(parent) = self.cwd.parent().map(Path::to_path_buf) {
                    let from = self.cwd.file_name().map(|n| n.to_os_string());
                    self.cd(&parent, true);
                    if let Some(name) = from {
                        self.current.hover(&name, self.limit);
                    }
                }
            }
            Action::Enter => {
                if let Some(e) = self.hovered().filter(|e| e.is_dir()).cloned() {
                    self.cd(&e.path, true);
                }
            }
            Action::Back => {
                if let Some(p) = self.back.back() {
                    self.cd(&p, false);
                }
            }
            Action::Forward => {
                if let Some(p) = self.back.forward() {
                    self.cd(&p, false);
                }
            }
            Action::Toggle => {
                if let Some(path) = self.hovered().map(|e| e.path.clone()) {
                    if !self.toggle(&path) {
                        self.notify(
                            "Toggle",
                            "This file cannot be selected, due to path nesting conflict.",
                            Level::Warn,
                        );
                    }
                }
                self.arrow(Step::Offset(1));
            }
            Action::SelectAll | Action::InvertSelection => {
                let paths: Vec<PathBuf> =
                    self.current.items.iter().map(|e| e.path.clone()).collect();
                let (on, off): (Vec<PathBuf>, Vec<PathBuf>) =
                    paths.into_iter().partition(|p| self.selected.contains(p));
                if action == Action::InvertSelection {
                    // Remove in one pass so the rest keeps its selection order.
                    self.selected.retain(|p| !on.contains(p));
                }
                let mut conflict = false;
                for path in off {
                    conflict |= !self.toggle(&path);
                }
                if conflict {
                    self.notify(
                        "Toggle all",
                        "Some files cannot be selected due to path nesting conflict.",
                        Level::Warn,
                    );
                }
            }
            Action::Seek(units) => self.seek(units),
            Action::Spot => {
                if self.hovered().is_some() {
                    self.popup = Some(Popup::Spot {
                        row: 1,
                        size: self.spawn_size(),
                        pending_c: false,
                    });
                }
            }
            Action::Open => self.open_selected(),
            Action::Hidden => {
                self.show_hidden = !self.show_hidden;
                self.refresh_prefs();
            }
            Action::Linemode(m) => self.linemode = m,
            Action::Copy(what) => {
                let text = self.copy_text(what);
                self.clipboard = text.clone();
                self.notify("Copy", "Copied to the clipboard.", Level::Info);
                return Outcome::Copy(text);
            }
            Action::Filter => self.open_input(InputKind::Filter),
            Action::Find { previous } => self.open_input(InputKind::Find { previous }),
            Action::FindArrow { previous } => {
                if let Some(m) = self.finder.clone() {
                    self.jump_to_match(&m, previous, false);
                }
            }
            Action::Sort(by, reverse) => {
                let sorter = Sorter {
                    by,
                    reverse,
                    ..self.sorter
                };
                if sorter != self.sorter {
                    self.sorter = sorter;
                    self.refresh_prefs();
                }
                // yazi pairs time and size sorts with the matching linemode.
                match by {
                    SortBy::Mtime => self.linemode = Linemode::Mtime,
                    SortBy::Btime => self.linemode = Linemode::Btime,
                    SortBy::Size => self.linemode = Linemode::Size,
                    _ => {}
                }
            }
            Action::Goto(goto) => {
                if let Some(home) = fs::home() {
                    let target = match goto {
                        Goto::Home => home,
                        Goto::Config => home.join(".config"),
                        Goto::Downloads => home.join("Downloads"),
                    };
                    self.cd(&target, true);
                }
            }
            Action::CdInteractive => self.open_input(InputKind::Cd),
            Action::Follow => {
                if let Some(e) = self.hovered().cloned() {
                    if let Some(link) = &e.link_to {
                        let base = e.path.parent().unwrap_or(Path::new("/"));
                        let target = fs::clean(&base.join(link));
                        self.reveal(&target);
                    }
                }
            }
            Action::Help => {
                self.popup = Some(Popup::Help {
                    editor: self.editor(""),
                    scroll: Scroll::default(),
                    spot: None,
                });
            }
            Action::Unavailable => self.notify(
                desc,
                "Not available while choosing a file to submit.",
                Level::Warn,
            ),
        }
        Outcome::Continue
    }

    fn open_input(&mut self, kind: InputKind) {
        self.popup = Some(Popup::Input {
            cmp: (kind == InputKind::Cd).then(Cmp::default),
            kind,
            editor: self.editor(""),
            invalid: false,
        });
    }

    /// Enter/o: the selection, or else the hovered entry, must be one file.
    fn open_selected(&mut self) {
        if self.selected.len() > 1 {
            self.notify(
                "Submit",
                format!(
                    "Learn accepts one file per submission, and {} are selected. Esc clears the selection.",
                    self.selected.len()
                ),
                Level::Warn,
            );
            return;
        }
        let target = match self.selected.first() {
            Some(p) => Entry::from_path(p),
            None => self.hovered().cloned(),
        };
        let Some(entry) = target else {
            return;
        };
        if entry.is_dir() {
            if self.selected.is_empty() {
                self.cd(&entry.path, true);
            } else {
                self.notify("Submit", "Choose a file, not a folder.", Level::Warn);
            }
        } else if !entry.is_file() {
            self.notify(
                "Submit",
                "Only regular files can be submitted.",
                Level::Warn,
            );
        } else {
            self.choose(entry.path);
        }
    }

    fn choose(&mut self, file: PathBuf) {
        self.open_input(InputKind::Comment { file });
    }

    fn copy_text(&self, what: CopyWhat) -> String {
        let targets: Vec<PathBuf> = if self.selected.is_empty() {
            self.hovered().map(|e| e.path.clone()).into_iter().collect()
        } else {
            self.selected.clone()
        };
        let parts: Vec<String> = targets
            .iter()
            .map(|p| match what {
                CopyWhat::Path => p.display().to_string(),
                CopyWhat::DirPath => p
                    .parent()
                    .map(|d| d.display().to_string())
                    .unwrap_or_default(),
                CopyWhat::Filename => p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                CopyWhat::NameWithoutExt => p
                    .file_stem()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            })
            .collect();
        if parts.is_empty() && what == CopyWhat::DirPath {
            return self.cwd.display().to_string();
        }
        parts.join("\n")
    }

    // ---------- Input popups ----------

    /// Re-applies live filter/find and refreshes completion after an edit.
    fn after_edit(&mut self) {
        let Some(Popup::Input { kind, editor, .. }) = self.popup.as_ref() else {
            return;
        };
        let (kind, value, word) = (kind.clone(), editor.value(), editor.completion_word());
        let ok = match kind {
            InputKind::Filter => self.apply_filter(&value),
            InputKind::Find { previous } => self.apply_find(&value, previous),
            InputKind::Cd => {
                let cwd = self.cwd.clone();
                if let Some(Popup::Input { cmp: Some(cmp), .. }) = self.popup.as_mut() {
                    refresh_cmp(cmp, &word, &cwd);
                }
                true
            }
            InputKind::Comment { .. } => true,
        };
        if let Some(Popup::Input { invalid, .. }) = self.popup.as_mut() {
            *invalid = !ok;
        }
    }

    fn on_input_key(
        &mut self,
        kind: InputKind,
        mut editor: Editor,
        mut cmp: Option<Cmp>,
        invalid: bool,
        key: KeyEvent,
    ) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // The completion popup sees keys first while it is visible.
        if let Some(c) = cmp.as_mut().filter(|c| !c.items.is_empty()) {
            let total = c.items.len();
            let rows = total.min(CMP_ROWS);
            let step = match key.code {
                KeyCode::Up => Some(Step::Prev),
                KeyCode::Down => Some(Step::Next),
                KeyCode::Char('p') if ctrl => Some(Step::Prev),
                KeyCode::Char('n') if ctrl => Some(Step::Next),
                KeyCode::Char('k') if alt => Some(Step::Prev),
                KeyCode::Char('j') if alt => Some(Step::Next),
                _ => None,
            };
            let handled = if let Some(step) = step {
                c.scroll.arrow(step, total, rows, rows / 2);
                true
            } else {
                match key.code {
                    KeyCode::Tab => {
                        accept_cmp(c, &mut editor);
                        let word = editor.completion_word();
                        refresh_cmp(c, &word, &self.cwd);
                        true
                    }
                    KeyCode::Char('c') if ctrl => {
                        c.items.clear();
                        true
                    }
                    // Enter completes, then submits below.
                    KeyCode::Enter => {
                        accept_cmp(c, &mut editor);
                        false
                    }
                    KeyCode::Esc => {
                        c.items.clear();
                        false
                    }
                    _ => false,
                }
            };
            if handled {
                self.popup = Some(Popup::Input {
                    kind,
                    editor,
                    cmp,
                    invalid,
                });
                return Outcome::Continue;
            }
        }

        let edit = editor.handle_key(key, &self.history);
        let copied = self.sync_register(&editor);
        let outcome = self.after_input_key(kind, editor, cmp, invalid, edit);
        match (outcome, copied) {
            (Outcome::Continue, Some(text)) => Outcome::Copy(text),
            (outcome, _) => outcome,
        }
    }

    fn after_input_key(
        &mut self,
        kind: InputKind,
        editor: Editor,
        mut cmp: Option<Cmp>,
        invalid: bool,
        edit: Edit,
    ) -> Outcome {
        match edit {
            Edit::Submit(value) => {
                editor::remember(&mut self.history, &value);
                self.submit_input(kind, value)
            }
            Edit::Cancel => Outcome::Continue,
            Edit::Changed => {
                self.popup = Some(Popup::Input {
                    kind,
                    editor,
                    cmp,
                    invalid,
                });
                self.after_edit();
                Outcome::Continue
            }
            Edit::Moved | Edit::None => {
                if let Some(c) = cmp.as_mut().filter(|c| !c.items.is_empty()) {
                    if editor.mode == Mode::Normal {
                        c.items.clear();
                    } else {
                        // Moving the cursor re-triggers completion while it is shown.
                        refresh_cmp(c, &editor.completion_word(), &self.cwd);
                    }
                }
                self.popup = Some(Popup::Input {
                    kind,
                    editor,
                    cmp,
                    invalid,
                });
                Outcome::Continue
            }
        }
    }

    fn submit_input(&mut self, kind: InputKind, value: String) -> Outcome {
        match kind {
            InputKind::Filter => {
                self.apply_filter(&value);
            }
            InputKind::Find { previous } => {
                self.apply_find(&value, previous);
            }
            InputKind::Cd => {
                let path = fs::expand(&value, &self.cwd);
                if path.is_dir() {
                    self.cd(&path, true);
                } else if path.exists() {
                    self.reveal(&path);
                }
            }
            InputKind::Comment { file } => {
                return Outcome::Submit {
                    file,
                    comment: value,
                };
            }
        }
        Outcome::Continue
    }

    // ---------- Help ----------

    fn on_help_key(
        &mut self,
        mut editor: Editor,
        mut scroll: Scroll,
        spot: Option<SpotState>,
        key: KeyEvent,
    ) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let for_spot = spot.is_some();
        let entries = keymap::help_items(for_spot, &editor.value());
        let total = entries.len();
        let rows = self.help_limit.max(1);
        let back = |p: &mut Self, spot: Option<SpotState>| {
            if let Some(s) = spot {
                p.popup = Some(Popup::Spot {
                    row: s.row,
                    size: s.size,
                    pending_c: false,
                });
            }
        };
        let step = match key.code {
            KeyCode::Up => Some(Step::Prev),
            KeyCode::Down => Some(Step::Next),
            KeyCode::Char('p') if ctrl => Some(Step::Prev),
            KeyCode::Char('n') if ctrl => Some(Step::Next),
            KeyCode::Char('k') if editor.mode == Mode::Normal => Some(Step::Prev),
            KeyCode::Char('j') if editor.mode == Mode::Normal => Some(Step::Next),
            _ => None,
        };
        if let Some(step) = step {
            scroll.arrow(step, total, rows, rows / 2);
            self.popup = Some(Popup::Help {
                editor,
                scroll,
                spot,
            });
            return Outcome::Continue;
        }
        if key.code == KeyCode::Enter {
            let action = entries.get(scroll.cursor).and_then(|h| h.action);
            back(self, spot);
            return match (action, entries.get(scroll.cursor)) {
                (Some(action), Some(h)) => self.run(action, h.desc),
                _ => Outcome::Continue,
            };
        }
        let edit = editor.handle_key(key, &[]);
        let copied = self.sync_register(&editor);
        match edit {
            Edit::Cancel | Edit::Submit(_) => back(self, spot),
            Edit::Changed => {
                let total = keymap::help_items(for_spot, &editor.value()).len();
                scroll.cursor = scroll.cursor.min(total.saturating_sub(1));
                scroll.arrow(Step::Offset(0), total, rows, rows / 2);
                self.popup = Some(Popup::Help {
                    editor,
                    scroll,
                    spot,
                });
            }
            Edit::Moved | Edit::None => {
                self.popup = Some(Popup::Help {
                    editor,
                    scroll,
                    spot,
                })
            }
        }
        copied.map_or(Outcome::Continue, Outcome::Copy)
    }

    // ---------- Spot ----------

    /// Starts a recursive size count for the selection, or for a hovered
    /// directory. Symlinks count their own length and are not followed.
    fn spawn_size(&self) -> Option<Arc<SizeJob>> {
        let roots: Vec<PathBuf> = if self.selected.is_empty() {
            let e = self.hovered().filter(|e| e.is_dir() && !e.is_link)?;
            vec![e.path.clone()]
        } else {
            self.selected.clone()
        };
        let job = Arc::new(SizeJob::default());
        let worker = job.clone();
        std::thread::spawn(move || {
            let mut queue = Vec::new();
            for root in roots {
                match std::fs::symlink_metadata(&root) {
                    Ok(m) if m.is_dir() => queue.push(root),
                    Ok(m) => {
                        worker.bytes.fetch_add(m.len(), Ordering::Relaxed);
                    }
                    Err(_) => {}
                }
            }
            while let Some(dir) = queue.pop() {
                if worker.cancel.load(Ordering::Relaxed) {
                    return;
                }
                let Ok(read) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in read.flatten() {
                    let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                        continue;
                    };
                    if meta.is_dir() {
                        queue.push(entry.path());
                    } else {
                        worker.bytes.fetch_add(meta.len(), Ordering::Relaxed);
                    }
                }
            }
            worker.done.store(true, Ordering::Relaxed);
        });
        Some(job)
    }

    /// Rows of the spot table: (label, value); header rows have no value.
    pub fn spot_rows(&self, size: Option<&SizeJob>) -> Vec<(String, Option<String>)> {
        let Some(e) = self.hovered() else {
            return Vec::new();
        };
        let size_text = |job: &SizeJob| {
            let bytes = fs::readable_size(job.bytes.load(Ordering::Relaxed));
            if job.done.load(Ordering::Relaxed) {
                bytes
            } else {
                format!("{bytes} (?)")
            }
        };
        let mut rows = Vec::new();
        // Any selection shows yazi's multi spotter.
        if !self.selected.is_empty() {
            rows.push(("Multi".to_string(), None));
            rows.push((
                "  Size:".into(),
                Some(size.map(size_text).unwrap_or_default()),
            ));
            rows.push((
                "  Count:".into(),
                Some(format!("{} selected", self.selected.len())),
            ));
            return rows;
        }
        if let Some(job) = size {
            rows.push(("Folder".into(), None));
            rows.push(("  Size:".into(), Some(size_text(job))));
            rows.push((String::new(), None));
        }
        rows.push(("Base".into(), None));
        rows.push(("  Created:".into(), Some(fs::full_time(e.btime))));
        rows.push(("  Modified:".into(), Some(fs::full_time(e.mtime))));
        if !e.is_dir() {
            rows.push(("  Size:".into(), Some(fs::readable_size(e.len))));
        }
        if let Some(perm) = fs::permissions(e) {
            rows.push(("  Permissions:".into(), Some(perm)));
        }
        rows.push(("  Owner:".into(), Some(fs::owner(e))));
        rows.push(("  Path:".into(), Some(e.path.display().to_string())));
        if let Some(link) = &e.link_to {
            rows.push(("  Link to:".into(), Some(link.display().to_string())));
        }
        rows
    }

    fn on_spot_key(
        &mut self,
        mut row: usize,
        size: Option<Arc<SizeJob>>,
        pending_c: bool,
        key: KeyEvent,
    ) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let close = |size: &Option<Arc<SizeJob>>| {
            if let Some(job) = size {
                job.cancel.store(true, Ordering::Relaxed);
            }
        };
        if pending_c {
            if key.code == KeyCode::Char('c') && !ctrl && !key.modifiers.contains(KeyModifiers::ALT)
            {
                let rows = self.spot_rows(size.as_deref());
                let text = rows
                    .get(row)
                    .map(|(label, value)| value.clone().unwrap_or_else(|| label.trim().to_string()))
                    .unwrap_or_default();
                self.popup = Some(Popup::Spot {
                    row,
                    size,
                    pending_c: false,
                });
                return if text.is_empty() {
                    Outcome::Continue
                } else {
                    Outcome::Copy(text)
                };
            }
            self.popup = Some(Popup::Spot {
                row,
                size,
                pending_c: false,
            });
            return Outcome::Continue;
        }
        let total = self.spot_rows(size.as_deref()).len().max(1);
        match key.code {
            KeyCode::Esc | KeyCode::Tab => {
                close(&size);
                return Outcome::Continue;
            }
            KeyCode::Char('c') if ctrl => {
                close(&size);
                return Outcome::Continue;
            }
            KeyCode::Up | KeyCode::Char('k') => row = Step::Prev.add(row, total, total),
            KeyCode::Down | KeyCode::Char('j') => row = Step::Next.add(row, total, total),
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Right | KeyCode::Char('l') => {
                close(&size);
                let step = if matches!(key.code, KeyCode::Left | KeyCode::Char('h')) {
                    Step::Prev
                } else {
                    Step::Next
                };
                self.arrow(step);
                let size = self.spawn_size();
                self.popup = Some(Popup::Spot {
                    row: 1,
                    size,
                    pending_c: false,
                });
                return Outcome::Continue;
            }
            KeyCode::Char('c') => {
                self.popup = Some(Popup::Spot {
                    row,
                    size,
                    pending_c: true,
                });
                return Outcome::Continue;
            }
            KeyCode::Char('~') | KeyCode::F(1) => {
                self.popup = Some(Popup::Help {
                    editor: self.editor(""),
                    scroll: Scroll::default(),
                    spot: Some(SpotState { row, size }),
                });
                return Outcome::Continue;
            }
            _ => {}
        }
        self.popup = Some(Popup::Spot {
            row,
            size,
            pending_c: false,
        });
        Outcome::Continue
    }
}

/// Rows of bindings visible in the help popup.
pub const HELP_ROWS: usize = 21;

/// Completion for the change-directory input, like yazi's: every entry of the
/// typed directory (dot files included), smart-case prefix matches first, then
/// case-sensitive substring matches, at most 30.
fn refresh_cmp(cmp: &mut Cmp, word: &str, cwd: &Path) {
    if word == "~" {
        cmp.items.clear();
        return;
    }
    let (mut dir_part, mut partial) = match word.rfind('/') {
        Some(i) => (word[..=i].to_string(), &word[i + 1..]),
        None => (String::new(), word),
    };
    // A final `.` or `..` is the directory to list, as in yazi's completer.
    if partial == "." || partial == ".." {
        dir_part = format!("{word}/");
        partial = "";
    }
    let dir = if dir_part.is_empty() {
        cwd.to_path_buf()
    } else {
        fs::expand(&dir_part, cwd)
    };
    let names = cmp.cache.entry(dir.clone()).or_insert_with(|| {
        let mut names: Vec<(String, bool)> = std::fs::read_dir(&dir)
            .map(|read| {
                read.flatten()
                    .map(|e| {
                        let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
                        (e.file_name().to_string_lossy().into_owned(), is_dir)
                    })
                    .collect()
            })
            .unwrap_or_default();
        names.sort_by(|a, b| natsort::natsort(a.0.as_bytes(), b.0.as_bytes(), true));
        names
    });
    let sensitive = partial.chars().any(char::is_uppercase);
    let lower = partial.to_lowercase();
    let prefix = names.iter().filter(|(n, _)| {
        if sensitive {
            n.starts_with(partial)
        } else {
            n.to_lowercase().starts_with(&lower)
        }
    });
    let substring = names.iter().filter(|(n, _)| {
        let is_prefix = if sensitive {
            n.starts_with(partial)
        } else {
            n.to_lowercase().starts_with(&lower)
        };
        !is_prefix && n.contains(partial)
    });
    cmp.items = prefix.chain(substring).take(CMP_MAX).cloned().collect();
    cmp.prefix = dir_part;
    cmp.scroll = Scroll::default();
}

/// Replaces the typed path component with the selected candidate.
fn accept_cmp(cmp: &mut Cmp, editor: &mut Editor) {
    let Some((name, is_dir)) = cmp.items.get(cmp.scroll.cursor).cloned() else {
        return;
    };
    let value = editor.value();
    let cursor_byte: usize = value
        .chars()
        .take(editor.cursor())
        .map(char::len_utf8)
        .sum();
    let rest_start = value[cursor_byte..]
        .find('/')
        .map_or(value.len(), |i| cursor_byte + i);
    let rest = &value[rest_start..];
    let inserted = format!("{}{}{}", cmp.prefix, name, if is_dir { "/" } else { "" });
    let cursor = inserted.chars().count();
    let new_value = format!(
        "{inserted}{}",
        if is_dir {
            rest.trim_start_matches('/')
        } else {
            rest
        }
    );
    editor.replace(&new_value, cursor);
}

#[cfg(test)]
mod tests;
