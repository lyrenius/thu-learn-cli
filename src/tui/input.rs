//! Single-line text input and filesystem path helpers for TUI forms.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};
use unicode_width::UnicodeWidthChar;

/// An editable single-line text field. `cursor` counts chars, not bytes.
#[derive(Debug, Clone, Default)]
pub struct Input {
    value: String,
    cursor: usize,
}

impl Input {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.chars().count();
        Self { value, cursor }
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    pub fn set(&mut self, value: impl Into<String>) {
        *self = Self::new(value);
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    fn byte_index(&self, char_index: usize) -> usize {
        self.value
            .char_indices()
            .nth(char_index)
            .map(|(i, _)| i)
            .unwrap_or(self.value.len())
    }

    pub fn insert_str(&mut self, text: &str) {
        for c in text.chars().filter(|c| !c.is_control()) {
            let at = self.byte_index(self.cursor);
            self.value.insert(at, c);
            self.cursor += 1;
        }
    }

    /// Applies an editing key. Returns `false` for keys the input does not handle.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let len = self.value.chars().count();
        match key.code {
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = len,
            KeyCode::Char('u') if ctrl => {
                let at = self.byte_index(self.cursor);
                self.value.replace_range(..at, "");
                self.cursor = 0;
            }
            KeyCode::Char('k') if ctrl => {
                let at = self.byte_index(self.cursor);
                self.value.truncate(at);
            }
            KeyCode::Char('w') if ctrl => {
                let chars: Vec<char> = self.value.chars().collect();
                let mut start = self.cursor;
                while start > 0 && chars[start - 1].is_whitespace() {
                    start -= 1;
                }
                while start > 0 && !chars[start - 1].is_whitespace() && chars[start - 1] != '/' {
                    start -= 1;
                }
                if start == self.cursor && start > 0 {
                    start -= 1;
                }
                let (from, to) = (self.byte_index(start), self.byte_index(self.cursor));
                self.value.replace_range(from..to, "");
                self.cursor = start;
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.insert_str(&c.to_string());
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let (from, to) = (
                    self.byte_index(self.cursor - 1),
                    self.byte_index(self.cursor),
                );
                self.value.replace_range(from..to, "");
                self.cursor -= 1;
            }
            KeyCode::Backspace => {}
            KeyCode::Delete if self.cursor < len => {
                let (from, to) = (
                    self.byte_index(self.cursor),
                    self.byte_index(self.cursor + 1),
                );
                self.value.replace_range(from..to, "");
            }
            KeyCode::Delete => {}
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(len),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = len,
            _ => return false,
        }
        true
    }

    /// Returns the visible slice for a field `width` cells wide and the cursor column in it.
    /// The view scrolls horizontally so the cursor always stays visible.
    pub fn view(&self, width: usize) -> (String, usize) {
        let chars: Vec<char> = self.value.chars().collect();
        let w = |c: &char| c.width().unwrap_or(0);
        let width = width.max(1);
        let mut start = 0;
        while start < self.cursor
            && chars[start..self.cursor].iter().map(w).sum::<usize>() > width - 1
        {
            start += 1;
        }
        let mut visible = String::new();
        let mut used = 0;
        for c in &chars[start..] {
            if used + w(c) > width {
                break;
            }
            used += w(c);
            visible.push(*c);
        }
        let cursor_x = chars[start..self.cursor].iter().map(w).sum();
        (visible, cursor_x)
    }
}

/// Expands `~` and resolves a typed path against the current directory.
pub fn expand_path(raw: &str) -> PathBuf {
    let raw = raw.trim();
    let home = || directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf());
    if raw == "~" {
        if let Some(h) = home() {
            return h;
        }
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(h) = home() {
            return h.join(rest);
        }
    }
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir().map(|d| d.join(&p)).unwrap_or(p)
    }
}

/// Normalizes a path pasted or dragged into the terminal: trims, removes one
/// pair of surrounding quotes, and unescapes shell-style `\ ` sequences.
pub fn normalize_pasted_path(raw: &str) -> String {
    let t = raw.trim();
    let unquoted = ['\'', '"']
        .iter()
        .find_map(|q| t.strip_prefix(*q).and_then(|r| r.strip_suffix(*q)));
    if let Some(inner) = unquoted {
        return inner.to_string();
    }
    if let Some(rest) = t.strip_prefix("file://") {
        return rest.to_string();
    }
    let mut out = String::with_capacity(t.len());
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek() {
                if " '\"()&;[]".contains(next) {
                    out.push(next);
                    chars.next();
                    continue;
                }
            }
        }
        out.push(c);
    }
    out
}

/// Result of a Tab completion request on a path field.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PathCompletion {
    /// Replacement text for the field when completion made progress.
    pub value: Option<String>,
    /// Matching entry names when more than one entry matches.
    pub candidates: Vec<String>,
}

/// Completes the last path component of `typed`, like a shell does on Tab.
pub fn complete_path(typed: &str) -> PathCompletion {
    let (dir_part, prefix) = match typed.rfind('/') {
        Some(i) => (&typed[..=i], &typed[i + 1..]),
        None => ("", typed),
    };
    let dir = if dir_part.is_empty() {
        expand_path(".")
    } else {
        expand_path(dir_part)
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return PathCompletion::default();
    };
    let mut matches: Vec<(String, bool)> = entries
        .filter_map(|e| e.ok())
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (name, e.path().is_dir())
        })
        .filter(|(name, _)| name.starts_with(prefix))
        .filter(|(name, _)| prefix.starts_with('.') || !name.starts_with('.'))
        .collect();
    matches.sort();

    match matches.as_slice() {
        [] => PathCompletion::default(),
        [(name, is_dir)] => {
            let slash = if *is_dir { "/" } else { "" };
            PathCompletion {
                value: Some(format!("{dir_part}{name}{slash}")),
                candidates: Vec::new(),
            }
        }
        many => {
            let common = common_prefix(many.iter().map(|(n, _)| n.as_str()));
            let value = (common.chars().count() > prefix.chars().count())
                .then(|| format!("{dir_part}{common}"));
            PathCompletion {
                value,
                candidates: many
                    .iter()
                    .map(|(n, d)| if *d { format!("{n}/") } else { n.clone() })
                    .collect(),
            }
        }
    }
}

fn common_prefix<'a>(mut names: impl Iterator<Item = &'a str>) -> String {
    let Some(first) = names.next() else {
        return String::new();
    };
    let mut prefix: Vec<char> = first.chars().collect();
    for name in names {
        let n = prefix
            .iter()
            .zip(name.chars())
            .take_while(|(a, b)| **a == *b)
            .count();
        prefix.truncate(n);
    }
    prefix.into_iter().collect()
}

/// Replaces characters that are unsafe in file names on common platforms.
pub fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim_matches('.').trim().to_string();
    if cleaned.is_empty() {
        "download".to_string()
    } else {
        cleaned
    }
}

/// Creates an empty `dir/name`, or `dir/name (n).ext` when that path exists,
/// and returns its path. Creation is atomic, so concurrent downloads with the
/// same name never claim, and then overwrite, the same file.
pub fn claim_unique_path(dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let candidates = std::iter::once(dir.join(name))
        .chain((1..).map(|n| dir.join(format!("{stem} ({n}){ext}"))));
    for path in candidates {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!("an unused file name always exists")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "thu-learn-tui-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn input_edits_multibyte_text_at_cursor() {
        let mut input = Input::new("作业.pdf");
        input.handle_key(key(KeyCode::Home));
        input.handle_key(key(KeyCode::Right));
        input.handle_key(key(KeyCode::Right));
        input.handle_key(key(KeyCode::Char('2')));
        assert_eq!(input.value(), "作业2.pdf");
        input.handle_key(key(KeyCode::Backspace));
        input.handle_key(key(KeyCode::Backspace));
        assert_eq!(input.value(), "作.pdf");
        input.handle_key(key(KeyCode::Delete));
        assert_eq!(input.value(), "作pdf");
    }

    #[test]
    fn input_ctrl_shortcuts_delete_words_and_lines() {
        let mut input = Input::new("~/hw/report final.pdf");
        input.handle_key(ctrl('w'));
        assert_eq!(input.value(), "~/hw/report ");
        input.handle_key(ctrl('w'));
        assert_eq!(input.value(), "~/hw/");
        input.handle_key(ctrl('u'));
        assert_eq!(input.value(), "");
    }

    #[test]
    fn input_view_keeps_cursor_visible() {
        let input = Input::new("abcdefghij");
        let (visible, cursor) = input.view(5);
        assert_eq!(visible, "ghij");
        assert_eq!(cursor, 4);

        let mut input = Input::new("数学分析作业");
        input.handle_key(key(KeyCode::Home));
        let (visible, cursor) = input.view(6);
        assert_eq!(visible, "数学分");
        assert_eq!(cursor, 0);
    }

    #[test]
    fn pasted_paths_are_unquoted_and_unescaped() {
        assert_eq!(normalize_pasted_path("'/tmp/a b.pdf' "), "/tmp/a b.pdf");
        assert_eq!(normalize_pasted_path("\"/tmp/x.pdf\""), "/tmp/x.pdf");
        assert_eq!(
            normalize_pasted_path("/tmp/My\\ Homework\\ \\(1\\).pdf"),
            "/tmp/My Homework (1).pdf"
        );
        assert_eq!(normalize_pasted_path("file:///tmp/x.pdf"), "/tmp/x.pdf");
    }

    #[test]
    fn path_completion_completes_unique_and_common_prefixes() {
        let dir = temp_dir("complete");
        std::fs::write(dir.join("report-final.pdf"), b"x").unwrap();
        std::fs::write(dir.join("report-draft.pdf"), b"x").unwrap();
        std::fs::create_dir(dir.join("slides")).unwrap();
        let base = format!("{}/", dir.display());

        let unique = complete_path(&format!("{base}sl"));
        assert_eq!(unique.value, Some(format!("{base}slides/")));

        let partial = complete_path(&format!("{base}rep"));
        assert_eq!(partial.value, Some(format!("{base}report-")));
        assert_eq!(
            partial.candidates,
            vec![
                "report-draft.pdf".to_string(),
                "report-final.pdf".to_string()
            ]
        );

        assert_eq!(
            complete_path(&format!("{base}zzz")),
            PathCompletion::default()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn claim_unique_path_never_reuses_a_name() {
        let dir = temp_dir("unique");
        std::fs::write(dir.join("hw.pdf"), b"x").unwrap();
        let claim = |name| claim_unique_path(&dir, name).unwrap();
        assert_eq!(claim("hw.pdf"), dir.join("hw (1).pdf"));
        // The first claim created the file, so a second one moves on.
        assert_eq!(claim("hw.pdf"), dir.join("hw (2).pdf"));
        assert_eq!(std::fs::read(dir.join("hw.pdf")).unwrap(), b"x");
        assert_eq!(claim("new.pdf"), dir.join("new.pdf"));
        assert_eq!(claim("README"), dir.join("README"));
        assert!(claim_unique_path(&dir.join("missing"), "a.pdf").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn safe_file_name_replaces_separators() {
        assert_eq!(safe_file_name("a/b\\c:d.pdf"), "a_b_c_d.pdf");
        assert_eq!(safe_file_name("  ..  "), "download");
    }
}
