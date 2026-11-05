//! A single-line editor that behaves like yazi's input popups: it opens in
//! insert mode with Emacs-style keys, and `Esc` switches to a vi-like normal
//! mode (a second `Esc` cancels).
//!
//! The character classes and word-motion rules are ported from yazi
//! (https://github.com/sxyazi/yazi) at commit
//! b4f0d123952e52c4dafe874178de2b34c82f1ab2: `yazi-widgets/src/input/chars.rs`,
//! `gait.rs`, `actor/backward.rs`, `actor/forward.rs`, and the word boundary
//! scan in `actor/kill.rs`. Changes: cursors are char indices throughout (yazi's
//! kill sets a byte offset, which misplaces the cursor after CJK text).
//! Copyright (c) 2023 - sxyazi. Licensed under the MIT License; see the full
//! notice in `natsort.rs` in this directory.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthChar;

/// Maximum entries kept in the shared input history.
pub const HISTORY_MAX: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Insert,
    Normal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharKind {
    Space,
    Punct,
    Other,
}

/// How words are split: `Fine` at every class change, `Lean` only around runs
/// of letters and digits, `Wide` only between whitespace and non-whitespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gait {
    Fine,
    Lean,
    Wide,
}

impl CharKind {
    fn new(c: char) -> Self {
        if c.is_whitespace() {
            Self::Space
        } else if Self::is_punct(c) {
            Self::Punct
        } else {
            Self::Other
        }
    }

    fn vary(self, other: Self, gait: Gait) -> bool {
        match gait {
            Gait::Fine => self != other,
            Gait::Lean => self == Self::Other && other != Self::Other,
            Gait::Wide => (self == Self::Space) != (other == Self::Space),
        }
    }

    const fn is_punct(c: char) -> bool {
        match c {
            '_' => false,
            c if c.is_ascii_punctuation() => true,
            '！' | '，' | '；' | '？' => true,
            _ => false,
        }
    }
}

/// What a key did to the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// Nothing visible changed.
    None,
    /// Cursor or mode changed, text did not.
    Moved,
    /// The text changed.
    Changed,
    Submit(String),
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Snap {
    value: Vec<char>,
    cursor: usize,
}

#[derive(Debug, Clone)]
pub struct Editor {
    value: Vec<char>,
    cursor: usize,
    pub mode: Mode,
    /// Pending operator in normal mode: `d`, `c`, or `y`.
    op: Option<char>,
    snaps: Vec<Snap>,
    snap_idx: usize,
    /// Text cut or yanked by `d`/`c`/`y`/`x`, pasted by `p`/`P`.
    register: String,
    /// Index into the history while recalling, and the unsent draft.
    recall: Option<usize>,
    draft: String,
    /// First visible char when the value is wider than the field.
    scroll: usize,
}

impl Editor {
    pub fn new(value: &str) -> Self {
        let value: Vec<char> = value.chars().collect();
        let cursor = value.len();
        Self {
            snaps: vec![Snap {
                value: value.clone(),
                cursor,
            }],
            value,
            cursor,
            mode: Mode::Insert,
            op: None,
            snap_idx: 0,
            register: String::new(),
            recall: None,
            draft: String::new(),
            scroll: 0,
        }
    }

    pub fn value(&self) -> String {
        self.value.iter().collect()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Replaces the text, puts the cursor at `cursor`, and stays in the current mode.
    pub fn replace(&mut self, value: &str, cursor: usize) {
        self.value = value.chars().collect();
        self.cursor = cursor.min(self.value.len());
        self.recall = None;
    }

    pub fn insert_str(&mut self, text: &str) {
        for c in text.chars() {
            let c = if c == '\n' || c == '\r' { ' ' } else { c };
            if c.is_control() {
                continue;
            }
            self.value.insert(self.cursor, c);
            self.cursor += 1;
        }
        self.recall = None;
    }

    /// Text from the start up to the end of the path component at the cursor,
    /// which is what completion looks at.
    pub fn completion_word(&self) -> String {
        let end = self.value[self.cursor..]
            .iter()
            .position(|&c| c == '/')
            .map_or(self.value.len(), |i| self.cursor + i);
        self.value[..end].iter().collect()
    }

    fn max_cursor(&self) -> usize {
        match self.mode {
            Mode::Insert => self.value.len(),
            Mode::Normal => self.value.len().saturating_sub(1),
        }
    }

    fn clamp(&mut self) {
        self.cursor = self.cursor.min(self.max_cursor());
    }

    fn checkpoint(&mut self) {
        let snap = Snap {
            value: self.value.clone(),
            cursor: self.cursor,
        };
        if self.snaps.get(self.snap_idx) != Some(&snap) {
            self.snaps.truncate(self.snap_idx + 1);
            self.snaps.push(snap);
            self.snap_idx = self.snaps.len() - 1;
        }
    }

    fn restore(&mut self, idx: usize) {
        let snap = self.snaps[idx].clone();
        self.snap_idx = idx;
        self.value = snap.value;
        self.cursor = snap.cursor;
        self.mode = Mode::Normal;
        self.clamp();
    }

    /// Applies a key. `history` is oldest-first.
    pub fn handle_key(&mut self, key: KeyEvent, history: &[String]) -> Edit {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let before = (self.value.clone(), self.cursor, self.mode);

        // Keys that work in every mode.
        match key.code {
            KeyCode::Enter => return Edit::Submit(self.value()),
            KeyCode::Char('c') if ctrl => return Edit::Cancel,
            KeyCode::Esc => {
                return match (self.mode, self.op) {
                    (Mode::Insert, _) => {
                        self.mode = Mode::Normal;
                        self.cursor = self.cursor.saturating_sub(1);
                        self.clamp();
                        self.checkpoint();
                        Edit::Moved
                    }
                    (Mode::Normal, Some(_)) => {
                        self.op = None;
                        Edit::Moved
                    }
                    (Mode::Normal, None) => Edit::Cancel,
                };
            }
            // Word motions that skip punctuation: A-b/A-f and C-Left/C-Right.
            KeyCode::Left if ctrl || alt => self.motion(self.backward(Gait::Lean)),
            KeyCode::Char('b') if alt => self.motion(self.backward(Gait::Lean)),
            KeyCode::Right if ctrl || alt => self.motion(self.forward(Gait::Lean, true)),
            KeyCode::Char('f') if alt => self.motion(self.forward(Gait::Lean, true)),
            KeyCode::Left => self.motion(self.cursor.saturating_sub(1)),
            KeyCode::Char('b') if ctrl => self.motion(self.cursor.saturating_sub(1)),
            KeyCode::Right => self.motion(self.cursor + 1),
            KeyCode::Char('f') if ctrl => self.motion(self.cursor + 1),
            KeyCode::Home => self.motion(0),
            KeyCode::Char('a') if ctrl => self.motion(0),
            KeyCode::End => self.motion(self.value.len()),
            KeyCode::Char('e') if ctrl => self.motion(self.value.len()),
            KeyCode::Up => return self.recall(-1, history),
            KeyCode::Char('p') if ctrl => return self.recall(-1, history),
            KeyCode::Down => return self.recall(1, history),
            KeyCode::Char('n') if ctrl => return self.recall(1, history),
            _ if self.mode == Mode::Insert => self.insert_key(key),
            _ => self.normal_key(key, history),
        }
        let after = (&self.value, self.cursor, self.mode);
        if *after.0 != before.0 {
            self.recall = None;
            Edit::Changed
        } else if (after.1, after.2) != (before.1, before.2) {
            Edit::Moved
        } else {
            Edit::None
        }
    }

    fn insert_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('h') if ctrl => self.backspace(),
            KeyCode::Backspace if ctrl => self.kill_backward(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Char('d') if ctrl => self.delete_under(),
            KeyCode::Delete if ctrl => self.kill_forward(),
            KeyCode::Delete => self.delete_under(),
            KeyCode::Char('u') if ctrl => {
                self.value.drain(..self.cursor);
                self.cursor = 0;
            }
            KeyCode::Char('k') if ctrl => self.value.truncate(self.cursor),
            KeyCode::Char('w') if ctrl => self.kill_backward(),
            KeyCode::Char('d') if alt => self.kill_forward(),
            KeyCode::Char(c) if !ctrl && !alt => self.insert_str(&c.to_string()),
            _ => {}
        }
    }

    fn normal_key(&mut self, key: KeyEvent, history: &[String]) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let KeyCode::Char(c) = key.code else {
            return;
        };
        if ctrl {
            if c == 'r' && self.snap_idx + 1 < self.snaps.len() {
                self.restore(self.snap_idx + 1);
            }
            return;
        }
        // A pending operator takes a motion, or the same key again for the whole line.
        if let Some(op) = self.op {
            if c == op {
                self.op = None;
                self.apply_op(op, 0, self.value.len());
                return;
            }
            if let Some(target) = self.motion_target(c, true) {
                self.op = None;
                let (a, b) = (self.cursor.min(target), self.cursor.max(target));
                self.apply_op(op, a, b);
            } else {
                self.op = None;
            }
            return;
        }
        match c {
            'i' => self.mode = Mode::Insert,
            'I' => {
                self.cursor = self.first_char();
                self.mode = Mode::Insert;
            }
            'a' => {
                self.mode = Mode::Insert;
                self.cursor = (self.cursor + 1).min(self.value.len());
            }
            'A' => {
                self.mode = Mode::Insert;
                self.cursor = self.value.len();
            }
            'd' | 'c' | 'y' => self.op = Some(c),
            'x' => self.cut(self.cursor, (self.cursor + 1).min(self.value.len()), false),
            's' => self.cut(self.cursor, (self.cursor + 1).min(self.value.len()), true),
            'D' => self.cut(self.cursor, self.value.len(), false),
            'C' => self.cut(self.cursor, self.value.len(), true),
            'S' => self.cut(0, self.value.len(), true),
            'p' => self.paste(true),
            'P' => self.paste(false),
            'u' if self.snap_idx > 0 => self.restore(self.snap_idx - 1),
            'k' => {
                self.recall(-1, history);
            }
            'j' => {
                self.recall(1, history);
            }
            c => {
                if let Some(target) = self.motion_target(c, false) {
                    self.motion(target);
                }
            }
        }
    }

    /// Target cursor for a motion key in normal mode. With an operator pending,
    /// word ends and `$` are exclusive ends.
    fn motion_target(&self, c: char, operating: bool) -> Option<usize> {
        let target = match c {
            'h' => self.cursor.saturating_sub(1),
            'l' => self.cursor + 1,
            '0' => 0,
            '$' if operating => self.value.len(),
            '$' => self.value.len().saturating_sub(1),
            '^' | '_' => self.first_char(),
            'b' => self.backward(Gait::Fine),
            'B' => self.backward(Gait::Wide),
            'w' => self.forward_op(Gait::Fine, false, operating),
            'W' => self.forward_op(Gait::Wide, false, operating),
            'e' => self.forward_op(Gait::Fine, true, operating),
            'E' => self.forward_op(Gait::Wide, true, operating),
            _ => return None,
        };
        Some(target.min(self.value.len()))
    }

    fn motion(&mut self, target: usize) {
        self.cursor = target;
        self.clamp();
    }

    fn first_char(&self) -> usize {
        self.value
            .iter()
            .position(|c| !c.is_whitespace())
            .unwrap_or(0)
    }

    fn backward(&self, gait: Gait) -> usize {
        if self.cursor == 0 {
            return 0;
        }
        let mut it = self.value[..self.cursor].iter().rev().enumerate();
        let mut prev = CharKind::new(*it.next().expect("cursor > 0").1);
        for (i, &c) in it {
            let k = CharKind::new(c);
            if prev != CharKind::Space && prev.vary(k, gait) {
                return self.cursor - i;
            }
            prev = k;
        }
        0
    }

    fn forward(&self, gait: Gait, end_of_word: bool) -> usize {
        self.forward_op(gait, end_of_word, false)
    }

    fn forward_op(&self, gait: Gait, end_of_word: bool, operating: bool) -> usize {
        let mut it = self.value.iter().skip(self.cursor).enumerate();
        let Some((_, &first)) = it.next() else {
            return self.cursor;
        };
        let mut prev = CharKind::new(first);
        let delta = usize::from(self.mode == Mode::Normal);
        for (i, &c) in it {
            let k = CharKind::new(c);
            let boundary = if end_of_word {
                prev != CharKind::Space && prev.vary(k, gait) && i != 1
            } else {
                k != CharKind::Space && k.vary(prev, gait)
            };
            if boundary {
                let step = if end_of_word && !operating {
                    i - delta
                } else {
                    i
                };
                return self.cursor + step;
            }
            prev = k;
        }
        self.value.len()
    }

    /// Number of chars in one word run (after leading spaces), as yazi's kill.
    fn word_run(chars: impl Iterator<Item = char> + Clone) -> usize {
        let spaces = chars
            .clone()
            .take_while(|&c| CharKind::new(c) == CharKind::Space)
            .count();
        let mut rest = chars.skip(spaces);
        let run = match rest.next() {
            Some(c) => {
                let kind = CharKind::new(c);
                rest.take_while(|&c| CharKind::new(c) == kind).count() + 1
            }
            None => 0,
        };
        spaces + run
    }

    fn kill_backward(&mut self) {
        let n = Self::word_run(self.value[..self.cursor].iter().rev().copied());
        let start = self.cursor - n;
        self.value.drain(start..self.cursor);
        self.cursor = start;
    }

    fn kill_forward(&mut self) {
        let n = Self::word_run(self.value[self.cursor..].iter().copied());
        self.value.drain(self.cursor..self.cursor + n);
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.value.remove(self.cursor - 1);
            self.cursor -= 1;
        }
    }

    fn delete_under(&mut self) {
        if self.cursor < self.value.len() {
            self.value.remove(self.cursor);
        }
    }

    fn apply_op(&mut self, op: char, a: usize, b: usize) {
        match op {
            'y' => self.register = self.value[a..b].iter().collect(),
            'd' => self.cut(a, b, false),
            _ => self.cut(a, b, true),
        }
    }

    /// Cuts `[a, b)` into the register; `insert` switches to insert mode after.
    fn cut(&mut self, a: usize, b: usize, insert: bool) {
        if a < b {
            self.register = self.value.drain(a..b).collect();
        }
        self.cursor = a;
        if insert {
            self.mode = Mode::Insert;
        }
        self.clamp();
        self.checkpoint();
    }

    fn paste(&mut self, after: bool) {
        if self.register.is_empty() {
            return;
        }
        let at = if after && !self.value.is_empty() {
            (self.cursor + 1).min(self.value.len())
        } else {
            self.cursor
        };
        let text: Vec<char> = self.register.chars().collect();
        let n = text.len();
        self.value.splice(at..at, text);
        self.cursor = at + n - 1;
        self.clamp();
        self.checkpoint();
    }

    fn recall(&mut self, dir: i32, history: &[String]) -> Edit {
        if history.is_empty() {
            return Edit::None;
        }
        let next = match (self.recall, dir < 0) {
            (None, true) => {
                self.draft = self.value();
                Some(history.len() - 1)
            }
            (None, false) => return Edit::None,
            (Some(0), true) => return Edit::None,
            (Some(i), true) => Some(i - 1),
            (Some(i), false) if i + 1 >= history.len() => None,
            (Some(i), false) => Some(i + 1),
        };
        let text = match next {
            Some(i) => history[i].clone(),
            None => std::mem::take(&mut self.draft),
        };
        self.value = text.chars().collect();
        self.cursor = self.value.len();
        self.clamp();
        self.recall = next;
        Edit::Changed
    }

    /// Visible slice for a field `width` cells wide and the cursor column,
    /// scrolling horizontally with a margin of up to five characters.
    pub fn view(&mut self, width: usize) -> (String, usize) {
        let width = width.max(1);
        let w = |c: &char| c.width().unwrap_or(0);
        let margin = 5.min(width / 2);
        if self.cursor < self.scroll + margin {
            self.scroll = self.cursor.saturating_sub(margin);
        }
        loop {
            let upto: usize = self.value[self.scroll..self.cursor.min(self.value.len())]
                .iter()
                .map(w)
                .sum();
            let after: usize = self.value[self.cursor.min(self.value.len())..]
                .iter()
                .take(margin)
                .map(w)
                .sum();
            if self.scroll < self.cursor && upto + after.max(1) > width {
                self.scroll += 1;
            } else {
                break;
            }
        }
        let mut visible = String::new();
        let mut used = 0;
        for c in &self.value[self.scroll..] {
            if used + w(c) > width {
                break;
            }
            used += w(c);
            visible.push(*c);
        }
        let cursor_x = self.value[self.scroll..self.cursor.min(self.value.len())]
            .iter()
            .map(w)
            .sum();
        (visible, cursor_x)
    }
}

/// Adds a submitted value to the shared history: no empties, no duplicates,
/// newest last, at most [`HISTORY_MAX`] entries.
pub fn remember(history: &mut Vec<String>, value: &str) {
    if value.is_empty() {
        return;
    }
    history.retain(|h| h != value);
    history.push(value.to_string());
    if history.len() > HISTORY_MAX {
        history.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ch(c: char) -> KeyEvent {
        key(KeyCode::Char(c))
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn typed(e: &mut Editor, s: &str) {
        for c in s.chars() {
            e.handle_key(ch(c), &[]);
        }
    }

    #[test]
    fn opens_in_insert_mode_and_esc_twice_cancels() {
        let mut e = Editor::new("");
        typed(&mut e, "abc");
        assert_eq!(e.value(), "abc");
        assert_eq!(e.handle_key(key(KeyCode::Esc), &[]), Edit::Moved);
        assert_eq!(e.mode, Mode::Normal);
        assert_eq!(e.cursor(), 2);
        assert_eq!(e.handle_key(key(KeyCode::Esc), &[]), Edit::Cancel);
    }

    #[test]
    fn insert_mode_letters_are_text_not_commands() {
        let mut e = Editor::new("");
        typed(&mut e, "hjkl dw");
        assert_eq!(e.value(), "hjkl dw");
    }

    #[test]
    fn normal_mode_motions_and_operators() {
        let mut e = Editor::new("hw1_final.pdf report");
        e.handle_key(key(KeyCode::Esc), &[]);
        e.handle_key(ch('0'), &[]);
        e.handle_key(ch('w'), &[]);
        assert_eq!(e.cursor(), 9); // '_' is a word char, '.' is punctuation
        e.handle_key(ch('0'), &[]);
        e.handle_key(ch('e'), &[]);
        assert_eq!(e.cursor(), 8); // on the last char of "hw1_final"
        e.handle_key(ch('0'), &[]);
        e.handle_key(ch('d'), &[]);
        e.handle_key(ch('w'), &[]);
        assert_eq!(e.value(), ".pdf report");
        e.handle_key(ch('u'), &[]);
        assert_eq!(e.value(), "hw1_final.pdf report");
        e.handle_key(ctrl('r'), &[]);
        assert_eq!(e.value(), ".pdf report");
        e.handle_key(ch('d'), &[]);
        e.handle_key(ch('d'), &[]);
        assert_eq!(e.value(), "");
        e.handle_key(ch('P'), &[]);
        assert_eq!(e.value(), ".pdf report");
    }

    #[test]
    fn ctrl_w_keeps_char_cursor_after_cjk() {
        let mut e = Editor::new("");
        typed(&mut e, "作业 report");
        e.handle_key(ctrl('w'), &[]);
        assert_eq!(e.value(), "作业 ");
        assert_eq!(e.cursor(), 3);
        typed(&mut e, "x");
        assert_eq!(e.value(), "作业 x");
    }

    #[test]
    fn history_recall_restores_the_draft() {
        let mut history = Vec::new();
        remember(&mut history, "pdf");
        remember(&mut history, "report");
        remember(&mut history, "pdf");
        assert_eq!(history, ["report", "pdf"]);
        let mut e = Editor::new("dra");
        e.handle_key(key(KeyCode::Up), &history);
        assert_eq!(e.value(), "pdf");
        e.handle_key(key(KeyCode::Up), &history);
        assert_eq!(e.value(), "report");
        e.handle_key(key(KeyCode::Up), &history);
        assert_eq!(e.value(), "report");
        e.handle_key(key(KeyCode::Down), &history);
        e.handle_key(key(KeyCode::Down), &history);
        assert_eq!(e.value(), "dra");
    }

    #[test]
    fn view_scrolls_with_margin() {
        let mut e = Editor::new("abcdefghijklmnopqrstuvwxyz");
        let (visible, x) = e.view(10);
        assert_eq!(x, visible.chars().count().min(10));
        assert!(visible.ends_with('z'));
        e.handle_key(key(KeyCode::Home), &[]);
        let (visible, x) = e.view(10);
        assert_eq!((visible.as_str(), x), ("abcdefghij", 0));
    }
}
