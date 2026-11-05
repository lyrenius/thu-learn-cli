//! The picker's key bindings: yazi's default `[mgr]` keymap
//! (`yazi-config/preset/keymap-default.toml`), minus actions that change files,
//! run commands, or need tabs, plugins, or external tools. Those keys still
//! answer with a notice instead of silently doing nothing.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::fs::SortBy;
use super::scroll::Step;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Key {
    const fn new(code: KeyCode) -> Self {
        Self {
            code,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    const fn ch(c: char) -> Self {
        Self::new(KeyCode::Char(c))
    }

    const fn ctrl(c: char) -> Self {
        Self {
            ctrl: true,
            ..Self::ch(c)
        }
    }

    const fn shift(code: KeyCode) -> Self {
        Self {
            shift: true,
            ..Self::new(code)
        }
    }

    /// Normalizes a terminal event. Shift is part of the character for letters
    /// and symbols, so it only counts for non-character keys.
    pub fn from_event(e: KeyEvent) -> Self {
        let shift =
            e.modifiers.contains(KeyModifiers::SHIFT) && !matches!(e.code, KeyCode::Char(_));
        Self {
            code: e.code,
            ctrl: e.modifiers.contains(KeyModifiers::CONTROL),
            alt: e.modifiers.contains(KeyModifiers::ALT),
            shift,
        }
    }

    /// yazi's notation: `g`, `<Space>`, `<C-a>`, `<S-PageUp>`.
    pub fn label(&self) -> String {
        let base = match self.code {
            KeyCode::Char(' ') => "Space".to_string(),
            KeyCode::Char(c) if !self.ctrl && !self.alt && !self.shift => return c.to_string(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Esc => "Esc".into(),
            KeyCode::Enter => "Enter".into(),
            KeyCode::Tab => "Tab".into(),
            KeyCode::BackTab => "BackTab".into(),
            KeyCode::Up => "Up".into(),
            KeyCode::Down => "Down".into(),
            KeyCode::Left => "Left".into(),
            KeyCode::Right => "Right".into(),
            KeyCode::Home => "Home".into(),
            KeyCode::End => "End".into(),
            KeyCode::PageUp => "PageUp".into(),
            KeyCode::PageDown => "PageDown".into(),
            KeyCode::F(n) => format!("F{n}"),
            other => format!("{other:?}"),
        };
        let mut prefix = String::new();
        if self.ctrl {
            prefix.push_str("C-");
        }
        if self.alt {
            prefix.push_str("A-");
        }
        if self.shift {
            prefix.push_str("S-");
        }
        format!("<{prefix}{base}>")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Linemode {
    None,
    Size,
    Permissions,
    Btime,
    Mtime,
    Owner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyWhat {
    Path,
    DirPath,
    Filename,
    NameWithoutExt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Goto {
    Home,
    Config,
    Downloads,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Escape,
    Quit,
    Arrow(Step),
    Leave,
    Enter,
    Back,
    Forward,
    /// Space: toggle the hovered entry, then move down one.
    Toggle,
    SelectAll,
    InvertSelection,
    Seek(i32),
    Spot,
    /// Enter/o: choose the selection, or the hovered file, for submission.
    Open,
    Hidden,
    Linemode(Linemode),
    Copy(CopyWhat),
    Filter,
    Find {
        previous: bool,
    },
    FindArrow {
        previous: bool,
    },
    Sort(SortBy, bool),
    Goto(Goto),
    CdInteractive,
    Follow,
    Help,
    /// A yazi key that has no meaning while choosing a file to submit.
    Unavailable,
}

#[derive(Debug, PartialEq)]
pub struct Binding {
    pub on: &'static [Key],
    pub action: Action,
    pub desc: &'static str,
}

use Action as A;
use KeyCode as K;

macro_rules! b {
    ([$($k:expr),+], $action:expr, $desc:expr) => {
        Binding { on: &[$($k),+], action: $action, desc: $desc }
    };
}

const fn k(code: KeyCode) -> Key {
    Key::new(code)
}

const fn c(ch: char) -> Key {
    Key::ch(ch)
}

/// Bindings in yazi's keymap order. Help lists the available ones.
pub static BINDINGS: &[Binding] = &[
    b!(
        [k(K::Esc)],
        A::Escape,
        "Clear the find, then the filter, then the selection"
    ),
    b!(
        [Key::ctrl('[')],
        A::Escape,
        "Clear the find, then the filter, then the selection"
    ),
    b!([c('q')], A::Quit, "Cancel the submission"),
    b!([c('Q')], A::Quit, "Cancel the submission"),
    b!([Key::ctrl('c')], A::Quit, "Cancel the submission"),
    b!([c('k')], A::Arrow(Step::Prev), "Previous file"),
    b!([c('j')], A::Arrow(Step::Next), "Next file"),
    b!([k(K::Up)], A::Arrow(Step::Prev), "Previous file"),
    b!([k(K::Down)], A::Arrow(Step::Next), "Next file"),
    b!([c('g'), c('g')], A::Arrow(Step::Top), "Go to top"),
    b!([c('G')], A::Arrow(Step::Bot), "Go to bottom"),
    b!([k(K::Home)], A::Arrow(Step::Top), "Go to top"),
    b!([k(K::End)], A::Arrow(Step::Bot), "Go to bottom"),
    b!(
        [Key::ctrl('u')],
        A::Arrow(Step::Percent(-50)),
        "Move cursor up half page"
    ),
    b!(
        [Key::ctrl('d')],
        A::Arrow(Step::Percent(50)),
        "Move cursor down half page"
    ),
    b!(
        [Key::ctrl('b')],
        A::Arrow(Step::Percent(-100)),
        "Move cursor up one page"
    ),
    b!(
        [Key::ctrl('f')],
        A::Arrow(Step::Percent(100)),
        "Move cursor down one page"
    ),
    b!(
        [Key::shift(K::PageUp)],
        A::Arrow(Step::Percent(-50)),
        "Move cursor up half page"
    ),
    b!(
        [Key::shift(K::PageDown)],
        A::Arrow(Step::Percent(50)),
        "Move cursor down half page"
    ),
    b!(
        [k(K::PageUp)],
        A::Arrow(Step::Percent(-100)),
        "Move cursor up one page"
    ),
    b!(
        [k(K::PageDown)],
        A::Arrow(Step::Percent(100)),
        "Move cursor down one page"
    ),
    b!([c('h')], A::Leave, "Back to the parent directory"),
    b!([c('l')], A::Enter, "Enter the child directory"),
    b!([k(K::Left)], A::Leave, "Back to the parent directory"),
    b!([k(K::Right)], A::Enter, "Enter the child directory"),
    b!([c('H')], A::Back, "Back to previous directory"),
    b!([c('L')], A::Forward, "Forward to next directory"),
    b!([c(' ')], A::Toggle, "Toggle the current selection state"),
    b!([Key::ctrl('a')], A::SelectAll, "Select all files"),
    b!(
        [Key::ctrl('r')],
        A::InvertSelection,
        "Invert selection of all files"
    ),
    b!([c('K')], A::Seek(-5), "Seek up 5 units in the preview"),
    b!([c('J')], A::Seek(5), "Seek down 5 units in the preview"),
    b!([k(K::Tab)], A::Spot, "Spot hovered file"),
    b!([c('o')], A::Open, "Choose the selected or hovered file"),
    b!([c('O')], A::Open, "Choose the selected or hovered file"),
    b!(
        [k(K::Enter)],
        A::Open,
        "Choose the selected or hovered file"
    ),
    b!(
        [Key::shift(K::Enter)],
        A::Open,
        "Choose the selected or hovered file"
    ),
    b!([c('.')], A::Hidden, "Toggle the visibility of hidden files"),
    b!(
        [c('m'), c('s')],
        A::Linemode(Linemode::Size),
        "Linemode: size"
    ),
    b!(
        [c('m'), c('p')],
        A::Linemode(Linemode::Permissions),
        "Linemode: permissions"
    ),
    b!(
        [c('m'), c('b')],
        A::Linemode(Linemode::Btime),
        "Linemode: btime"
    ),
    b!(
        [c('m'), c('m')],
        A::Linemode(Linemode::Mtime),
        "Linemode: mtime"
    ),
    b!(
        [c('m'), c('o')],
        A::Linemode(Linemode::Owner),
        "Linemode: owner"
    ),
    b!(
        [c('m'), c('n')],
        A::Linemode(Linemode::None),
        "Linemode: none"
    ),
    b!([c('c'), c('c')], A::Copy(CopyWhat::Path), "Copy file path"),
    b!([c('c'), c('C')], A::Copy(CopyWhat::Path), "Copy file URL"),
    b!(
        [c('c'), c('d')],
        A::Copy(CopyWhat::DirPath),
        "Copy directory path"
    ),
    b!(
        [c('c'), c('D')],
        A::Copy(CopyWhat::DirPath),
        "Copy directory URL"
    ),
    b!(
        [c('c'), c('f')],
        A::Copy(CopyWhat::Filename),
        "Copy filename"
    ),
    b!(
        [c('c'), c('n')],
        A::Copy(CopyWhat::NameWithoutExt),
        "Copy filename without extension"
    ),
    b!([c('f')], A::Filter, "Filter files"),
    b!([c('/')], A::Find { previous: false }, "Find next file"),
    b!([c('?')], A::Find { previous: true }, "Find previous file"),
    b!([c('n')], A::FindArrow { previous: false }, "Next found"),
    b!([c('N')], A::FindArrow { previous: true }, "Previous found"),
    b!(
        [c(','), c('m')],
        A::Sort(SortBy::Mtime, false),
        "Sort by modified time"
    ),
    b!(
        [c(','), c('M')],
        A::Sort(SortBy::Mtime, true),
        "Sort by modified time (reverse)"
    ),
    b!(
        [c(','), c('b')],
        A::Sort(SortBy::Btime, false),
        "Sort by birth time"
    ),
    b!(
        [c(','), c('B')],
        A::Sort(SortBy::Btime, true),
        "Sort by birth time (reverse)"
    ),
    b!(
        [c(','), c('e')],
        A::Sort(SortBy::Extension, false),
        "Sort by extension"
    ),
    b!(
        [c(','), c('E')],
        A::Sort(SortBy::Extension, true),
        "Sort by extension (reverse)"
    ),
    b!(
        [c(','), c('a')],
        A::Sort(SortBy::Alphabetical, false),
        "Sort alphabetically"
    ),
    b!(
        [c(','), c('A')],
        A::Sort(SortBy::Alphabetical, true),
        "Sort alphabetically (reverse)"
    ),
    b!(
        [c(','), c('n')],
        A::Sort(SortBy::Natural, false),
        "Sort naturally"
    ),
    b!(
        [c(','), c('N')],
        A::Sort(SortBy::Natural, true),
        "Sort naturally (reverse)"
    ),
    b!(
        [c(','), c('s')],
        A::Sort(SortBy::Size, false),
        "Sort by size"
    ),
    b!(
        [c(','), c('S')],
        A::Sort(SortBy::Size, true),
        "Sort by size (reverse)"
    ),
    b!(
        [c(','), c('r')],
        A::Sort(SortBy::Random, false),
        "Sort randomly"
    ),
    b!([c('g'), c('h')], A::Goto(Goto::Home), "Go home"),
    b!([c('g'), c('c')], A::Goto(Goto::Config), "Go to ~/.config"),
    b!(
        [c('g'), c('d')],
        A::Goto(Goto::Downloads),
        "Go to ~/Downloads"
    ),
    b!([c('g'), c(' ')], A::CdInteractive, "Jump interactively"),
    b!([c('g'), c('f')], A::Follow, "Follow hovered symlink"),
    b!([c('~')], A::Help, "Open help"),
    b!([k(K::F(1))], A::Help, "Open help"),
    // yazi keys that change files, run commands, or need tabs or plugins.
    b!([c('v')], A::Unavailable, "Visual mode"),
    b!([c('V')], A::Unavailable, "Visual mode"),
    b!([c('y')], A::Unavailable, "Yank"),
    b!([c('x')], A::Unavailable, "Cut"),
    b!([c('p')], A::Unavailable, "Paste"),
    b!([c('P')], A::Unavailable, "Paste"),
    b!([c('-')], A::Unavailable, "Symlink"),
    b!([c('_')], A::Unavailable, "Symlink"),
    b!([c('Y')], A::Unavailable, "Unyank"),
    b!([c('X')], A::Unavailable, "Unyank"),
    b!([c('d')], A::Unavailable, "Trash"),
    b!([c('D')], A::Unavailable, "Delete"),
    b!([c('a')], A::Unavailable, "Create"),
    b!([c('A')], A::Unavailable, "Bulk create"),
    b!([c('r')], A::Unavailable, "Rename"),
    b!([c(';')], A::Unavailable, "Shell commands"),
    b!([c(':')], A::Unavailable, "Shell commands"),
    b!([c('s')], A::Unavailable, "Search with fd"),
    b!([c('S')], A::Unavailable, "Search with ripgrep"),
    b!([Key::ctrl('s')], A::Escape, "Cancel the ongoing search"),
    b!([c('z')], A::Unavailable, "fzf"),
    b!([c('Z')], A::Unavailable, "zoxide"),
    b!([c('w')], A::Unavailable, "Task manager"),
    b!([Key::ctrl('z')], A::Unavailable, "Suspend"),
    b!([c('g'), c('t')], A::Unavailable, "Trash bin"),
    b!([c('t'), c('t')], A::Unavailable, "Tabs"),
    b!([c('t'), c('r')], A::Unavailable, "Tabs"),
    b!([c('1')], A::Unavailable, "Tabs"),
    b!([c('2')], A::Unavailable, "Tabs"),
    b!([c('3')], A::Unavailable, "Tabs"),
    b!([c('4')], A::Unavailable, "Tabs"),
    b!([c('5')], A::Unavailable, "Tabs"),
    b!([c('6')], A::Unavailable, "Tabs"),
    b!([c('7')], A::Unavailable, "Tabs"),
    b!([c('8')], A::Unavailable, "Tabs"),
    b!([c('9')], A::Unavailable, "Tabs"),
    b!([c('[')], A::Unavailable, "Tabs"),
    b!([c(']')], A::Unavailable, "Tabs"),
    b!([c('{')], A::Unavailable, "Tabs"),
    b!([c('}')], A::Unavailable, "Tabs"),
];

/// Result of feeding one key into a possibly pending chord.
#[derive(Debug, PartialEq)]
pub enum Resolved {
    Run(&'static Binding),
    /// The key starts a chord; show candidates and wait.
    Pending,
    /// No binding; any pending chord is dropped.
    Unbound,
}

pub fn resolve(pending: &mut Vec<Key>, key: Key) -> Resolved {
    let mut seq = std::mem::take(pending);
    seq.push(key);
    let mut prefix = false;
    for binding in BINDINGS {
        if binding.on == seq.as_slice() {
            return Resolved::Run(binding);
        }
        if binding.on.len() > seq.len() && binding.on.starts_with(&seq) {
            prefix = true;
        }
    }
    if prefix {
        *pending = seq;
        Resolved::Pending
    } else {
        Resolved::Unbound
    }
}

/// Bindings that continue the pending chord, for the which-key popup.
pub fn candidates(pending: &[Key]) -> Vec<&'static Binding> {
    BINDINGS
        .iter()
        .filter(|b| b.on.len() > pending.len() && b.on.starts_with(pending))
        .filter(|b| b.action != Action::Unavailable)
        .collect()
}

pub fn chord_label(on: &[Key]) -> String {
    on.iter().map(Key::label).collect()
}

/// Help entries filtered like yazi: chord prefix (case-sensitive), chord
/// prefix (any case), chord substring, then description substring.
pub fn help_entries(query: &str) -> Vec<&'static Binding> {
    let all: Vec<&Binding> = BINDINGS
        .iter()
        .filter(|b| b.action != Action::Unavailable)
        .collect();
    if query.is_empty() {
        return all;
    }
    let q = query.to_lowercase();
    let mut out: Vec<&Binding> = Vec::new();
    let buckets: [&dyn Fn(&Binding) -> bool; 4] = [
        &|b| chord_label(b.on).starts_with(query),
        &|b| chord_label(b.on).to_lowercase().starts_with(&q),
        &|b| chord_label(b.on).to_lowercase().contains(&q),
        &|b| b.desc.to_lowercase().contains(&q),
    ];
    for bucket in buckets {
        for b in &all {
            if bucket(b) && !out.iter().any(|o| std::ptr::eq(*o, *b)) {
                out.push(b);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(c: char) -> Key {
        Key::from_event(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
    }

    #[test]
    fn single_keys_and_chords_resolve() {
        let mut pending = Vec::new();
        assert!(matches!(
            resolve(&mut pending, press('j')),
            Resolved::Run(b) if b.action == Action::Arrow(Step::Next)
        ));
        assert_eq!(resolve(&mut pending, press('g')), Resolved::Pending);
        assert_eq!(candidates(&pending).len(), 6);
        assert!(matches!(
            resolve(&mut pending, press('g')),
            Resolved::Run(b) if b.action == Action::Arrow(Step::Top)
        ));
        assert!(pending.is_empty());
        assert_eq!(resolve(&mut pending, press(',')), Resolved::Pending);
        assert_eq!(resolve(&mut pending, press('x')), Resolved::Unbound);
        assert!(pending.is_empty());
    }

    #[test]
    fn shifted_letters_match_uppercase_bindings() {
        let key = Key::from_event(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT));
        let mut pending = Vec::new();
        assert!(matches!(
            resolve(&mut pending, key),
            Resolved::Run(b) if b.action == Action::Arrow(Step::Bot)
        ));
    }

    #[test]
    fn labels_follow_yazi_notation() {
        assert_eq!(chord_label(&[c('g'), c(' ')]), "g<Space>");
        assert_eq!(Key::ctrl('a').label(), "<C-a>");
        assert_eq!(Key::shift(KeyCode::PageUp).label(), "<S-PageUp>");
    }

    #[test]
    fn help_filter_ranks_chord_matches_first() {
        let found = help_entries("gh");
        assert_eq!(found[0].action, Action::Goto(Goto::Home));
        assert!(help_entries("sort")
            .iter()
            .all(|b| matches!(b.action, Action::Sort(..))));
        assert!(help_entries("")
            .iter()
            .all(|b| b.action != Action::Unavailable));
    }
}
