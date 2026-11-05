//! A yazi-style file browser used to pick the file for a homework submission:
//! parent directory, current directory, and a preview of the selection.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::input::Input;

/// Bytes read from a file for its text preview.
const PREVIEW_BYTES: usize = 16 * 1024;
/// Entries listed when previewing a directory.
const PREVIEW_ENTRIES: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preview {
    Directory(Result<Vec<Entry>, String>),
    Text(Vec<String>),
    Binary,
    Unreadable(String),
}

/// Directories first, then files, each sorted case-insensitively.
pub fn list_dir(dir: &Path, show_hidden: bool) -> Result<Vec<Entry>, String> {
    let read = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    let mut entries: Vec<Entry> = read
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !show_hidden && name.starts_with('.') {
                return None;
            }
            let path = e.path();
            // Follow symlinks so linked folders behave like folders.
            let meta = std::fs::metadata(&path).ok();
            Some(Entry {
                name,
                is_dir: meta.as_ref().is_some_and(|m| m.is_dir()),
                size: meta.as_ref().map_or(0, |m| m.len()),
                modified: meta.and_then(|m| m.modified().ok()),
                path,
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

fn preview_file(path: &Path) -> Preview {
    let mut buf = Vec::with_capacity(PREVIEW_BYTES);
    let read =
        std::fs::File::open(path).and_then(|f| f.take(PREVIEW_BYTES as u64).read_to_end(&mut buf));
    if let Err(e) = read {
        return Preview::Unreadable(e.to_string());
    }
    if buf.contains(&0) {
        return Preview::Binary;
    }
    // A multi-byte character may be cut at the end of the buffer.
    let text = match std::str::from_utf8(&buf) {
        Ok(t) => t.to_string(),
        Err(e) if e.valid_up_to() + 4 >= buf.len() => {
            String::from_utf8_lossy(&buf[..e.valid_up_to()]).into_owned()
        }
        Err(_) => return Preview::Binary,
    };
    Preview::Text(text.lines().map(str::to_string).collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Browser,
    /// Typing a filter for the current directory.
    Filter,
    /// Typing a path to jump to.
    GoTo,
    Comment,
}

/// Submission picker state: browser position, chosen file, and comment.
#[derive(Debug, Clone)]
pub struct FilePicker {
    pub cwd: PathBuf,
    entries: Result<Vec<Entry>, String>,
    /// Index into [`Self::visible`].
    pub selected: usize,
    /// First visible row of the current column, persisted for stable scrolling.
    pub offset: usize,
    pub show_hidden: bool,
    pub filter: Input,
    pub goto: Input,
    pub goto_candidates: Vec<String>,
    pub focus: Focus,
    pub chosen: Option<PathBuf>,
    pub comment: Input,
    pub error: Option<String>,
    preview_cache: Option<(PathBuf, Preview)>,
    parent_cache: Option<(PathBuf, Vec<Entry>)>,
}

impl FilePicker {
    pub fn open(dir: &Path) -> Self {
        let mut picker = Self {
            cwd: dir.to_path_buf(),
            entries: Ok(Vec::new()),
            selected: 0,
            offset: 0,
            show_hidden: false,
            filter: Input::default(),
            goto: Input::default(),
            goto_candidates: Vec::new(),
            focus: Focus::Browser,
            chosen: None,
            comment: Input::default(),
            error: None,
            preview_cache: None,
            parent_cache: None,
        };
        let start = if dir.is_dir() {
            dir.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
        };
        picker.cd(&start, None);
        picker
    }

    pub fn listing_error(&self) -> Option<&str> {
        self.entries.as_ref().err().map(String::as_str)
    }

    /// Entries of the current directory that match the filter.
    pub fn visible(&self) -> Vec<&Entry> {
        let q = self.filter.value().to_lowercase();
        match &self.entries {
            Ok(entries) => entries
                .iter()
                .filter(|e| q.is_empty() || e.name.to_lowercase().contains(&q))
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn current(&self) -> Option<&Entry> {
        self.visible().get(self.selected).copied()
    }

    /// Changes directory and selects `select` when it is listed.
    fn cd(&mut self, dir: &Path, select: Option<&Path>) {
        let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        self.entries = list_dir(&dir, self.show_hidden);
        self.cwd = dir;
        self.filter.clear();
        self.offset = 0;
        self.selected = select
            .and_then(|target| self.visible().iter().position(|e| e.path == target))
            .unwrap_or(0);
    }

    fn reload(&mut self) {
        let keep = self.current().map(|e| e.path.clone());
        let filter = self.filter.value().to_string();
        let cwd = self.cwd.clone();
        self.cd(&cwd, keep.as_deref());
        self.filter.set(filter);
        self.clamp();
        self.parent_cache = None;
        self.preview_cache = None;
    }

    fn clamp(&mut self) {
        let n = self.visible().len();
        self.selected = self.selected.min(n.saturating_sub(1));
    }

    pub fn move_by(&mut self, delta: isize) {
        let n = self.visible().len();
        if n == 0 {
            return;
        }
        let next = (self.selected as isize).saturating_add(delta);
        self.selected = next.clamp(0, n as isize - 1) as usize;
    }

    pub fn parent(&mut self) {
        if let Some(parent) = self.cwd.parent().map(Path::to_path_buf) {
            let from = self.cwd.clone();
            self.cd(&parent, Some(&from));
        }
    }

    /// Enters the selected directory, or returns the selected file.
    pub fn open_selected(&mut self) -> Option<PathBuf> {
        let entry = self.current()?.clone();
        if entry.is_dir {
            self.cd(&entry.path, None);
            None
        } else {
            Some(entry.path)
        }
    }

    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.reload();
    }

    pub fn home(&mut self) {
        if let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()) {
            self.cd(&home, None);
        }
    }

    /// Jumps to a typed or pasted path: a folder opens, a file is selected in its folder.
    pub fn go_to(&mut self, path: &Path) -> Result<(), String> {
        if path.is_dir() {
            self.cd(path, None);
            Ok(())
        } else if path.is_file() {
            let parent = path.parent().unwrap_or(Path::new("/")).to_path_buf();
            let file = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            if !self.show_hidden
                && file
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
            {
                self.show_hidden = true;
            }
            self.cd(&parent, Some(&file));
            Ok(())
        } else {
            Err(format!("No such file or folder: {}", path.display()))
        }
    }

    pub fn filter_changed(&mut self) {
        self.selected = 0;
        self.offset = 0;
    }

    /// Entries of the parent directory, cached per directory.
    pub fn parent_entries(&mut self) -> Option<&[Entry]> {
        let parent = self.cwd.parent()?.to_path_buf();
        let stale = self.parent_cache.as_ref().is_none_or(|(p, _)| p != &parent);
        if stale {
            let entries = list_dir(&parent, self.show_hidden).unwrap_or_default();
            self.parent_cache = Some((parent, entries));
        }
        self.parent_cache.as_ref().map(|(_, e)| e.as_slice())
    }

    /// Preview of the selected entry, cached until the selection changes.
    pub fn preview(&mut self) -> Option<&Preview> {
        let entry = self.current()?.clone();
        let stale = self
            .preview_cache
            .as_ref()
            .is_none_or(|(p, _)| p != &entry.path);
        if stale {
            let preview = if entry.is_dir {
                Preview::Directory(list_dir(&entry.path, self.show_hidden).map(|mut v| {
                    v.truncate(PREVIEW_ENTRIES);
                    v
                }))
            } else {
                preview_file(&entry.path)
            };
            self.preview_cache = Some((entry.path, preview));
        }
        self.preview_cache.as_ref().map(|(_, p)| p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "thu-learn-picker-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("Slides")).unwrap();
        std::fs::create_dir_all(dir.join("assignments/hw1")).unwrap();
        std::fs::write(dir.join("b-report.pdf"), b"%PDF\0binary").unwrap();
        std::fs::write(dir.join("a-notes.txt"), "line one\nline two\n").unwrap();
        std::fs::write(dir.join(".hidden"), b"x").unwrap();
        std::fs::write(dir.join("assignments/hw1/answer.md"), "# Answer").unwrap();
        std::fs::canonicalize(&dir).unwrap()
    }

    fn names(p: &FilePicker) -> Vec<String> {
        p.visible().iter().map(|e| e.name.clone()).collect()
    }

    #[test]
    fn lists_directories_first_and_hides_dotfiles() {
        let dir = fixture("list");
        let mut p = FilePicker::open(&dir);
        assert_eq!(
            names(&p),
            vec!["assignments", "Slides", "a-notes.txt", "b-report.pdf"]
        );
        p.toggle_hidden();
        assert!(names(&p).contains(&".hidden".to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn enters_folders_and_returns_to_parent_with_selection() {
        let dir = fixture("nav");
        let mut p = FilePicker::open(&dir);
        assert_eq!(p.open_selected(), None); // enter "assignments"
        assert_eq!(p.cwd, dir.join("assignments"));
        assert_eq!(p.open_selected(), None); // enter "hw1"
        assert_eq!(
            p.open_selected(),
            Some(dir.join("assignments/hw1/answer.md"))
        );
        p.parent();
        p.parent();
        assert_eq!(p.cwd, dir);
        assert_eq!(p.current().unwrap().name, "assignments");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn filter_narrows_current_folder() {
        let dir = fixture("filter");
        let mut p = FilePicker::open(&dir);
        p.filter.set("REP");
        p.filter_changed();
        assert_eq!(names(&p), vec!["b-report.pdf"]);
        assert_eq!(p.open_selected(), Some(dir.join("b-report.pdf")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn go_to_selects_file_in_its_folder() {
        let dir = fixture("goto");
        let mut p = FilePicker::open(&dir.join("Slides"));
        p.go_to(&dir.join("assignments/hw1/answer.md")).unwrap();
        assert_eq!(p.cwd, dir.join("assignments/hw1"));
        assert_eq!(p.current().unwrap().name, "answer.md");
        assert!(p.go_to(&dir.join("missing")).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn previews_text_binary_and_folders() {
        let dir = fixture("preview");
        let mut p = FilePicker::open(&dir);
        assert!(matches!(p.preview(), Some(Preview::Directory(Ok(v))) if v[0].name == "hw1"));
        p.move_by(2);
        assert_eq!(
            p.preview(),
            Some(&Preview::Text(vec!["line one".into(), "line two".into()]))
        );
        p.move_by(1);
        assert_eq!(p.preview(), Some(&Preview::Binary));
        std::fs::remove_dir_all(&dir).ok();
    }
}
