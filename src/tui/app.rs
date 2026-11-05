//! TUI state and key handling.
//!
//! [`App`] is plain state: key presses and background results update it and
//! return [`Effect`]s, which the runtime in [`super`] executes. Keeping network
//! I/O out of this module makes the interaction logic testable without Learn.
//! The only I/O here is local directory listing for the submission file picker.
//!
//! Logging in is not the TUI's job: without a session it asks the user to run
//! `thu-learn login` and retry.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::browser::{FilePicker, Focus};
use super::input::{complete_path, expand_path, normalize_pasted_path, Input};
use crate::models::{
    Course, CourseFile, Homework, HomeworkAttachment, HomeworkStatus, Notification,
};

/// Wait this long after the selection settles before fetching homework details.
const DETAIL_DEBOUNCE: Duration = Duration::from_millis(150);
const INFO_TTL: Duration = Duration::from_secs(8);
const ERROR_TTL: Duration = Duration::from_secs(20);
/// Rows moved by PageUp/PageDown in lists.
const PAGE: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Homework,
    Announcements,
    Files,
    Courses,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Homework, Tab::Announcements, Tab::Files, Tab::Courses];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn title(self) -> &'static str {
        match self {
            Tab::Homework => "Homework",
            Tab::Announcements => "Announcements",
            Tab::Files => "Files",
            Tab::Courses => "Courses",
        }
    }

    fn offset(self, delta: isize) -> Tab {
        let n = Tab::ALL.len() as isize;
        Tab::ALL[((self.index() as isize + delta).rem_euclid(n)) as usize]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeworkFilter {
    /// Not submitted and not past the deadline.
    Pending,
    /// Not submitted and past the deadline.
    Overdue,
    /// Submitted or graded.
    Done,
    All,
}

impl HomeworkFilter {
    pub fn label(self) -> &'static str {
        match self {
            HomeworkFilter::Pending => "Pending",
            HomeworkFilter::Overdue => "Overdue",
            HomeworkFilter::Done => "Done",
            HomeworkFilter::All => "All",
        }
    }

    fn next(self) -> Self {
        match self {
            HomeworkFilter::Pending => HomeworkFilter::Overdue,
            HomeworkFilter::Overdue => HomeworkFilter::Done,
            HomeworkFilter::Done => HomeworkFilter::All,
            HomeworkFilter::All => HomeworkFilter::Pending,
        }
    }

    pub fn matches(self, h: &Homework, now: DateTime<Local>) -> bool {
        let overdue = is_overdue(h, now);
        match self {
            HomeworkFilter::Pending => h.status == HomeworkStatus::Pending && !overdue,
            HomeworkFilter::Overdue => h.status == HomeworkStatus::Pending && overdue,
            HomeworkFilter::Done => h.status != HomeworkStatus::Pending,
            HomeworkFilter::All => true,
        }
    }
}

/// Only pending homework with an explicit past deadline is overdue.
pub fn is_overdue(h: &Homework, now: DateTime<Local>) -> bool {
    h.status == HomeworkStatus::Pending && h.deadline.is_some_and(|d| d < now)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnouncementFilter {
    All,
    Unread,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Session {
    /// Verifying saved cookies.
    Checking,
    /// No usable session; carries the reason shown to the user.
    LoggedOut(String),
    Ready,
}

/// Remote data that keeps showing the previous value while it refreshes.
#[derive(Debug, Clone)]
pub struct Remote<T> {
    pub data: T,
    pub loading: bool,
    pub loaded: bool,
    pub error: Option<String>,
}

impl<T: Default> Default for Remote<T> {
    fn default() -> Self {
        Self {
            data: T::default(),
            loading: false,
            loaded: false,
            error: None,
        }
    }
}

impl<T> Remote<T> {
    fn start(&mut self) {
        self.loading = true;
    }

    fn finish(&mut self, result: Result<T, String>) {
        self.loading = false;
        match result {
            Ok(data) => {
                self.data = data;
                self.loaded = true;
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct HomeworkDetail {
    pub description: String,
    pub attachments: Vec<HomeworkAttachment>,
}

#[derive(Debug, Clone)]
pub enum DetailState {
    Loading,
    Loaded(HomeworkDetail),
    Failed(String),
}

#[derive(Debug, Clone, Default)]
pub struct CourseData {
    pub semester: String,
    /// Current-semester courses, shown in the Courses tab and course picker.
    pub current: Vec<Course>,
    /// Previous-semester courses, used to name older homework.
    pub previous: Vec<Course>,
}

/// Work for the runtime to perform outside the pure state.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    CheckSession,
    /// Reload courses, homework, announcements, and files.
    LoadAll,
    LoadHomework {
        courses: Vec<Course>,
    },
    LoadHomeworkDetail {
        course_id: String,
        student_homework_id: String,
        base_id: String,
    },
    DownloadFile {
        file: CourseFile,
        dir: PathBuf,
    },
    DownloadAttachments {
        attachments: Vec<HomeworkAttachment>,
        dir: PathBuf,
    },
    Submit {
        student_homework_id: String,
        file: PathBuf,
        comment: String,
    },
    OpenPath(PathBuf),
}

/// Results reported back by background work.
#[derive(Debug)]
pub enum Msg {
    SessionReady,
    SessionFailed(String),
    Courses(Result<CourseData, String>),
    Homework(Result<Vec<Homework>, String>),
    Announcements(Result<Vec<Notification>, String>),
    Files {
        files: Vec<CourseFile>,
        failures: Vec<String>,
    },
    HomeworkDetail {
        id: String,
        result: Result<HomeworkDetail, String>,
    },
    Downloaded(Result<Vec<PathBuf>, String>),
    Submitted {
        id: String,
        result: Result<(), String>,
    },
    OpenFailed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone)]
pub struct Status {
    pub kind: StatusKind,
    pub text: String,
    pub at: Instant,
}

/// The submit screen: which homework, plus the file picker and comment.
#[derive(Debug, Clone)]
pub struct SubmitState {
    pub student_homework_id: String,
    pub title: String,
    pub course: String,
    pub already_submitted: bool,
    pub picker: FilePicker,
}

#[derive(Debug, Clone)]
pub enum Overlay {
    Help,
    /// Index 0 is "All courses"; index `n` is `courses.current[n - 1]`.
    CoursePicker {
        selected: usize,
    },
    Submit(Box<SubmitState>),
    Confirm {
        title: String,
        lines: Vec<String>,
        effect: Box<Effect>,
        /// Screen to return to when the confirmation is declined.
        back: Option<Box<SubmitState>>,
    },
    DownloadDir {
        input: Input,
        error: Option<String>,
    },
}

pub struct App {
    pub tab: Tab,
    pub session: Session,
    pub courses: CourseData,
    pub courses_loading: bool,
    pub homework: Remote<Vec<Homework>>,
    pub announcements: Remote<Vec<Notification>>,
    pub files: Remote<Vec<CourseFile>>,
    pub details: HashMap<String, DetailState>,
    pub course_filter: Option<String>,
    pub homework_filter: HomeworkFilter,
    pub announcement_filter: AnnouncementFilter,
    /// Per-tab search queries, indexed by [`Tab::index`].
    pub search: [Input; 4],
    pub searching: bool,
    /// Selected item key per tab; resolved against the visible list on each use.
    selection: [Option<String>; 4],
    /// First visible row per tab, persisted between frames for stable scrolling.
    pub offsets: [usize; 4],
    selection_changed_at: Instant,
    pub zoomed: bool,
    pub detail_scroll: u16,
    /// Largest useful scroll value, written by the renderer.
    pub detail_max_scroll: u16,
    detail_key: Option<(Tab, String)>,
    pub overlay: Option<Overlay>,
    pub status: Option<Status>,
    pub in_flight: usize,
    pub download_dir: PathBuf,
    /// Folder the submission picker opens in; remembers the last one used.
    pub submit_dir: PathBuf,
    pub last_download: Option<PathBuf>,
    pub now: DateTime<Local>,
    pub spinner: usize,
    pub should_quit: bool,
}

impl App {
    pub fn new(download_dir: PathBuf, now: DateTime<Local>) -> Self {
        Self {
            tab: Tab::Homework,
            session: Session::Checking,
            courses: CourseData::default(),
            courses_loading: false,
            homework: Remote::default(),
            announcements: Remote::default(),
            files: Remote::default(),
            details: HashMap::new(),
            course_filter: None,
            homework_filter: HomeworkFilter::Pending,
            announcement_filter: AnnouncementFilter::All,
            search: Default::default(),
            searching: false,
            selection: Default::default(),
            offsets: [0; 4],
            selection_changed_at: Instant::now(),
            zoomed: false,
            detail_scroll: 0,
            detail_max_scroll: u16::MAX,
            detail_key: None,
            overlay: None,
            status: None,
            in_flight: 0,
            submit_dir: download_dir.clone(),
            download_dir,
            last_download: None,
            now,
            spinner: 0,
            should_quit: false,
        }
    }

    pub fn init(&self) -> Vec<Effect> {
        vec![Effect::CheckSession]
    }

    // ---------- Derived views ----------

    pub fn is_busy(&self) -> bool {
        self.session == Session::Checking
            || self.courses_loading
            || self.homework.loading
            || self.announcements.loading
            || self.files.loading
            || self.in_flight > 0
            || matches!(
                self.selected_homework()
                    .and_then(|h| self.details.get(&h.student_homework_id)),
                Some(DetailState::Loading)
            )
    }

    fn course_matches(&self, course_id: &str) -> bool {
        self.course_filter
            .as_deref()
            .is_none_or(|id| id == course_id)
    }

    fn query(&self, tab: Tab) -> String {
        self.search[tab.index()].value().trim().to_lowercase()
    }

    pub fn course_name(&self, id: &str) -> Option<&str> {
        self.courses
            .current
            .iter()
            .chain(&self.courses.previous)
            .find(|c| c.id == id)
            .map(|c| c.name.as_str())
    }

    pub fn visible_homework(&self) -> Vec<&Homework> {
        let q = self.query(Tab::Homework);
        let mut list: Vec<&Homework> = self
            .homework
            .data
            .iter()
            .filter(|h| self.course_matches(&h.course_id))
            .filter(|h| self.homework_filter.matches(h, self.now))
            .filter(|h| matches_query(&q, &[&h.title, &h.course_name]))
            .collect();
        let ascending = self.homework_filter == HomeworkFilter::Pending;
        list.sort_by(|a, b| match (a.deadline, b.deadline) {
            (Some(x), Some(y)) if ascending => x.cmp(&y),
            (Some(x), Some(y)) => y.cmp(&x),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
        list
    }

    pub fn visible_announcements(&self) -> Vec<&Notification> {
        let q = self.query(Tab::Announcements);
        let mut list: Vec<&Notification> = self
            .announcements
            .data
            .iter()
            .filter(|n| self.course_matches(&n.course_id))
            .filter(|n| self.announcement_filter == AnnouncementFilter::All || !n.read)
            .filter(|n| matches_query(&q, &[&n.title, &n.course_name, &n.publisher]))
            .collect();
        list.sort_by(|a, b| b.publish_time.cmp(&a.publish_time));
        list
    }

    pub fn visible_files(&self) -> Vec<&CourseFile> {
        let q = self.query(Tab::Files);
        let mut list: Vec<&CourseFile> = self
            .files
            .data
            .iter()
            .filter(|f| self.course_matches(&f.course_id))
            .filter(|f| matches_query(&q, &[&f.title, &f.course_name, &f.file_type]))
            .collect();
        list.sort_by(|a, b| b.upload_time.cmp(&a.upload_time));
        list
    }

    pub fn visible_courses(&self) -> Vec<&Course> {
        let q = self.query(Tab::Courses);
        self.courses
            .current
            .iter()
            .filter(|c| matches_query(&q, &[&c.name, &c.teacher]))
            .collect()
    }

    fn visible_keys(&self, tab: Tab) -> Vec<String> {
        match tab {
            Tab::Homework => self
                .visible_homework()
                .iter()
                .map(|h| h.student_homework_id.clone())
                .collect(),
            Tab::Announcements => self
                .visible_announcements()
                .iter()
                .map(|n| n.id.clone())
                .collect(),
            Tab::Files => self.visible_files().iter().map(|f| f.id.clone()).collect(),
            Tab::Courses => self
                .visible_courses()
                .iter()
                .map(|c| c.id.clone())
                .collect(),
        }
    }

    /// Index of the selected row in the visible list, falling back to the first row.
    pub fn selected_index(&self, tab: Tab) -> Option<usize> {
        let keys = self.visible_keys(tab);
        if keys.is_empty() {
            return None;
        }
        let key = self.selection[tab.index()].as_ref();
        Some(
            key.and_then(|k| keys.iter().position(|x| x == k))
                .unwrap_or(0),
        )
    }

    fn selected_key(&self, tab: Tab) -> Option<String> {
        let keys = self.visible_keys(tab);
        let idx = self.selected_index(tab)?;
        keys.into_iter().nth(idx)
    }

    pub fn selected_homework(&self) -> Option<&Homework> {
        let key = self.selected_key(Tab::Homework)?;
        self.homework
            .data
            .iter()
            .find(|h| h.student_homework_id == key)
    }

    pub fn selected_announcement(&self) -> Option<&Notification> {
        let key = self.selected_key(Tab::Announcements)?;
        self.announcements.data.iter().find(|n| n.id == key)
    }

    pub fn selected_file(&self) -> Option<&CourseFile> {
        let key = self.selected_key(Tab::Files)?;
        self.files.data.iter().find(|f| f.id == key)
    }

    pub fn selected_course(&self) -> Option<&Course> {
        let key = self.selected_key(Tab::Courses)?;
        self.courses.current.iter().find(|c| c.id == key)
    }

    pub fn pending_count(&self, course_id: Option<&str>) -> usize {
        self.homework
            .data
            .iter()
            .filter(|h| course_id.is_none_or(|id| id == h.course_id))
            .filter(|h| HomeworkFilter::Pending.matches(h, self.now))
            .count()
    }

    pub fn unread_count(&self, course_id: Option<&str>) -> usize {
        self.announcements
            .data
            .iter()
            .filter(|n| course_id.is_none_or(|id| id == n.course_id))
            .filter(|n| !n.read)
            .count()
    }

    pub fn file_count(&self, course_id: Option<&str>) -> usize {
        self.files
            .data
            .iter()
            .filter(|f| course_id.is_none_or(|id| id == f.course_id))
            .count()
    }

    /// Resets detail scrolling when the selected item changes.
    pub fn sync_detail_key(&mut self) {
        let key = self.selected_key(self.tab).map(|k| (self.tab, k));
        if key != self.detail_key {
            self.detail_key = key;
            self.detail_scroll = 0;
            self.detail_max_scroll = u16::MAX;
        }
    }

    // ---------- Status ----------

    fn info(&mut self, text: impl Into<String>) {
        self.set_status(StatusKind::Info, text);
    }

    fn success(&mut self, text: impl Into<String>) {
        self.set_status(StatusKind::Success, text);
    }

    fn error(&mut self, text: impl Into<String>) {
        self.set_status(StatusKind::Error, text);
    }

    fn set_status(&mut self, kind: StatusKind, text: impl Into<String>) {
        self.status = Some(Status {
            kind,
            text: text.into(),
            at: Instant::now(),
        });
    }

    // ---------- Background results ----------

    pub fn on_msg(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::SessionReady => {
                self.session = Session::Ready;
                return self.reload_all();
            }
            Msg::SessionFailed(reason) => self.session = Session::LoggedOut(reason),
            Msg::Courses(result) => {
                self.courses_loading = false;
                match result {
                    Ok(data) => {
                        let keep_filter = self
                            .course_filter
                            .as_ref()
                            .is_some_and(|id| data.current.iter().any(|c| &c.id == id));
                        if !keep_filter {
                            self.course_filter = None;
                        }
                        self.courses = data;
                    }
                    Err(e) => {
                        self.homework.loading = false;
                        self.announcements.loading = false;
                        self.files.loading = false;
                        self.error(format!(
                            "Could not load courses: {e} (if the session expired, run `thu-learn login`, then press r)"
                        ));
                    }
                }
            }
            Msg::Homework(result) => {
                if let Err(e) = &result {
                    self.error(format!("Could not load homework: {e}"));
                }
                self.homework.finish(result);
            }
            Msg::Announcements(result) => {
                if let Err(e) = &result {
                    self.error(format!("Could not load announcements: {e}"));
                }
                self.announcements.finish(result);
            }
            Msg::Files { files, failures } => {
                if !failures.is_empty() {
                    self.error(format!(
                        "Could not load files for {} course(s): {}",
                        failures.len(),
                        failures.join("; ")
                    ));
                }
                self.files.finish(Ok(files));
            }
            Msg::HomeworkDetail { id, result } => {
                let state = match result {
                    Ok(d) => DetailState::Loaded(d),
                    Err(e) => DetailState::Failed(e),
                };
                self.details.insert(id, state);
            }
            Msg::Downloaded(result) => {
                self.in_flight = self.in_flight.saturating_sub(1);
                match result {
                    Ok(paths) if paths.len() == 1 => {
                        self.success(format!("Saved {} · o to open", paths[0].display()));
                        self.last_download = paths.into_iter().next();
                    }
                    Ok(paths) => {
                        self.success(format!(
                            "Saved {} files to {} · o to open the folder",
                            paths.len(),
                            self.download_dir.display()
                        ));
                        self.last_download = Some(self.download_dir.clone());
                    }
                    Err(e) => self.error(format!("Download failed: {e}")),
                }
            }
            Msg::Submitted { id, result } => {
                self.in_flight = self.in_flight.saturating_sub(1);
                match result {
                    Ok(()) => {
                        self.success("Submission succeeded.");
                        self.details.remove(&id);
                        if self.session == Session::Ready {
                            self.homework.start();
                            return vec![Effect::LoadHomework {
                                courses: self.all_courses(),
                            }];
                        }
                    }
                    Err(e) => self.error(format!("Submission failed: {e}")),
                }
            }
            Msg::OpenFailed(e) => self.error(format!("Could not open: {e}")),
        }
        Vec::new()
    }

    fn all_courses(&self) -> Vec<Course> {
        self.courses
            .current
            .iter()
            .chain(&self.courses.previous)
            .cloned()
            .collect()
    }

    fn reload_all(&mut self) -> Vec<Effect> {
        self.courses_loading = true;
        self.homework.start();
        self.announcements.start();
        self.files.start();
        vec![Effect::LoadAll]
    }

    // ---------- Timer ----------

    pub fn on_tick(&mut self, now: Instant, wall: DateTime<Local>) -> Vec<Effect> {
        self.spinner = self.spinner.wrapping_add(1);
        self.now = wall;
        if let Some(status) = &self.status {
            let ttl = if status.kind == StatusKind::Error {
                ERROR_TTL
            } else {
                INFO_TTL
            };
            if now.duration_since(status.at) > ttl {
                self.status = None;
            }
        }
        self.sync_detail_key();

        if self.session != Session::Ready
            || self.tab != Tab::Homework
            || now.duration_since(self.selection_changed_at) < DETAIL_DEBOUNCE
        {
            return Vec::new();
        }
        let Some(hw) = self.selected_homework() else {
            return Vec::new();
        };
        if self.details.contains_key(&hw.student_homework_id) {
            return Vec::new();
        }
        let effect = Effect::LoadHomeworkDetail {
            course_id: hw.course_id.clone(),
            student_homework_id: hw.student_homework_id.clone(),
            base_id: hw.base_id.clone(),
        };
        self.details
            .insert(hw.student_homework_id.clone(), DetailState::Loading);
        vec![effect]
    }

    // ---------- Input ----------

    pub fn on_paste(&mut self, text: &str) {
        match &mut self.overlay {
            Some(Overlay::Submit(submit)) => {
                let picker = &mut submit.picker;
                match picker.focus {
                    Focus::Comment => picker.comment.insert_str(&text.replace(['\r', '\n'], " ")),
                    Focus::Filter => {
                        picker.filter.insert_str(text.trim());
                        picker.filter_changed();
                    }
                    Focus::GoTo => picker.goto.insert_str(&normalize_pasted_path(text)),
                    Focus::Browser => {
                        // A dragged-in file is taken as the choice.
                        let path = expand_path(&normalize_pasted_path(text));
                        match picker.go_to(&path) {
                            Ok(()) if path.is_file() => {
                                picker.chosen = picker.current().map(|e| e.path.clone());
                                picker.focus = Focus::Comment;
                                picker.error = None;
                            }
                            Ok(()) => picker.error = None,
                            Err(e) => picker.error = Some(e),
                        }
                    }
                }
            }
            Some(Overlay::DownloadDir { input, .. }) => {
                input.insert_str(&normalize_pasted_path(text));
            }
            None if self.searching => {
                self.search[self.tab.index()].insert_str(&text.replace(['\r', '\n'], " "));
            }
            _ => {}
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return Vec::new();
        }
        if let Some(overlay) = self.overlay.take() {
            return self.on_overlay_key(overlay, key);
        }
        if self.session != Session::Ready {
            return self.on_session_key(key);
        }
        if self.searching {
            self.on_search_key(key);
            return Vec::new();
        }
        if self.zoomed {
            if let Some(effects) = self.on_zoom_key(key) {
                return effects;
            }
        }
        self.on_normal_key(key)
    }

    fn on_session_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        match (&self.session, key.code) {
            (_, KeyCode::Char('q')) => self.should_quit = true,
            (_, KeyCode::Char('?')) => self.overlay = Some(Overlay::Help),
            (Session::LoggedOut(_), KeyCode::Char('r') | KeyCode::Enter) => {
                self.session = Session::Checking;
                return vec![Effect::CheckSession];
            }
            _ => {}
        }
        Vec::new()
    }

    fn on_search_key(&mut self, key: KeyEvent) {
        let input = &mut self.search[self.tab.index()];
        match key.code {
            KeyCode::Esc => {
                input.clear();
                self.searching = false;
            }
            KeyCode::Enter => self.searching = false,
            KeyCode::Down => self.move_selection(1),
            KeyCode::Up => self.move_selection(-1),
            _ => {
                input.handle_key(key);
                self.offsets[self.tab.index()] = 0;
                self.selection_changed_at = Instant::now();
            }
        }
    }

    /// Keys that behave differently while the detail pane is zoomed.
    /// Returns `None` to fall through to normal handling.
    fn on_zoom_key(&mut self, key: KeyEvent) -> Option<Vec<Effect>> {
        let page = 10;
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Backspace => {
                self.zoomed = false
            }
            KeyCode::Down | KeyCode::Char('j') => self.scroll_detail(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_detail(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll_detail(page),
            KeyCode::PageUp | KeyCode::Char('b') => self.scroll_detail(-page),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_detail(page)
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_detail(-page)
            }
            KeyCode::Home | KeyCode::Char('g') => self.detail_scroll = 0,
            KeyCode::End | KeyCode::Char('G') => self.detail_scroll = self.detail_max_scroll,
            KeyCode::Char('n') => self.move_selection(1),
            KeyCode::Char('p') => self.move_selection(-1),
            _ => return None,
        }
        Some(Vec::new())
    }

    fn on_normal_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.overlay = Some(Overlay::Help),
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                self.switch_tab(self.tab.offset(1))
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.switch_tab(self.tab.offset(-1))
            }
            KeyCode::Char(c @ '1'..='4') => {
                self.switch_tab(Tab::ALL[c as usize - '1' as usize]);
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Char('d') if ctrl => self.move_selection(PAGE as isize),
            KeyCode::Char('u') if ctrl => self.move_selection(-(PAGE as isize)),
            KeyCode::PageDown => self.move_selection(PAGE as isize),
            KeyCode::PageUp => self.move_selection(-(PAGE as isize)),
            KeyCode::Home | KeyCode::Char('g') => self.move_selection(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.move_selection(isize::MAX / 2),
            KeyCode::Char('J') => self.scroll_detail(3),
            KeyCode::Char('K') => self.scroll_detail(-3),
            KeyCode::Enter => {
                if self.tab == Tab::Courses {
                    if let Some(c) = self.selected_course() {
                        let (id, name) = (c.id.clone(), c.name.clone());
                        self.course_filter = Some(id);
                        self.switch_tab(Tab::Homework);
                        self.info(format!("Showing {name} · Esc or c to change"));
                    }
                } else if self.selected_key(self.tab).is_some() {
                    self.zoomed = true;
                }
            }
            KeyCode::Esc => {
                if !self.search[self.tab.index()].is_empty() {
                    self.search[self.tab.index()].clear();
                } else if self.course_filter.is_some() {
                    self.course_filter = None;
                    self.info("Showing all courses");
                }
            }
            KeyCode::Char('/') => {
                self.searching = true;
                self.zoomed = false;
            }
            KeyCode::Char('c') => {
                let selected = self
                    .course_filter
                    .as_ref()
                    .and_then(|id| self.courses.current.iter().position(|c| &c.id == id))
                    .map_or(0, |i| i + 1);
                self.overlay = Some(Overlay::CoursePicker { selected });
            }
            KeyCode::Char('f') => match self.tab {
                Tab::Homework => {
                    self.homework_filter = self.homework_filter.next();
                    self.offsets[0] = 0;
                    self.selection_changed_at = Instant::now();
                }
                Tab::Announcements => {
                    self.announcement_filter = match self.announcement_filter {
                        AnnouncementFilter::All => AnnouncementFilter::Unread,
                        AnnouncementFilter::Unread => AnnouncementFilter::All,
                    };
                    self.offsets[1] = 0;
                }
                _ => {}
            },
            KeyCode::Char('r') => {
                if self.courses_loading || self.homework.loading {
                    self.info("Already refreshing…");
                } else {
                    self.details.clear();
                    self.info("Refreshing…");
                    return self.reload_all();
                }
            }
            KeyCode::Char('d') => return self.download_selected(),
            KeyCode::Char('s') => self.open_submit_form(),
            KeyCode::Char('D') => {
                self.overlay = Some(Overlay::DownloadDir {
                    input: Input::new(self.download_dir.display().to_string()),
                    error: None,
                });
            }
            KeyCode::Char('o') => match &self.last_download {
                Some(p) => return vec![Effect::OpenPath(p.clone())],
                None => self.info("Nothing downloaded yet in this session."),
            },
            _ => {}
        }
        Vec::new()
    }

    fn switch_tab(&mut self, tab: Tab) {
        if self.tab != tab {
            self.tab = tab;
            self.zoomed = false;
            self.selection_changed_at = Instant::now();
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let keys = self.visible_keys(self.tab);
        if keys.is_empty() {
            return;
        }
        let current = self.selected_index(self.tab).unwrap_or(0) as isize;
        let next = (current.saturating_add(delta)).clamp(0, keys.len() as isize - 1) as usize;
        self.selection[self.tab.index()] = Some(keys[next].clone());
        self.selection_changed_at = Instant::now();
        self.sync_detail_key();
    }

    fn scroll_detail(&mut self, delta: i32) {
        let next = (self.detail_scroll as i32 + delta).max(0);
        self.detail_scroll = (next.min(self.detail_max_scroll as i32)) as u16;
    }

    fn download_selected(&mut self) -> Vec<Effect> {
        match self.tab {
            Tab::Files => {
                let Some(file) = self.selected_file().cloned() else {
                    return Vec::new();
                };
                self.in_flight += 1;
                self.info(format!("Downloading {}…", file.title));
                vec![Effect::DownloadFile {
                    file,
                    dir: self.download_dir.clone(),
                }]
            }
            Tab::Homework => {
                let Some(hw) = self.selected_homework() else {
                    return Vec::new();
                };
                match self.details.get(&hw.student_homework_id) {
                    Some(DetailState::Loaded(d)) if !d.attachments.is_empty() => {
                        let attachments = d.attachments.clone();
                        self.in_flight += 1;
                        self.info(format!("Downloading {} attachment(s)…", attachments.len()));
                        vec![Effect::DownloadAttachments {
                            attachments,
                            dir: self.download_dir.clone(),
                        }]
                    }
                    Some(DetailState::Loaded(_)) => {
                        self.info("This homework has no attachments.");
                        Vec::new()
                    }
                    _ => {
                        self.info("Attachments are still loading…");
                        Vec::new()
                    }
                }
            }
            _ => Vec::new(),
        }
    }

    fn open_submit_form(&mut self) {
        if self.tab != Tab::Homework {
            return;
        }
        let Some(hw) = self.selected_homework() else {
            return;
        };
        let course = if hw.course_name.is_empty() {
            "(previous course)".to_string()
        } else {
            hw.course_name.clone()
        };
        self.overlay = Some(Overlay::Submit(Box::new(SubmitState {
            student_homework_id: hw.student_homework_id.clone(),
            title: hw.title.clone(),
            course,
            already_submitted: hw.status != HomeworkStatus::Pending,
            picker: FilePicker::open(&self.submit_dir),
        })));
    }

    fn on_overlay_key(&mut self, overlay: Overlay, key: KeyEvent) -> Vec<Effect> {
        match overlay {
            Overlay::Help => {
                if !matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Char('?' | 'q') | KeyCode::Enter
                ) {
                    self.overlay = Some(Overlay::Help);
                }
            }
            Overlay::CoursePicker { mut selected } => {
                let count = self.courses.current.len() + 1;
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q' | 'c') => return Vec::new(),
                    KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1) % count,
                    KeyCode::Up | KeyCode::Char('k') => selected = (selected + count - 1) % count,
                    KeyCode::Home | KeyCode::Char('g') => selected = 0,
                    KeyCode::End | KeyCode::Char('G') => selected = count - 1,
                    KeyCode::Enter => {
                        self.course_filter = selected
                            .checked_sub(1)
                            .and_then(|i| self.courses.current.get(i))
                            .map(|c| c.id.clone());
                        self.offsets = [0; 4];
                        self.selection_changed_at = Instant::now();
                        if self.tab == Tab::Courses {
                            self.switch_tab(Tab::Homework);
                        }
                        return Vec::new();
                    }
                    _ => {}
                }
                self.overlay = Some(Overlay::CoursePicker { selected });
            }
            Overlay::Submit(mut submit) => {
                if self.on_submit_key(&mut submit, key) {
                    self.overlay = Some(Overlay::Submit(submit));
                }
            }
            Overlay::Confirm {
                title,
                lines,
                effect,
                back,
            } => match key.code {
                KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                    if matches!(*effect, Effect::Submit { .. }) {
                        self.in_flight += 1;
                        self.info("Submitting…");
                    }
                    return vec![*effect];
                }
                KeyCode::Char('n' | 'N' | 'q') | KeyCode::Esc => match back {
                    Some(submit) => self.overlay = Some(Overlay::Submit(submit)),
                    None => self.info("Cancelled."),
                },
                _ => {
                    self.overlay = Some(Overlay::Confirm {
                        title,
                        lines,
                        effect,
                        back,
                    })
                }
            },
            Overlay::DownloadDir { mut input, error } => match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    let dir = expand_path(input.value());
                    if dir.exists() && !dir.is_dir() {
                        self.overlay = Some(Overlay::DownloadDir {
                            input,
                            error: Some("That path exists and is not a directory.".into()),
                        });
                    } else {
                        self.info(format!("Downloads now go to {}", dir.display()));
                        self.download_dir = dir;
                    }
                }
                KeyCode::Tab => {
                    let completion = complete_path(input.value());
                    if let Some(v) = completion.value {
                        input.set(v);
                    }
                    self.overlay = Some(Overlay::DownloadDir { input, error });
                }
                _ => {
                    input.handle_key(key);
                    self.overlay = Some(Overlay::DownloadDir { input, error: None });
                }
            },
        }
        Vec::new()
    }

    /// Handles a key on the submit screen. Returns `false` when the screen closes.
    fn on_submit_key(&mut self, submit: &mut SubmitState, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let p = &mut submit.picker;
        match p.focus {
            Focus::Filter => match key.code {
                KeyCode::Esc => {
                    p.filter.clear();
                    p.filter_changed();
                    p.focus = Focus::Browser;
                }
                KeyCode::Enter => p.focus = Focus::Browser,
                KeyCode::Down => p.move_by(1),
                KeyCode::Up => p.move_by(-1),
                _ => {
                    if p.filter.handle_key(key) {
                        p.filter_changed();
                    }
                }
            },
            Focus::GoTo => match key.code {
                KeyCode::Esc => {
                    p.goto.clear();
                    p.goto_candidates.clear();
                    p.error = None;
                    p.focus = Focus::Browser;
                }
                KeyCode::Tab => {
                    let completion = complete_path(p.goto.value());
                    p.goto_candidates = completion.candidates;
                    if let Some(v) = completion.value {
                        p.goto.set(v);
                    }
                }
                KeyCode::Enter => match p.go_to(&expand_path(p.goto.value())) {
                    Ok(()) => {
                        p.goto.clear();
                        p.goto_candidates.clear();
                        p.error = None;
                        p.focus = Focus::Browser;
                    }
                    Err(e) => p.error = Some(e),
                },
                _ => {
                    if p.goto.handle_key(key) {
                        p.error = None;
                        p.goto_candidates.clear();
                    }
                }
            },
            Focus::Comment => match key.code {
                KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab | KeyCode::Up => {
                    p.focus = Focus::Browser
                }
                KeyCode::Enter => return !self.review_submission(submit),
                _ => {
                    p.comment.handle_key(key);
                }
            },
            Focus::Browser => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    if p.filter.is_empty() {
                        self.info("Submission cancelled.");
                        return false;
                    }
                    p.filter.clear();
                    p.filter_changed();
                }
                KeyCode::Down | KeyCode::Char('j') => p.move_by(1),
                KeyCode::Up | KeyCode::Char('k') => p.move_by(-1),
                KeyCode::Char('d') if ctrl => p.move_by(PAGE as isize),
                KeyCode::Char('u') if ctrl => p.move_by(-(PAGE as isize)),
                KeyCode::PageDown => p.move_by(PAGE as isize),
                KeyCode::PageUp => p.move_by(-(PAGE as isize)),
                KeyCode::Home | KeyCode::Char('g') => p.move_by(isize::MIN / 2),
                KeyCode::End | KeyCode::Char('G') => p.move_by(isize::MAX / 2),
                KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => p.parent(),
                KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => {
                    if let Some(file) = p.open_selected() {
                        p.chosen = Some(file);
                        p.focus = Focus::Comment;
                        p.error = None;
                    }
                }
                KeyCode::Char(' ') => {
                    if let Some(path) = p.current().filter(|e| !e.is_dir).map(|e| e.path.clone()) {
                        p.chosen = if p.chosen.as_ref() == Some(&path) {
                            None
                        } else {
                            Some(path)
                        };
                    }
                }
                KeyCode::Char('.') => p.toggle_hidden(),
                KeyCode::Char('~') => p.home(),
                KeyCode::Char('/') => p.focus = Focus::Filter,
                KeyCode::Char(':') => {
                    let cwd = p.cwd.display().to_string();
                    p.goto.set(format!("{}/", cwd.trim_end_matches('/')));
                    p.focus = Focus::GoTo;
                }
                KeyCode::Tab => p.focus = Focus::Comment,
                _ => {}
            },
        }
        true
    }

    /// Moves a complete submission to the confirmation step. Returns whether it did.
    fn review_submission(&mut self, submit: &mut SubmitState) -> bool {
        let p = &mut submit.picker;
        let Some(path) = p.chosen.clone() else {
            p.error = Some("Choose a file first: press Enter on a file.".into());
            p.focus = Focus::Browser;
            return false;
        };
        if !path.is_file() {
            p.error = Some(format!("Not a file: {}", path.display()));
            p.focus = Focus::Browser;
            return false;
        }
        self.submit_dir = p.cwd.clone();
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let mut lines = vec![
            format!("{} | {}", submit.course, submit.title),
            format!("File: {} ({})", path.display(), human_size(size)),
        ];
        if !p.comment.is_empty() {
            lines.push(format!("Comment: {}", p.comment.value()));
        }
        if submit.already_submitted {
            lines.push("This homework already has a submission.".into());
        }
        self.overlay = Some(Overlay::Confirm {
            title: "Submit homework?".into(),
            lines,
            effect: Box::new(Effect::Submit {
                student_homework_id: submit.student_homework_id.clone(),
                file: path,
                comment: p.comment.value().to_string(),
            }),
            back: Some(Box::new(submit.clone())),
        });
        true
    }
}

fn matches_query(q: &str, fields: &[&str]) -> bool {
    q.is_empty() || fields.iter().any(|f| f.to_lowercase().contains(q))
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::TimeZone;

    pub fn local_dt(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, mo, d, h, mi, 0).single().unwrap()
    }

    pub fn homework(
        id: &str,
        course_id: &str,
        title: &str,
        deadline: Option<DateTime<Local>>,
        status: HomeworkStatus,
    ) -> Homework {
        Homework {
            student_homework_id: id.into(),
            base_id: format!("base-{id}"),
            course_id: course_id.into(),
            course_name: format!("Course {course_id}"),
            title: title.into(),
            deadline,
            deadline_raw: String::new(),
            submit_time: String::new(),
            grade: None,
            comment: String::new(),
            grader: String::new(),
            grade_time: String::new(),
            status,
        }
    }

    pub fn course(id: &str, name: &str) -> Course {
        Course {
            id: id.into(),
            name: name.into(),
            teacher: "Teacher".into(),
        }
    }

    pub fn notification(id: &str, course_id: &str, time: &str, read: bool) -> Notification {
        Notification {
            id: id.into(),
            course_id: course_id.into(),
            course_name: format!("Course {course_id}"),
            title: format!("Notice {id}"),
            publish_time: time.into(),
            publisher: "Teacher".into(),
            read,
            content: "Body".into(),
        }
    }

    pub fn ready_app() -> App {
        let now = local_dt(2026, 6, 1, 12, 0);
        let mut app = App::new(PathBuf::from("/tmp/downloads"), now);
        app.session = Session::Ready;
        app.courses = CourseData {
            semester: "2025-2026-2".into(),
            current: vec![course("c1", "Data Structures"), course("c2", "Networks")],
            previous: vec![],
        };
        app.homework.finish(Ok(vec![
            homework(
                "late",
                "c1",
                "Heap lab",
                Some(local_dt(2026, 6, 9, 23, 59)),
                HomeworkStatus::Pending,
            ),
            homework(
                "soon",
                "c2",
                "Socket report",
                Some(local_dt(2026, 6, 2, 8, 0)),
                HomeworkStatus::Pending,
            ),
            homework(
                "overdue",
                "c1",
                "Old quiz",
                Some(local_dt(2026, 5, 20, 8, 0)),
                HomeworkStatus::Pending,
            ),
            homework(
                "done",
                "c2",
                "Lab 0",
                Some(local_dt(2026, 5, 1, 8, 0)),
                HomeworkStatus::Graded,
            ),
        ]));
        app.announcements.finish(Ok(vec![
            notification("a1", "c1", "2026-05-01 09:00", true),
            notification("a2", "c2", "2026-05-30 09:00", false),
        ]));
        app
    }

    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    fn ids(list: Vec<&Homework>) -> Vec<&str> {
        list.into_iter()
            .map(|h| h.student_homework_id.as_str())
            .collect()
    }

    #[test]
    fn homework_filters_cycle_and_sort() {
        let mut app = ready_app();
        assert_eq!(ids(app.visible_homework()), vec!["soon", "late"]);
        press(&mut app, KeyCode::Char('f'));
        assert_eq!(app.homework_filter, HomeworkFilter::Overdue);
        assert_eq!(ids(app.visible_homework()), vec!["overdue"]);
        press(&mut app, KeyCode::Char('f'));
        assert_eq!(ids(app.visible_homework()), vec!["done"]);
        press(&mut app, KeyCode::Char('f'));
        assert_eq!(app.visible_homework().len(), 4);
        press(&mut app, KeyCode::Char('f'));
        assert_eq!(app.homework_filter, HomeworkFilter::Pending);
    }

    #[test]
    fn selection_follows_item_across_filter_changes() {
        let mut app = ready_app();
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected_homework().unwrap().student_homework_id, "late");
        // "All" sorts by deadline descending; the selected item stays selected.
        app.homework_filter = HomeworkFilter::All;
        assert_eq!(app.selected_homework().unwrap().student_homework_id, "late");
        assert_eq!(app.selected_index(Tab::Homework), Some(0));
    }

    #[test]
    fn search_filters_current_tab_and_esc_clears() {
        let mut app = ready_app();
        press(&mut app, KeyCode::Char('/'));
        assert!(app.searching);
        type_text(&mut app, "sock");
        assert_eq!(ids(app.visible_homework()), vec!["soon"]);
        press(&mut app, KeyCode::Enter);
        assert!(!app.searching);
        assert_eq!(app.visible_homework().len(), 1);
        // The query belongs to the Homework tab only.
        press(&mut app, KeyCode::Char('2'));
        assert_eq!(app.visible_announcements().len(), 2);
        press(&mut app, KeyCode::Char('1'));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.visible_homework().len(), 2);
    }

    #[test]
    fn course_picker_filters_every_tab() {
        let mut app = ready_app();
        press(&mut app, KeyCode::Char('c'));
        assert!(matches!(
            app.overlay,
            Some(Overlay::CoursePicker { selected: 0 })
        ));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.course_filter.as_deref(), Some("c1"));
        assert_eq!(ids(app.visible_homework()), vec!["late"]);
        assert_eq!(app.visible_announcements().len(), 1);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.course_filter, None);
    }

    #[test]
    fn courses_tab_enter_filters_homework() {
        let mut app = ready_app();
        press(&mut app, KeyCode::Char('4'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.tab, Tab::Homework);
        assert_eq!(app.course_filter.as_deref(), Some("c2"));
        assert_eq!(ids(app.visible_homework()), vec!["soon"]);
    }

    #[test]
    fn unread_filter_hides_read_announcements() {
        let mut app = ready_app();
        press(&mut app, KeyCode::Char('2'));
        assert_eq!(app.visible_announcements()[0].id, "a2");
        press(&mut app, KeyCode::Char('f'));
        let visible = app.visible_announcements();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, "a2");
        assert_eq!(app.unread_count(None), 1);
    }

    #[test]
    fn tick_debounces_homework_detail_fetch() {
        let mut app = ready_app();
        let start = Instant::now();
        app.selection_changed_at = start;
        assert!(app.on_tick(start, app.now).is_empty());
        let effects = app.on_tick(start + DETAIL_DEBOUNCE * 2, app.now);
        assert_eq!(
            effects,
            vec![Effect::LoadHomeworkDetail {
                course_id: "c2".into(),
                student_homework_id: "soon".into(),
                base_id: "base-soon".into(),
            }]
        );
        // Already requested: no duplicate fetch.
        assert!(app.on_tick(start + DETAIL_DEBOUNCE * 3, app.now).is_empty());
    }

    #[test]
    fn download_waits_for_loaded_attachments() {
        let mut app = ready_app();
        assert!(press(&mut app, KeyCode::Char('d')).is_empty());
        let attachment = HomeworkAttachment {
            section: "作业附件".into(),
            filename: "spec.pdf".into(),
            download_path: "https://learn.tsinghua.edu.cn/x".into(),
        };
        app.details.insert(
            "soon".into(),
            DetailState::Loaded(HomeworkDetail {
                description: String::new(),
                attachments: vec![attachment.clone()],
            }),
        );
        let effects = press(&mut app, KeyCode::Char('d'));
        assert_eq!(
            effects,
            vec![Effect::DownloadAttachments {
                attachments: vec![attachment],
                dir: PathBuf::from("/tmp/downloads"),
            }]
        );
        assert_eq!(app.in_flight, 1);
        app.on_msg(Msg::Downloaded(Ok(vec![PathBuf::from(
            "/tmp/downloads/spec.pdf",
        )])));
        assert_eq!(app.in_flight, 0);
        assert_eq!(
            press(&mut app, KeyCode::Char('o')),
            vec![Effect::OpenPath(PathBuf::from("/tmp/downloads/spec.pdf"))]
        );
    }

    #[test]
    fn submit_picks_file_in_browser_then_confirms() {
        let dir = std::env::temp_dir().join(format!("thu-learn-submit-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("drafts")).unwrap();
        std::fs::write(dir.join("answer.pdf"), b"pdf").unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();

        let mut app = ready_app();
        app.submit_dir = dir.clone();
        press(&mut app, KeyCode::Char('s'));
        let picker = |app: &App| match &app.overlay {
            Some(Overlay::Submit(s)) => s.picker.clone(),
            other => panic!("submit screen should be open, got {other:?}"),
        };
        assert_eq!(picker(&app).cwd, dir);

        // Enter on a folder opens it; h goes back and keeps the folder selected.
        press(&mut app, KeyCode::Enter);
        assert_eq!(picker(&app).cwd, dir.join("drafts"));
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(picker(&app).current().unwrap().name, "drafts");

        // Reviewing without a file explains what to do.
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Enter);
        assert!(picker(&app).error.unwrap().starts_with("Choose a file"));

        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter); // choose answer.pdf, focus comment
        assert_eq!(picker(&app).chosen, Some(dir.join("answer.pdf")));
        assert_eq!(picker(&app).focus, Focus::Comment);
        type_text(&mut app, "final version");
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.overlay, Some(Overlay::Confirm { .. })));

        // Declining returns to the picker with the choice intact.
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(picker(&app).comment.value(), "final version");
        press(&mut app, KeyCode::Enter);
        let effects = press(&mut app, KeyCode::Char('y'));
        assert_eq!(
            effects,
            vec![Effect::Submit {
                student_homework_id: "soon".into(),
                file: dir.join("answer.pdf"),
                comment: "final version".into(),
            }]
        );
        assert!(app.overlay.is_none());
        assert_eq!(app.submit_dir, dir);

        let reload = app.on_msg(Msg::Submitted {
            id: "soon".into(),
            result: Ok(()),
        });
        assert!(matches!(reload.as_slice(), [Effect::LoadHomework { .. }]));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pasting_a_file_path_chooses_it() {
        let dir = std::env::temp_dir().join(format!("thu-learn-paste-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("my report.pdf"), b"pdf").unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();

        let mut app = ready_app();
        press(&mut app, KeyCode::Char('s'));
        app.on_paste(&format!("'{}'", dir.join("my report.pdf").display()));
        let Some(Overlay::Submit(submit)) = &app.overlay else {
            panic!("submit screen should stay open");
        };
        assert_eq!(submit.picker.chosen, Some(dir.join("my report.pdf")));
        assert_eq!(submit.picker.focus, Focus::Comment);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn logged_out_screen_retries_session_check() {
        let mut app = App::new(PathBuf::from("."), local_dt(2026, 6, 1, 12, 0));
        assert_eq!(app.init(), vec![Effect::CheckSession]);
        app.on_msg(Msg::SessionFailed("No saved session".into()));
        assert!(press(&mut app, KeyCode::Char('j')).is_empty());
        assert_eq!(
            press(&mut app, KeyCode::Char('r')),
            vec![Effect::CheckSession]
        );
        assert_eq!(app.session, Session::Checking);
        assert_eq!(app.on_msg(Msg::SessionReady), vec![Effect::LoadAll]);
        assert_eq!(app.session, Session::Ready);
    }

    #[test]
    fn human_size_formats_units() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }
}
