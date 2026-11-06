//! TUI state and key handling.
//!
//! [`App`] is plain state: key presses and background results update it and
//! return [`Effect`]s, which the runtime in [`super`] executes. Keeping network
//! I/O out of this module makes the interaction logic testable without Learn.
//! The only I/O here is local directory listing for the submission file picker.
//!
//! Logging in is not the TUI's job: `Client` renews expired sessions with saved
//! credentials, and without a usable session the TUI asks the user to run
//! `thu-learn login` and retry.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use super::input::{complete_path, expand_path, normalize_pasted_path, Input};
use super::picker::{CursorShape, Outcome, Picker};
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

/// The semester shown and its courses. Every tab shows one semester at a time.
#[derive(Debug, Clone, Default)]
pub struct CourseData {
    /// Semester ID, such as `2026-2027-1`.
    pub semester: String,
    /// Learn's current semester, shown first.
    pub current_semester: String,
    /// Semesters to switch to, newest first.
    pub semesters: Vec<String>,
    /// Courses of the shown semester.
    pub list: Vec<Course>,
}

/// `2025-2026-1` as `Fall 2025`: Learn numbers fall 1, spring 2, and summer 3.
pub fn semester_label(id: &str) -> String {
    let parts: Vec<&str> = id.split('-').collect();
    match parts.as_slice() {
        [y1, _, "1"] => format!("Fall {y1}"),
        [_, y2, "2"] => format!("Spring {y2}"),
        [_, y2, "3"] => format!("Summer {y2}"),
        _ => id.to_string(),
    }
}

/// yazi's visual mode in the Files tab: the files between `anchor` and the
/// cursor count as selected (or, with `unset`, as unselected) until `Esc` or
/// an action applies the range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visual {
    /// File ID where the range starts.
    anchor: String,
    pub unset: bool,
}

/// Screen areas of the last frame, for mouse hit-testing. The renderer
/// clears and refills them on every draw, so hidden panes have empty areas.
#[derive(Debug, Clone, Default)]
pub struct Hits {
    /// Header tab titles.
    pub tabs: Vec<(Rect, Tab)>,
    /// The list pane, and its item rows without the border and header.
    pub list: Rect,
    pub rows: Rect,
    pub detail: Rect,
    /// The semester in the header.
    pub semester: Rect,
    /// Course or semester picker rows and the index of the first one shown.
    pub menu: Rect,
    pub menu_offset: usize,
    /// The confirmation's `[Y]es` and `(N)o` buttons.
    pub yes: Rect,
    pub no: Rect,
    /// Key hints in the bottom bar and action hints at the end of the
    /// details; clicking one presses its key.
    pub keys: Vec<(Rect, KeyEvent)>,
    pub actions: Vec<(Rect, KeyEvent)>,
}

/// Work for the runtime to perform outside the pure state.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    CheckSession,
    /// Load a semester's courses, homework, announcements, and files;
    /// `None` is Learn's current semester.
    LoadAll {
        semester: Option<String>,
    },
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
    /// Download several files into one folder per course under `dir`.
    DownloadFiles {
        files: Vec<CourseFile>,
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
    /// Put text on the system clipboard (the picker's `c` keys).
    Copy(String),
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
    DownloadProgress {
        done: usize,
        total: usize,
    },
    /// Saved paths and `name: error` descriptions of failed downloads.
    Downloaded {
        saved: Vec<PathBuf>,
        failed: Vec<String>,
    },
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
    pub picker: Picker,
}

#[derive(Debug, Clone)]
pub enum Overlay {
    Help,
    /// Index 0 is "All courses"; index `n` is `courses.list[n - 1]`.
    CoursePicker {
        selected: usize,
    },
    /// Index into `courses.semesters`.
    SemesterPicker {
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
    /// Files selected with yazi's selection keys, by file ID.
    pub selected_files: HashSet<String>,
    pub visual: Option<Visual>,
    pub homework_filter: HomeworkFilter,
    pub announcement_filter: AnnouncementFilter,
    /// Per-tab search queries, indexed by [`Tab::index`].
    pub search: [Input; 4],
    pub searching: bool,
    /// Selected item key per tab; resolved against the visible list on each use.
    selection: [Option<String>; 4],
    /// First visible row per tab, persisted between frames for stable scrolling.
    pub offsets: [usize; 4],
    /// Visible rows of the list and detail panes, written by the renderer for paging.
    pub list_height: usize,
    pub detail_height: u16,
    /// First key of a two-key sequence such as `gg`.
    pending_g: bool,
    selection_changed_at: Instant,
    pub zoomed: bool,
    pub detail_scroll: u16,
    /// Largest useful scroll value, written by the renderer.
    pub detail_max_scroll: u16,
    /// Written by the renderer.
    pub hits: Hits,
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
            selected_files: HashSet::new(),
            visual: None,
            homework_filter: HomeworkFilter::Pending,
            announcement_filter: AnnouncementFilter::All,
            search: Default::default(),
            searching: false,
            selection: Default::default(),
            offsets: [0; 4],
            list_height: PAGE,
            detail_height: PAGE as u16,
            pending_g: false,
            selection_changed_at: Instant::now(),
            zoomed: false,
            detail_scroll: 0,
            detail_max_scroll: u16::MAX,
            hits: Hits::default(),
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

    /// Cursor shape for the picker's inputs; `None` keeps the terminal default.
    pub fn cursor_shape(&self) -> Option<CursorShape> {
        match &self.overlay {
            Some(Overlay::Submit(submit)) => submit.picker.cursor_shape(),
            _ => None,
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
            .list
            .iter()
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
            .list
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
        self.courses.list.iter().find(|c| c.id == key)
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

    /// File IDs from the visual anchor to the cursor, while the anchor is visible.
    fn visual_range(&self) -> Option<Vec<String>> {
        let visual = self.visual.as_ref()?;
        let keys = self.visible_keys(Tab::Files);
        let anchor = keys.iter().position(|k| *k == visual.anchor)?;
        let cursor = self.selected_index(Tab::Files)?;
        Some(keys[anchor.min(cursor)..=anchor.max(cursor)].to_vec())
    }

    /// Selected file IDs, counting an unapplied visual range as applied.
    pub fn marked_files(&self) -> HashSet<String> {
        let mut marked = self.selected_files.clone();
        if let (Some(visual), Some(range)) = (&self.visual, self.visual_range()) {
            for id in range {
                if visual.unset {
                    marked.remove(&id);
                } else {
                    marked.insert(id);
                }
            }
        }
        marked
    }

    /// Ends visual mode and applies its range to the selection.
    fn apply_visual(&mut self) {
        if self.visual.is_some() {
            self.selected_files = self.marked_files();
            self.visual = None;
        }
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
                            .is_some_and(|id| data.list.iter().any(|c| &c.id == id));
                        if !keep_filter {
                            self.course_filter = None;
                        }
                        self.courses = data;
                    }
                    Err(e) => {
                        self.homework.loading = false;
                        self.announcements.loading = false;
                        self.files.loading = false;
                        // Authentication errors already say when to run `thu-learn login`.
                        self.error(format!("Could not load courses: {e} (press r to retry)"));
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
                let ids: HashSet<&str> = files.iter().map(|f| f.id.as_str()).collect();
                self.selected_files.retain(|id| ids.contains(id.as_str()));
                self.files.finish(Ok(files));
            }
            Msg::HomeworkDetail { id, result } => {
                let state = match result {
                    Ok(d) => DetailState::Loaded(d),
                    Err(e) => DetailState::Failed(e),
                };
                self.details.insert(id, state);
            }
            Msg::DownloadProgress { done, total } => {
                self.info(format!("Downloading… {done} of {total}"));
            }
            Msg::Downloaded { saved, failed } => {
                self.in_flight = self.in_flight.saturating_sub(1);
                self.report_download(saved, failed);
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
                                courses: self.courses.list.clone(),
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

    fn report_download(&mut self, saved: Vec<PathBuf>, failed: Vec<String>) {
        let target = match saved.as_slice() {
            [] => {
                self.error(format!("Download failed: {}", failed.join("; ")));
                return;
            }
            [one] => one.clone(),
            [first, rest @ ..] => match first.parent() {
                Some(parent) if rest.iter().all(|p| p.parent() == Some(parent)) => {
                    parent.to_path_buf()
                }
                _ => self.download_dir.clone(),
            },
        };
        let total = saved.len() + failed.len();
        if !failed.is_empty() {
            self.error(format!(
                "Saved {} of {total} files · failed: {}",
                saved.len(),
                failed.join("; ")
            ));
        } else if saved.len() == 1 {
            self.success(format!("Saved {} · o to open", target.display()));
        } else {
            self.success(format!(
                "Saved {total} files to {} · o to open the folder",
                target.display()
            ));
        }
        self.last_download = Some(target);
    }

    /// Reloads the shown semester, or Learn's current one before any has loaded.
    fn reload_all(&mut self) -> Vec<Effect> {
        self.courses_loading = true;
        self.homework.start();
        self.announcements.start();
        self.files.start();
        let semester = Some(self.courses.semester.clone()).filter(|s| !s.is_empty());
        vec![Effect::LoadAll { semester }]
    }

    fn lists_loading(&self) -> bool {
        self.courses_loading
            || self.homework.loading
            || self.announcements.loading
            || self.files.loading
    }

    fn open_semester_picker(&mut self) {
        if self.lists_loading() {
            self.info("Still loading; switch semesters once it finishes.");
            return;
        }
        let shown = &self.courses.semester;
        let selected = self
            .courses
            .semesters
            .iter()
            .position(|s| s == shown)
            .unwrap_or(0);
        self.overlay = Some(Overlay::SemesterPicker { selected });
    }

    /// Shows another semester. Its lists start empty rather than showing the
    /// old semester's items under the new name.
    fn switch_semester(&mut self, semester: String) -> Vec<Effect> {
        if semester == self.courses.semester {
            return Vec::new();
        }
        self.apply_visual();
        self.info(format!(
            "Showing {semester} ({})",
            semester_label(&semester)
        ));
        self.courses.semester = semester;
        self.courses.list.clear();
        self.homework = Remote::default();
        self.announcements = Remote::default();
        self.files = Remote::default();
        self.details.clear();
        self.selected_files.clear();
        self.course_filter = None;
        self.selection = Default::default();
        self.offsets = [0; 4];
        self.zoomed = false;
        self.reload_all()
    }

    // ---------- Timer ----------

    pub fn on_tick(&mut self, now: Instant, wall: DateTime<Local>) -> Vec<Effect> {
        self.spinner = self.spinner.wrapping_add(1);
        self.now = wall;
        if let Some(Overlay::Submit(submit)) = self.overlay.as_mut() {
            submit.picker.tick(now);
        }
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
            Some(Overlay::Submit(submit)) => submit.picker.on_paste(text),
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
        // Ctrl-C quits, except in the submit picker, where it closes the picker
        // or its confirmation as in yazi.
        let in_picker = matches!(
            self.overlay,
            Some(Overlay::Submit(_)) | Some(Overlay::Confirm { back: Some(_), .. })
        );
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c')
            && !in_picker
        {
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
        // `gg` jumps to the top, as in yazi; any other key cancels the prefix.
        if std::mem::take(&mut self.pending_g) {
            if key.code == KeyCode::Char('g') {
                if self.zoomed {
                    self.detail_scroll = 0;
                } else {
                    self.move_selection(isize::MIN / 2);
                }
            }
            return Vec::new();
        }
        if key.code == KeyCode::Char('g') {
            self.pending_g = true;
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
        let page = self.detail_height.max(1) as i32;
        match paging(key) {
            Some(Paging::Half(dir)) => self.scroll_detail(dir * (page / 2).max(1)),
            Some(Paging::Full(dir)) => self.scroll_detail(dir * page),
            None => {}
        }
        if paging(key).is_some() {
            return Some(Vec::new());
        }
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Backspace => {
                self.zoomed = false
            }
            KeyCode::Down | KeyCode::Char('j') => self.scroll_detail(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_detail(-1),
            KeyCode::Char(' ') => self.scroll_detail(page),
            KeyCode::Home => self.detail_scroll = 0,
            KeyCode::End | KeyCode::Char('G') => self.detail_scroll = self.detail_max_scroll,
            KeyCode::Char('n') => self.move_selection(1),
            KeyCode::Char('p') => self.move_selection(-1),
            _ => return None,
        }
        Some(Vec::new())
    }

    /// yazi's selection keys, which act on the Files list. Returns whether
    /// `key` was one of them.
    fn on_select_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let is_select = match key.code {
            KeyCode::Char(' ' | 'v' | 'V') => !ctrl,
            KeyCode::Char('a' | 'r') => ctrl,
            _ => false,
        };
        if !is_select || self.zoomed {
            return is_select;
        }
        if self.tab != Tab::Files {
            self.info("Select files in the Files tab.");
            return true;
        }
        match key.code {
            KeyCode::Char(' ') => {
                if let Some(id) = self.selected_key(Tab::Files) {
                    if !self.selected_files.remove(&id) {
                        self.selected_files.insert(id);
                    }
                }
                self.move_selection(1);
            }
            KeyCode::Char(c @ ('v' | 'V')) => {
                // As in yazi, entering visual mode again starts a new range here.
                if let Some(anchor) = self.selected_key(Tab::Files) {
                    self.visual = Some(Visual {
                        anchor,
                        unset: c == 'V',
                    });
                }
            }
            KeyCode::Char('a') => {
                self.apply_visual();
                let visible = self.visible_keys(Tab::Files);
                self.selected_files.extend(visible);
            }
            _ => {
                self.apply_visual();
                for id in self.visible_keys(Tab::Files) {
                    if !self.selected_files.remove(&id) {
                        self.selected_files.insert(id);
                    }
                }
            }
        }
        true
    }

    fn on_normal_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if self.on_select_key(key) {
            return Vec::new();
        }
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
            _ if paging(key).is_some() => {
                let page = self.list_height.max(1) as isize;
                match paging(key) {
                    Some(Paging::Half(dir)) => {
                        self.move_selection(dir as isize * (page / 2).max(1))
                    }
                    Some(Paging::Full(dir)) => self.move_selection(dir as isize * page),
                    None => {}
                }
            }
            KeyCode::Home => self.move_selection(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.move_selection(isize::MAX / 2),
            KeyCode::Char('J') => self.scroll_detail(3),
            KeyCode::Char('K') => self.scroll_detail(-3),
            KeyCode::Enter => self.open_selected(),
            // yazi's escape order: visual range, then filter, then selection.
            KeyCode::Esc => {
                if self.visual.is_some() {
                    self.apply_visual();
                } else if !self.search[self.tab.index()].is_empty() {
                    self.search[self.tab.index()].clear();
                } else if self.tab == Tab::Files && !self.selected_files.is_empty() {
                    self.selected_files.clear();
                } else if self.course_filter.is_some() {
                    self.course_filter = None;
                    self.info("Showing all courses");
                }
            }
            KeyCode::Char('/') => {
                self.apply_visual();
                self.searching = true;
                self.zoomed = false;
            }
            KeyCode::Char('c') => {
                self.apply_visual();
                let selected = self
                    .course_filter
                    .as_ref()
                    .and_then(|id| self.courses.list.iter().position(|c| &c.id == id))
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
            KeyCode::Char('S') => {
                self.apply_visual();
                self.open_semester_picker();
            }
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

    /// Opens details full screen; in Courses, shows that course's homework.
    fn open_selected(&mut self) {
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

    fn switch_tab(&mut self, tab: Tab) {
        if self.tab != tab {
            self.apply_visual();
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
                self.apply_visual();
                if !self.selected_files.is_empty() {
                    self.confirm_batch_download();
                    return Vec::new();
                }
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

    /// Asks before downloading every selected file into one folder per course.
    fn confirm_batch_download(&mut self) {
        let mut files: Vec<CourseFile> = self
            .files
            .data
            .iter()
            .filter(|f| self.selected_files.contains(&f.id))
            .cloned()
            .collect();
        // Same order as the list, including selected files a search now hides.
        files.sort_by(|a, b| b.upload_time.cmp(&a.upload_time));
        let courses: HashSet<&str> = files.iter().map(|f| f.course_id.as_str()).collect();
        let mut lines = vec![
            format!(
                "{} from {}",
                count(files.len(), "file"),
                count(courses.len(), "course")
            ),
            format!("To {}, one folder per course", self.download_dir.display()),
            String::new(),
        ];
        const LISTED: usize = 5;
        for f in files.iter().take(LISTED) {
            lines.push(format!("{} | {}", f.course_name, f.title));
        }
        if files.len() > LISTED {
            lines.push(format!("… and {} more", files.len() - LISTED));
        }
        self.overlay = Some(Overlay::Confirm {
            title: format!("Download {}?", count(files.len(), "file")),
            lines,
            effect: Box::new(Effect::DownloadFiles {
                files,
                dir: self.download_dir.clone(),
            }),
            back: None,
        });
    }

    fn open_submit_form(&mut self) {
        if self.tab != Tab::Homework {
            return;
        }
        let Some(hw) = self.selected_homework() else {
            return;
        };
        let course = if hw.course_name.is_empty() {
            "(unknown course)".to_string()
        } else {
            hw.course_name.clone()
        };
        self.overlay = Some(Overlay::Submit(Box::new(SubmitState {
            student_homework_id: hw.student_homework_id.clone(),
            title: hw.title.clone(),
            course,
            already_submitted: hw.status != HomeworkStatus::Pending,
            picker: Picker::open(&self.submit_dir),
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
                let count = self.courses.list.len() + 1;
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q' | 'c') => return Vec::new(),
                    KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1) % count,
                    KeyCode::Up | KeyCode::Char('k') => selected = (selected + count - 1) % count,
                    KeyCode::Home | KeyCode::Char('g') => selected = 0,
                    KeyCode::End | KeyCode::Char('G') => selected = count - 1,
                    KeyCode::Enter => {
                        self.course_filter = selected
                            .checked_sub(1)
                            .and_then(|i| self.courses.list.get(i))
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
            Overlay::SemesterPicker { mut selected } => {
                let count = self.courses.semesters.len().max(1);
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q' | 'S') => return Vec::new(),
                    KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1) % count,
                    KeyCode::Up | KeyCode::Char('k') => selected = (selected + count - 1) % count,
                    KeyCode::Home | KeyCode::Char('g') => selected = 0,
                    KeyCode::End | KeyCode::Char('G') => selected = count - 1,
                    KeyCode::Enter => {
                        return match self.courses.semesters.get(selected).cloned() {
                            Some(semester) => self.switch_semester(semester),
                            None => Vec::new(),
                        };
                    }
                    _ => {}
                }
                self.overlay = Some(Overlay::SemesterPicker { selected });
            }
            Overlay::Submit(mut submit) => {
                let outcome = submit.picker.on_key(key);
                return self.on_picker_outcome(submit, outcome);
            }
            Overlay::Confirm {
                title,
                lines,
                effect,
                back,
            } => match key.code {
                KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                    match &*effect {
                        Effect::Submit { .. } => {
                            self.in_flight += 1;
                            self.info("Submitting…");
                        }
                        Effect::DownloadFiles { files, .. } => {
                            self.in_flight += 1;
                            self.info(format!("Downloading {}…", count(files.len(), "file")));
                            // yazi clears the selection once an action has used it.
                            self.selected_files.clear();
                        }
                        _ => {}
                    }
                    return vec![*effect];
                }
                KeyCode::Char('n' | 'N' | 'q') | KeyCode::Esc => match back {
                    Some(submit) => self.overlay = Some(Overlay::Submit(submit)),
                    None => self.info("Cancelled."),
                },
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => match back {
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

    fn on_picker_outcome(&mut self, submit: Box<SubmitState>, outcome: Outcome) -> Vec<Effect> {
        match outcome {
            Outcome::Continue => self.overlay = Some(Overlay::Submit(submit)),
            Outcome::Cancel => self.info("Submission cancelled."),
            Outcome::Copy(text) => {
                self.overlay = Some(Overlay::Submit(submit));
                return vec![Effect::Copy(text)];
            }
            Outcome::Submit { file, comment } => self.review_submission(submit, file, comment),
        }
        Vec::new()
    }

    /// Mouse input, following yazi: a click on a list row selects it, a right
    /// click also opens it, and the wheel moves the list or scrolls details.
    /// Tabs, course picker rows, and confirmation buttons can be clicked.
    pub fn on_mouse(&mut self, event: MouseEvent) -> Vec<Effect> {
        let pos = Position::new(event.column, event.row);
        let (button, step) = match event.kind {
            MouseEventKind::Down(b) => (Some(b), 0),
            MouseEventKind::ScrollDown => (None, 1),
            MouseEventKind::ScrollUp => (None, -1),
            _ => return Vec::new(),
        };
        let hint = |hits: &[(Rect, KeyEvent)]| {
            let left = button == Some(MouseButton::Left);
            let hit = hits.iter().find(|(r, _)| r.contains(pos));
            hit.filter(|_| left).map(|&(_, key)| key)
        };
        if let Some(key) = hint(&self.hits.keys) {
            return self.on_key(key);
        }
        if let Some(overlay) = self.overlay.take() {
            return self.on_overlay_mouse(overlay, event, pos);
        }
        if self.session != Session::Ready {
            return Vec::new();
        }
        self.pending_g = false;
        if let Some(key) = hint(&self.hits.actions) {
            self.searching = false;
            return self.on_key(key);
        }
        match button {
            None if self.hits.detail.contains(pos) => self.scroll_detail(3 * step),
            None if self.hits.list.contains(pos) => self.move_selection(step as isize),
            Some(MouseButton::Left | MouseButton::Right) => {
                let tab = self.hits.tabs.iter().find(|(r, _)| r.contains(pos));
                if let Some(&(_, tab)) = tab {
                    self.searching = false;
                    self.switch_tab(tab);
                } else if self.hits.semester.contains(pos) {
                    self.searching = false;
                    self.apply_visual();
                    self.open_semester_picker();
                } else if let Some(row) = self.row_at(pos) {
                    self.searching = false;
                    let current = self.selected_index(self.tab).unwrap_or(0);
                    self.move_selection(row as isize - current as isize);
                    if button == Some(MouseButton::Right) {
                        self.open_selected();
                    }
                }
            }
            _ => {}
        }
        Vec::new()
    }

    /// The visible list row under `pos`.
    fn row_at(&self, pos: Position) -> Option<usize> {
        if !self.hits.rows.contains(pos) {
            return None;
        }
        let row = self.offsets[self.tab.index()] + (pos.y - self.hits.rows.y) as usize;
        (row < self.visible_keys(self.tab).len()).then_some(row)
    }

    /// Popups take clicks on their own rows and buttons, as the matching key.
    fn on_overlay_mouse(
        &mut self,
        overlay: Overlay,
        event: MouseEvent,
        pos: Position,
    ) -> Vec<Effect> {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let left = event.kind == MouseEventKind::Down(MouseButton::Left);
        match overlay {
            Overlay::Submit(mut submit) => {
                let outcome = submit.picker.on_mouse(event);
                self.on_picker_outcome(submit, outcome)
            }
            overlay @ (Overlay::CoursePicker { .. } | Overlay::SemesterPicker { .. }) => {
                let (count, pick): (usize, fn(usize) -> Overlay) = match overlay {
                    Overlay::CoursePicker { .. } => (self.courses.list.len() + 1, |selected| {
                        Overlay::CoursePicker { selected }
                    }),
                    _ => (self.courses.semesters.len(), |selected| {
                        Overlay::SemesterPicker { selected }
                    }),
                };
                let row = self.hits.menu_offset + pos.y.saturating_sub(self.hits.menu.y) as usize;
                match event.kind {
                    MouseEventKind::ScrollDown => self.on_overlay_key(overlay, key(KeyCode::Down)),
                    MouseEventKind::ScrollUp => self.on_overlay_key(overlay, key(KeyCode::Up)),
                    _ if left && self.hits.menu.contains(pos) && row < count => {
                        self.on_overlay_key(pick(row), key(KeyCode::Enter))
                    }
                    _ => {
                        self.overlay = Some(overlay);
                        Vec::new()
                    }
                }
            }
            Overlay::Confirm { .. } if left && self.hits.yes.contains(pos) => {
                self.on_overlay_key(overlay, key(KeyCode::Char('y')))
            }
            Overlay::Confirm { .. } if left && self.hits.no.contains(pos) => {
                self.on_overlay_key(overlay, key(KeyCode::Char('n')))
            }
            overlay => {
                self.overlay = Some(overlay);
                Vec::new()
            }
        }
    }

    /// Shows the confirmation for a file chosen in the picker; declining it
    /// returns to the picker as it was.
    fn review_submission(&mut self, mut submit: Box<SubmitState>, file: PathBuf, comment: String) {
        // The file may have changed since the folder was listed.
        let size = match std::fs::metadata(&file) {
            Ok(m) if m.is_file() => m.len(),
            _ => {
                submit.picker.warn(
                    "Submit",
                    format!("{} is no longer a regular file.", file.display()),
                );
                self.overlay = Some(Overlay::Submit(submit));
                return;
            }
        };
        self.submit_dir = submit.picker.cwd.clone();
        // Warnings first, so a short terminal never hides them.
        let mut lines = Vec::new();
        if submit.already_submitted {
            lines.push("This homework already has a submission.".to_string());
        }
        lines.push(format!("{} | {}", submit.course, submit.title));
        lines.push(format!("File: {} ({})", file.display(), human_size(size)));
        if !comment.is_empty() {
            lines.push(format!("Comment: {comment}"));
        }
        self.overlay = Some(Overlay::Confirm {
            title: "Submit homework?".into(),
            lines,
            effect: Box::new(Effect::Submit {
                student_homework_id: submit.student_homework_id.clone(),
                file,
                comment,
            }),
            back: Some(submit),
        });
    }
}

/// Page movement keys from yazi's default keymap.
pub enum Paging {
    /// `-1` up or `1` down by half a page.
    Half(i32),
    /// `-1` up or `1` down by a full page.
    Full(i32),
}

/// `C-u`/`C-d` and `S-PageUp`/`S-PageDown` move half a page; `C-b`/`C-f` and
/// `PageUp`/`PageDown` move a full page.
pub fn paging(key: KeyEvent) -> Option<Paging> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        KeyCode::Char('u') if ctrl => Some(Paging::Half(-1)),
        KeyCode::Char('d') if ctrl => Some(Paging::Half(1)),
        KeyCode::Char('b') if ctrl => Some(Paging::Full(-1)),
        KeyCode::Char('f') if ctrl => Some(Paging::Full(1)),
        KeyCode::PageUp if shift => Some(Paging::Half(-1)),
        KeyCode::PageDown if shift => Some(Paging::Half(1)),
        KeyCode::PageUp => Some(Paging::Full(-1)),
        KeyCode::PageDown => Some(Paging::Full(1)),
        _ => None,
    }
}

/// `1 file`, `3 files`.
fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
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
            current_semester: "2025-2026-2".into(),
            semesters: vec!["2025-2026-2".into(), "2025-2026-1".into()],
            list: vec![course("c1", "Data Structures"), course("c2", "Networks")],
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

    pub fn course_file(id: &str, course_id: &str, title: &str, uploaded: &str) -> CourseFile {
        let course_name = if course_id == "c1" {
            "Data Structures"
        } else {
            "Networks"
        };
        CourseFile {
            id: id.into(),
            course_id: course_id.into(),
            course_name: course_name.into(),
            title: title.into(),
            size: "1.0M".into(),
            file_type: "pdf".into(),
            upload_time: uploaded.into(),
            description: String::new(),
        }
    }

    /// [`ready_app`] on the Files tab. Newest first, the list is f4, f3, f2, f1.
    pub fn files_app() -> App {
        let mut app = ready_app();
        app.files.finish(Ok(vec![
            course_file("f1", "c1", "L1 intro", "2026-03-01 10:00"),
            course_file("f2", "c1", "L2 heap", "2026-03-08 10:00"),
            course_file("f3", "c2", "Lab 1", "2026-03-15 10:00"),
            course_file("f4", "c2", "Lab 2", "2026-03-22 10:00"),
        ]));
        app.tab = Tab::Files;
        app
    }

    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(app: &mut App, c: char) -> Vec<Effect> {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    /// Selected file IDs, sorted, counting an unapplied visual range.
    fn marked(app: &App) -> Vec<String> {
        let mut ids: Vec<String> = app.marked_files().into_iter().collect();
        ids.sort();
        ids
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
        app.on_msg(Msg::Downloaded {
            saved: vec![PathBuf::from("/tmp/downloads/spec.pdf")],
            failed: Vec::new(),
        });
        assert_eq!(app.in_flight, 0);
        assert_eq!(
            press(&mut app, KeyCode::Char('o')),
            vec![Effect::OpenPath(PathBuf::from("/tmp/downloads/spec.pdf"))]
        );
    }

    #[test]
    fn file_selection_follows_yazi_keys() {
        let mut app = files_app();
        // Space toggles the hovered file, then moves down.
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(marked(&app), ["f4"]);
        assert_eq!(app.selected_index(Tab::Files), Some(1));
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(marked(&app), ["f4"]);

        // C-a selects every visible file and C-r inverts; C-r is not refresh.
        ctrl(&mut app, 'a');
        assert_eq!(marked(&app), ["f1", "f2", "f3", "f4"]);
        assert!(ctrl(&mut app, 'r').is_empty());
        assert!(marked(&app).is_empty());
        press(&mut app, KeyCode::Char(' ')); // f2
        ctrl(&mut app, 'r');
        assert_eq!(marked(&app), ["f1", "f3", "f4"]);
        press(&mut app, KeyCode::Esc);
        assert!(marked(&app).is_empty());

        // C-a only adds visible files, and Esc clears the search before the selection.
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "lab");
        press(&mut app, KeyCode::Enter);
        ctrl(&mut app, 'a');
        assert_eq!(marked(&app), ["f3", "f4"]);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.visible_files().len(), 4);
        assert_eq!(marked(&app), ["f3", "f4"]);
        press(&mut app, KeyCode::Esc);
        assert!(marked(&app).is_empty());

        // C-d still pages.
        app.list_height = 2;
        press(&mut app, KeyCode::Home);
        ctrl(&mut app, 'd');
        assert_eq!(app.selected_index(Tab::Files), Some(1));
        assert!(marked(&app).is_empty());

        // Other tabs only explain where selection works.
        press(&mut app, KeyCode::Char('1'));
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(
            app.status.as_ref().map(|s| s.text.as_str()),
            Some("Select files in the Files tab.")
        );
        assert!(marked(&app).is_empty());
    }

    #[test]
    fn visual_mode_selects_and_unselects_ranges() {
        let mut app = files_app();
        press(&mut app, KeyCode::Char('v'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        // The range shows as selected before it is applied.
        assert_eq!(marked(&app), ["f2", "f3", "f4"]);
        assert!(app.selected_files.is_empty());
        press(&mut app, KeyCode::Esc);
        assert!(app.visual.is_none());
        assert_eq!(marked(&app), ["f2", "f3", "f4"]);

        // V unselects from f2 up to f3; moving on reshapes the range.
        press(&mut app, KeyCode::Char('V'));
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(marked(&app), ["f4"]);
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(marked(&app), ["f3", "f4"]);
        press(&mut app, KeyCode::Esc);
        assert_eq!(marked(&app), ["f3", "f4"]);

        // Switching tabs applies a pending range.
        press(&mut app, KeyCode::Char('v'));
        press(&mut app, KeyCode::Char('1'));
        assert!(app.visual.is_none());
        assert_eq!(marked(&app), ["f1", "f3", "f4"]);

        // So does d, before it asks.
        press(&mut app, KeyCode::Char('3'));
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('v'));
        press(&mut app, KeyCode::Char('G'));
        press(&mut app, KeyCode::Char('d'));
        assert!(app.visual.is_none());
        assert_eq!(app.selected_files.len(), 4);
        assert!(matches!(app.overlay, Some(Overlay::Confirm { .. })));
    }

    #[test]
    fn batch_download_confirms_then_clears_selection() {
        let mut app = files_app();
        press(&mut app, KeyCode::Char(' ')); // f4
        press(&mut app, KeyCode::Char(' ')); // f3
        assert!(press(&mut app, KeyCode::Char('d')).is_empty());
        let Some(Overlay::Confirm { title, lines, .. }) = &app.overlay else {
            panic!("expected a confirmation, got {:?}", app.overlay);
        };
        assert_eq!(title, "Download 2 files?");
        assert_eq!(lines[0], "2 files from 1 course");
        assert_eq!(lines[1], "To /tmp/downloads, one folder per course");
        assert!(lines.contains(&"Networks | Lab 2".to_string()));

        // Declining keeps the selection.
        press(&mut app, KeyCode::Char('n'));
        assert!(app.overlay.is_none());
        assert_eq!(marked(&app), ["f3", "f4"]);

        press(&mut app, KeyCode::Char('d'));
        let effects = press(&mut app, KeyCode::Char('y'));
        let [Effect::DownloadFiles { files, dir }] = effects.as_slice() else {
            panic!("expected DownloadFiles, got {effects:?}");
        };
        let mut ids: Vec<&str> = files.iter().map(|f| f.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["f3", "f4"]);
        assert_eq!(dir, &PathBuf::from("/tmp/downloads"));
        assert_eq!(app.in_flight, 1);
        assert!(app.selected_files.is_empty());

        // Without a selection, d downloads the hovered file at once.
        let effects = press(&mut app, KeyCode::Char('d'));
        assert!(
            matches!(effects.as_slice(), [Effect::DownloadFile { file, .. }] if file.id == "f2"),
            "{effects:?}"
        );
    }

    #[test]
    fn download_results_report_partial_failures() {
        let mut app = files_app();
        let finish = |app: &mut App, saved: &[&str], failed: &[&str]| {
            app.in_flight += 1;
            app.on_msg(Msg::Downloaded {
                saved: saved.iter().map(PathBuf::from).collect(),
                failed: failed.iter().map(|s| s.to_string()).collect(),
            });
            let status = app.status.as_ref().unwrap();
            (status.kind, status.text.clone())
        };

        // Files from several courses: o opens the download folder.
        let status = finish(
            &mut app,
            &[
                "/tmp/downloads/Networks/a.pdf",
                "/tmp/downloads/Data Structures/b.pdf",
            ],
            &[],
        );
        assert_eq!(
            status,
            (
                StatusKind::Success,
                "Saved 2 files to /tmp/downloads · o to open the folder".into()
            )
        );
        assert_eq!(app.last_download, Some(PathBuf::from("/tmp/downloads")));

        // One course: its folder.
        finish(
            &mut app,
            &[
                "/tmp/downloads/Networks/a.pdf",
                "/tmp/downloads/Networks/b.pdf",
            ],
            &[],
        );
        assert_eq!(
            app.last_download,
            Some(PathBuf::from("/tmp/downloads/Networks"))
        );

        // A partial failure is an error that still counts what was saved.
        let status = finish(
            &mut app,
            &["/tmp/downloads/Networks/a.pdf"],
            &["Lab 2: HTTP 500"],
        );
        assert_eq!(
            status,
            (
                StatusKind::Error,
                "Saved 1 of 2 files · failed: Lab 2: HTTP 500".into()
            )
        );
        assert_eq!(
            app.last_download,
            Some(PathBuf::from("/tmp/downloads/Networks/a.pdf"))
        );

        let status = finish(&mut app, &[], &["Lab 2: HTTP 500"]);
        assert_eq!(
            status,
            (StatusKind::Error, "Download failed: Lab 2: HTTP 500".into())
        );
        assert_eq!(app.in_flight, 0);
    }

    #[test]
    fn semester_labels_name_the_term() {
        assert_eq!(semester_label("2025-2026-1"), "Fall 2025");
        assert_eq!(semester_label("2025-2026-2"), "Spring 2026");
        assert_eq!(semester_label("2025-2026-3"), "Summer 2026");
        assert_eq!(semester_label("odd"), "odd");
    }

    #[test]
    fn switching_semesters_reloads_an_empty_view() {
        let mut app = files_app();
        app.course_filter = Some("c2".into());
        ctrl(&mut app, 'a');
        // Not while lists are still loading.
        app.files.loading = true;
        press(&mut app, KeyCode::Char('S'));
        assert!(app.overlay.is_none());
        app.files.loading = false;

        press(&mut app, KeyCode::Char('S'));
        assert!(matches!(
            app.overlay,
            Some(Overlay::SemesterPicker { selected: 0 })
        ));
        press(&mut app, KeyCode::Char('j'));
        let effects = press(&mut app, KeyCode::Enter);
        let older = Some("2025-2026-1".to_string());
        assert_eq!(
            effects,
            vec![Effect::LoadAll {
                semester: older.clone()
            }]
        );
        assert_eq!(app.courses.semester, "2025-2026-1");
        assert!(app.courses.list.is_empty());
        assert!(app.homework.data.is_empty() && app.files.data.is_empty());
        assert!(app.courses_loading && app.homework.loading && !app.homework.loaded);
        assert!(app.course_filter.is_none() && app.selected_files.is_empty());

        // Once loaded, a refresh stays on the chosen semester.
        app.on_msg(Msg::Courses(Ok(CourseData {
            semester: "2025-2026-1".into(),
            current_semester: "2025-2026-2".into(),
            semesters: vec!["2025-2026-2".into(), "2025-2026-1".into()],
            list: vec![course("c9", "Old course")],
        })));
        app.on_msg(Msg::Homework(Ok(Vec::new())));
        app.on_msg(Msg::Announcements(Ok(Vec::new())));
        app.on_msg(Msg::Files {
            files: Vec::new(),
            failures: Vec::new(),
        });
        assert_eq!(
            press(&mut app, KeyCode::Char('r')),
            vec![Effect::LoadAll { semester: older }]
        );
    }

    #[test]
    fn refresh_drops_missing_selections() {
        let mut app = files_app();
        ctrl(&mut app, 'a');
        app.on_msg(Msg::Files {
            files: vec![course_file("f1", "c1", "L1 intro", "2026-03-01 10:00")],
            failures: Vec::new(),
        });
        assert_eq!(marked(&app), ["f1"]);
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("thu-learn-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(&dir).unwrap()
    }

    fn picker(app: &App) -> &Picker {
        match &app.overlay {
            Some(Overlay::Submit(s)) => &s.picker,
            Some(Overlay::Confirm { back: Some(s), .. }) => &s.picker,
            other => panic!("submit picker should be open, got {other:?}"),
        }
    }

    #[test]
    fn submit_picks_file_with_yazi_keys_then_confirms() {
        let dir = temp_dir("submit");
        std::fs::create_dir_all(dir.join("drafts")).unwrap();
        std::fs::write(dir.join("answer.pdf"), b"pdf").unwrap();

        let mut app = ready_app();
        app.submit_dir = dir.clone();
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(picker(&app).cwd, dir);

        // l enters a folder; h returns with the folder hovered.
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(picker(&app).cwd, dir.join("drafts"));
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(picker(&app).hovered().unwrap().name, "drafts");

        // j then Enter chooses the file and asks for a comment.
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        type_text(&mut app, "final version");
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.overlay, Some(Overlay::Confirm { .. })));

        // Declining returns to the picker in the same place.
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(picker(&app).hovered().unwrap().name, "answer.pdf");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Up); // recalls the previous comment
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
    fn ctrl_c_closes_the_picker_instead_of_quitting() {
        let mut app = ready_app();
        app.submit_dir = std::env::temp_dir();
        press(&mut app, KeyCode::Char('s'));
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.overlay.is_none());
        assert!(!app.should_quit);
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit);
    }

    #[test]
    fn pasting_a_file_path_chooses_it() {
        let dir = temp_dir("paste");
        std::fs::write(dir.join("my report.pdf"), b"pdf").unwrap();

        let mut app = ready_app();
        press(&mut app, KeyCode::Char('s'));
        app.on_paste(&format!("'{}'", dir.join("my report.pdf").display()));
        assert_eq!(picker(&app).cwd, dir);
        press(&mut app, KeyCode::Enter); // empty comment
        let Some(Overlay::Confirm { effect, .. }) = &app.overlay else {
            panic!("confirmation should open");
        };
        assert!(
            matches!(&**effect, Effect::Submit { file, .. } if file == &dir.join("my report.pdf"))
        );
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
        assert_eq!(
            app.on_msg(Msg::SessionReady),
            vec![Effect::LoadAll { semester: None }]
        );
        assert_eq!(app.session, Session::Ready);
    }

    #[test]
    fn list_motions_follow_yazi_keys() {
        let mut app = ready_app();
        app.homework_filter = HomeworkFilter::All;
        app.list_height = 2;
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(app.selected_index(Tab::Homework), Some(3));
        // A single `g` waits for the second key; `gg` goes to the top.
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.selected_index(Tab::Homework), Some(3));
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.selected_index(Tab::Homework), Some(0));
        // `g` followed by another key cancels without moving.
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected_index(Tab::Homework), Some(0));
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        app.on_key(ctrl('f')); // full page = 2 rows
        assert_eq!(app.selected_index(Tab::Homework), Some(2));
        app.on_key(ctrl('u')); // half page = 1 row
        assert_eq!(app.selected_index(Tab::Homework), Some(1));
    }

    #[test]
    fn human_size_formats_units() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }
}
