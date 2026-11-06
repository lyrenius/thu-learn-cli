//! Behaviour tests against yazi's documented semantics, on temporary directories.

use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    /// dir/
    ///   Slides/ (with lecture1.pdf)
    ///   drafts/ (empty)
    ///   .hidden
    ///   answer.pdf, notes.txt (two lines), report-final.md, report2.md, report10.md
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "thu-learn-picker-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("Slides")).unwrap();
        std::fs::create_dir_all(dir.join("drafts")).unwrap();
        std::fs::write(dir.join("Slides/lecture1.pdf"), b"%PDF").unwrap();
        std::fs::write(dir.join(".hidden"), b"x").unwrap();
        std::fs::write(dir.join("answer.pdf"), b"%PDF-1.7").unwrap();
        let lines: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.join("notes.txt"), lines).unwrap();
        std::fs::write(dir.join("report-final.md"), b"# final").unwrap();
        std::fs::write(dir.join("report2.md"), b"2").unwrap();
        std::fs::write(dir.join("report10.md"), b"10").unwrap();
        Self {
            dir: std::fs::canonicalize(&dir).unwrap(),
        }
    }

    fn picker(&self) -> Picker {
        let mut p = Picker::open(&self.dir);
        p.set_limits(20, 20);
        p
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn key(p: &mut Picker, code: KeyCode) -> Outcome {
    p.on_key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn ch(p: &mut Picker, c: char) -> Outcome {
    key(p, KeyCode::Char(c))
}

fn ctrl(p: &mut Picker, c: char) -> Outcome {
    p.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

fn typed(p: &mut Picker, s: &str) {
    for c in s.chars() {
        ch(p, c);
    }
}

fn names(p: &Picker) -> Vec<&str> {
    p.current.items.iter().map(|e| e.name.as_str()).collect()
}

fn hovered(p: &Picker) -> &str {
    p.hovered().map(|e| e.name.as_str()).unwrap_or("")
}

#[test]
fn lists_dirs_first_alphabetically_without_dot_files() {
    let fx = Fixture::new("list");
    let p = fx.picker();
    assert_eq!(
        names(&p),
        [
            "drafts",
            "Slides",
            "answer.pdf",
            "notes.txt",
            "report-final.md",
            "report10.md",
            "report2.md"
        ]
    );
    assert_eq!(hovered(&p), "drafts");
}

#[test]
fn j_k_wrap_and_space_clamps() {
    let fx = Fixture::new("wrap");
    let mut p = fx.picker();
    ch(&mut p, 'k');
    assert_eq!(hovered(&p), "report2.md");
    ch(&mut p, 'j');
    assert_eq!(hovered(&p), "drafts");
    ch(&mut p, 'G');
    ch(&mut p, ' ');
    // Space toggles and moves with a clamped offset: it stays on the last row.
    assert_eq!(hovered(&p), "report2.md");
    assert_eq!(p.selected, [fx.dir.join("report2.md")]);
}

#[test]
fn esc_peels_find_then_filter_then_selection() {
    let fx = Fixture::new("esc");
    let mut p = fx.picker();
    ch(&mut p, ' ');
    ch(&mut p, 'f');
    typed(&mut p, "report");
    key(&mut p, KeyCode::Enter);
    ch(&mut p, '/');
    typed(&mut p, "final");
    key(&mut p, KeyCode::Enter);
    assert!(p.finder.is_some() && p.current.filter.is_some() && !p.selected.is_empty());
    key(&mut p, KeyCode::Esc);
    assert!(p.finder.is_none() && p.current.filter.is_some());
    key(&mut p, KeyCode::Esc);
    assert!(p.current.filter.is_none() && !p.selected.is_empty());
    key(&mut p, KeyCode::Esc);
    assert!(p.selected.is_empty());
    // With nothing left, Esc does nothing and does not close the picker.
    assert_eq!(key(&mut p, KeyCode::Esc), Outcome::Continue);
}

#[test]
fn filter_is_live_smart_case_and_kept_per_directory() {
    let fx = Fixture::new("filter");
    let mut p = fx.picker();
    ch(&mut p, 'f');
    typed(&mut p, "REP");
    assert!(
        names(&p).is_empty(),
        "uppercase makes the match case-sensitive"
    );
    key(&mut p, KeyCode::Backspace);
    key(&mut p, KeyCode::Backspace);
    key(&mut p, KeyCode::Backspace);
    typed(&mut p, "rep");
    assert_eq!(names(&p), ["report-final.md", "report10.md", "report2.md"]);
    // An invalid pattern keeps the last valid filter and flags the input.
    typed(&mut p, "(");
    assert_eq!(names(&p).len(), 3);
    assert!(matches!(p.popup, Some(Popup::Input { invalid: true, .. })));
    key(&mut p, KeyCode::Backspace);
    // Esc twice leaves the input but keeps the filter.
    key(&mut p, KeyCode::Esc);
    key(&mut p, KeyCode::Esc);
    assert!(p.popup.is_none());
    assert_eq!(p.current.filter.as_ref().unwrap().raw, "rep");
    // The filter is remembered when coming back to the directory.
    ch(&mut p, 'h');
    ch(&mut p, 'l');
    assert_eq!(
        p.current.filter.as_ref().map(|f| f.raw.as_str()),
        Some("rep")
    );
    // Empty filter results say so.
    ch(&mut p, 'f');
    typed(&mut p, "zzz");
    assert!(names(&p).is_empty());
    assert!(p.current.filter.is_some());
}

#[test]
fn find_jumps_inclusively_and_n_wraps_exclusively() {
    let fx = Fixture::new("find");
    let mut p = fx.picker();
    ch(&mut p, '/');
    typed(&mut p, "report");
    assert_eq!(hovered(&p), "report-final.md");
    key(&mut p, KeyCode::Enter);
    assert_eq!(p.found_position(), Some((1, 3)));
    ch(&mut p, 'n');
    assert_eq!(hovered(&p), "report10.md");
    ch(&mut p, 'n');
    ch(&mut p, 'n');
    assert_eq!(hovered(&p), "report-final.md", "n wraps around");
    ch(&mut p, 'N');
    assert_eq!(hovered(&p), "report2.md");
    // `?` searches backwards from the cursor, including it.
    ch(&mut p, 'g');
    ch(&mut p, 'g');
    ch(&mut p, '?');
    typed(&mut p, "pdf");
    assert_eq!(hovered(&p), "answer.pdf");
}

#[test]
fn hidden_toggle_and_sorting_keep_the_hovered_file() {
    let fx = Fixture::new("sort");
    let mut p = fx.picker();
    ch(&mut p, 'G');
    assert_eq!(hovered(&p), "report2.md");
    ch(&mut p, '.');
    assert!(names(&p).contains(&".hidden"));
    assert_eq!(hovered(&p), "report2.md");
    ch(&mut p, ',');
    ch(&mut p, 'n');
    assert_eq!(
        &names(&p)[names(&p).len() - 2..],
        ["report2.md", "report10.md"]
    );
    assert_eq!(hovered(&p), "report2.md");
    assert_eq!(p.linemode, Linemode::None, ",n leaves the linemode alone");
    ch(&mut p, ',');
    ch(&mut p, 'M');
    assert_eq!(
        p.linemode,
        Linemode::Mtime,
        ",M also shows modification times"
    );
    ch(&mut p, 'm');
    ch(&mut p, 's');
    assert_eq!(p.linemode, Linemode::Size);
}

#[test]
fn leave_enter_back_and_forward() {
    let fx = Fixture::new("history");
    let mut p = fx.picker();
    ch(&mut p, 'j'); // Slides
    ch(&mut p, 'l');
    assert_eq!(p.cwd, fx.dir.join("Slides"));
    assert_eq!(hovered(&p), "lecture1.pdf");
    // l on a file does nothing.
    ch(&mut p, 'l');
    assert_eq!(p.cwd, fx.dir.join("Slides"));
    ch(&mut p, 'h');
    assert_eq!(p.cwd, fx.dir);
    assert_eq!(hovered(&p), "Slides");
    ch(&mut p, 'H');
    assert_eq!(p.cwd, fx.dir.join("Slides"));
    ch(&mut p, 'H');
    assert_eq!(p.cwd, fx.dir);
    ch(&mut p, 'L');
    assert_eq!(p.cwd, fx.dir.join("Slides"));
}

#[test]
fn interactive_cd_completes_and_reveals_files() {
    let fx = Fixture::new("cd");
    let mut p = fx.picker();
    ch(&mut p, 'g');
    ch(&mut p, ' ');
    assert!(matches!(
        p.popup,
        Some(Popup::Input {
            kind: InputKind::Cd,
            ..
        })
    ));
    typed(&mut p, &format!("{}/Sl", fx.dir.display()));
    let Some(Popup::Input { cmp: Some(cmp), .. }) = &p.popup else {
        panic!("completion should be active");
    };
    assert_eq!(cmp.items, [("Slides".to_string(), true)]);
    key(&mut p, KeyCode::Tab);
    typed(&mut p, "lec");
    key(&mut p, KeyCode::Tab);
    key(&mut p, KeyCode::Enter);
    // A file path reveals the file in its folder.
    assert_eq!(p.cwd, fx.dir.join("Slides"));
    assert_eq!(hovered(&p), "lecture1.pdf");
    // A missing path does nothing.
    ch(&mut p, 'g');
    ch(&mut p, ' ');
    typed(&mut p, "/definitely/not/here");
    key(&mut p, KeyCode::Enter);
    assert_eq!(p.cwd, fx.dir.join("Slides"));
}

#[test]
fn open_chooses_one_file_and_asks_for_a_comment() {
    let fx = Fixture::new("open");
    let mut p = fx.picker();
    // Enter on a folder enters it (the one difference from yazi's chooser).
    key(&mut p, KeyCode::Enter);
    assert_eq!(p.cwd, fx.dir.join("drafts"));
    ch(&mut p, 'h');
    // Two selected files are refused.
    ch(&mut p, 'G');
    ch(&mut p, ' ');
    ch(&mut p, 'k');
    ch(&mut p, ' ');
    key(&mut p, KeyCode::Enter);
    assert!(p.popup.is_none());
    assert!(p.notices.last().unwrap().message.contains("2 are selected"));
    // One selected file wins over the hovered one.
    key(&mut p, KeyCode::Esc);
    ch(&mut p, 'g');
    ch(&mut p, 'g');
    ch(&mut p, 'G');
    ch(&mut p, ' ');
    ch(&mut p, 'g');
    ch(&mut p, 'g');
    key(&mut p, KeyCode::Enter);
    typed(&mut p, "v2");
    assert_eq!(
        key(&mut p, KeyCode::Enter),
        Outcome::Submit {
            file: fx.dir.join("report2.md"),
            comment: "v2".into(),
        }
    );
}

#[test]
fn unavailable_keys_explain_themselves() {
    let fx = Fixture::new("unavailable");
    let mut p = fx.picker();
    ch(&mut p, 'd');
    assert_eq!(p.notices.last().unwrap().title, "Trash");
    assert!(fx.dir.join("drafts").exists());
}

#[test]
fn chords_show_candidates_and_unknown_keys_cancel() {
    let fx = Fixture::new("chord");
    let mut p = fx.picker();
    ch(&mut p, 'g');
    assert_eq!(p.pending.len(), 1);
    key(&mut p, KeyCode::Esc);
    assert!(p.pending.is_empty());
    assert_eq!(hovered(&p), "drafts");
}

#[test]
fn copy_spot_help_and_seek() {
    let fx = Fixture::new("misc");
    let mut p = fx.picker();
    ch(&mut p, 'G');
    ch(&mut p, 'c');
    assert_eq!(ch(&mut p, 'f'), Outcome::Copy("report2.md".into()));
    ch(&mut p, 'c');
    assert_eq!(ch(&mut p, 'd'), Outcome::Copy(fx.dir.display().to_string()));

    key(&mut p, KeyCode::Tab);
    assert!(matches!(p.popup, Some(Popup::Spot { .. })));
    let rows = p.spot_rows(None);
    assert_eq!(rows[0].0, "Base");
    key(&mut p, KeyCode::Tab);
    assert!(p.popup.is_none());

    ch(&mut p, '~');
    typed(&mut p, "gh");
    let Some(Popup::Help { editor, .. }) = &p.popup else {
        panic!("help should be open");
    };
    assert_eq!(
        keymap::help_entries(&editor.value())[0].action,
        Action::Goto(Goto::Home)
    );
    key(&mut p, KeyCode::Esc);
    key(&mut p, KeyCode::Esc);
    assert!(p.popup.is_none());

    // J scrolls a text preview by half its height.
    ch(&mut p, 'g');
    ch(&mut p, 'g');
    for _ in 0..3 {
        ch(&mut p, 'j');
    }
    assert_eq!(hovered(&p), "notes.txt");
    ch(&mut p, 'J');
    assert_eq!(p.preview.as_ref().unwrap().skip, 10);
    ch(&mut p, 'K');
    assert_eq!(p.preview.as_ref().unwrap().skip, 0);
    ctrl(&mut p, 'c');
}

#[test]
fn quit_keys_cancel() {
    let fx = Fixture::new("quit");
    let mut p = fx.picker();
    assert_eq!(ch(&mut p, 'q'), Outcome::Cancel);
    assert_eq!(ctrl(&mut p, 'c'), Outcome::Cancel);
}

fn render(p: &mut Picker, w: u16, h: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    let ctx = view::Context {
        course: "Networks",
        title: "Socket report",
    };
    terminal.draw(|f| view::draw(f, f.area(), p, &ctx)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..h {
        let mut skip = 0;
        for x in 0..w {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let s = buffer[(x, y)].symbol();
            skip = unicode_width::UnicodeWidthStr::width(s).saturating_sub(1);
            out.push_str(s);
        }
        out.push('\n');
    }
    out
}

#[test]
fn renders_yazi_header_columns_and_status() {
    let fx = Fixture::new("render");
    let mut p = fx.picker();
    ch(&mut p, 'f');
    typed(&mut p, "report");
    key(&mut p, KeyCode::Enter);
    ch(&mut p, ' ');
    let screen = render(&mut p, 120, 16);
    let lines: Vec<&str> = screen.lines().collect();
    assert!(lines[0].contains("Submit · Networks | Socket report"));
    assert!(lines[1].contains("(filter: report)"));
    assert!(
        lines[1].trim_end().ends_with("1"),
        "selected count on the right"
    );
    assert!(screen.contains("report-final.md"));
    assert!(!screen.contains("answer.pdf"));
    let status = lines[15];
    assert!(status.starts_with(" NOR "));
    assert!(status.contains(" 2/3 "));
    assert!(status.contains("-rw"));
}

#[test]
fn renders_empty_states() {
    let fx = Fixture::new("empty");
    let mut p = fx.picker();
    key(&mut p, KeyCode::Enter); // into the empty drafts folder
    assert!(render(&mut p, 100, 12).contains("No items"));
    ch(&mut p, 'h');
    ch(&mut p, 'f');
    typed(&mut p, "nothing-matches");
    key(&mut p, KeyCode::Enter);
    assert!(render(&mut p, 100, 12).contains("No filter results"));
}

#[test]
fn file_names_cannot_inject_escape_sequences() {
    let fx = Fixture::new("escape");
    std::fs::write(fx.dir.join("evil\u{1b}]52;c;Zm9v\u{7}.txt"), b"x").unwrap();
    let mut p = fx.picker();
    let screen = render(&mut p, 120, 16);
    assert!(!screen.contains('\u{1b}'));
    assert!(screen.contains("evil"));
}

// ---------- Regressions from the adversarial review ----------

#[test]
fn completion_follows_the_cursor_component() {
    let fx = Fixture::new("cmp-move");
    let mut p = fx.picker();
    ch(&mut p, 'g');
    ch(&mut p, ' ');
    typed(&mut p, "Slides/lec");
    key(&mut p, KeyCode::Home);
    key(&mut p, KeyCode::Tab);
    let Some(Popup::Input { editor, .. }) = &p.popup else {
        panic!("input should stay open");
    };
    assert!(
        !editor.value().contains("lecture1.pdf/"),
        "a stale candidate must not be spliced in: {}",
        editor.value()
    );
}

#[test]
fn completion_lists_dot_dot() {
    let fx = Fixture::new("cmp-dots");
    let mut p = fx.picker();
    ch(&mut p, 'j');
    ch(&mut p, 'l'); // into Slides
    ch(&mut p, 'g');
    ch(&mut p, ' ');
    typed(&mut p, "..");
    let Some(Popup::Input { cmp: Some(cmp), .. }) = &p.popup else {
        panic!("completion should be active");
    };
    assert!(cmp.items.iter().any(|(n, _)| n == "answer.pdf"));
    key(&mut p, KeyCode::Tab);
    let Some(Popup::Input { editor, .. }) = &p.popup else {
        panic!("input should stay open");
    };
    assert!(editor.value().starts_with("../"));
}

#[test]
fn parent_tracks_a_hidden_cwd_after_showing_hidden_files() {
    let fx = Fixture::new("parent-hidden");
    std::fs::create_dir_all(fx.dir.join(".cfg")).unwrap();
    let mut p = fx.picker();
    ch(&mut p, 'g');
    ch(&mut p, ' ');
    typed(&mut p, ".cfg");
    key(&mut p, KeyCode::Enter);
    assert_eq!(p.cwd, fx.dir.join(".cfg"));
    ch(&mut p, '.');
    let parent = p.parent.as_ref().unwrap();
    assert_eq!(parent.hovered().unwrap().name, ".cfg");
}

#[test]
fn invert_keeps_selection_order() {
    let fx = Fixture::new("invert");
    let mut p = fx.picker();
    ch(&mut p, 'j');
    ch(&mut p, 'l'); // Slides
    ch(&mut p, ' '); // Slides/lecture1.pdf
    ch(&mut p, 'h');
    ch(&mut p, 'G');
    ch(&mut p, ' '); // report2.md
    ctrl(&mut p, 'r'); // invert in the parent folder
    assert_eq!(p.selected[0], fx.dir.join("Slides/lecture1.pdf"));
    assert!(!p.selected.contains(&fx.dir.join("report2.md")));
}

#[test]
fn spot_chord_and_multi_spotter() {
    let fx = Fixture::new("spot");
    let mut p = fx.picker();
    ch(&mut p, 'G');
    key(&mut p, KeyCode::Tab);
    ch(&mut p, 'c');
    assert_eq!(
        ctrl(&mut p, 'c'),
        Outcome::Continue,
        "C-c must not complete `cc`"
    );
    key(&mut p, KeyCode::Esc);
    ch(&mut p, ' ');
    ch(&mut p, 'k');
    key(&mut p, KeyCode::Tab);
    let rows = p.spot_rows(None);
    assert_eq!(
        rows[0].0, "Multi",
        "one selected file uses the multi spotter"
    );
    ch(&mut p, '~');
    assert!(matches!(p.popup, Some(Popup::Help { spot: Some(_), .. })));
    key(&mut p, KeyCode::Esc);
    key(&mut p, KeyCode::Esc);
    assert!(
        matches!(p.popup, Some(Popup::Spot { .. })),
        "help returns to the spot"
    );
}

#[test]
fn help_scrolls_within_the_drawn_height() {
    let fx = Fixture::new("help-rows");
    let mut p = fx.picker();
    ch(&mut p, '~');
    render(&mut p, 80, 24);
    for _ in 0..30 {
        key(&mut p, KeyCode::Down);
    }
    let screen = render(&mut p, 80, 24);
    let Some(Popup::Help { scroll, .. }) = &p.popup else {
        panic!("help should be open");
    };
    assert!(scroll.cursor >= scroll.offset && scroll.cursor < scroll.offset + p.help_limit);
    let items = keymap::help_items(false, "");
    assert!(screen.contains(items[scroll.cursor].desc));
}

#[test]
fn seek_floors_like_yazi() {
    let fx = Fixture::new("seek");
    let mut p = fx.picker();
    p.set_limits(21, 21);
    for _ in 0..3 {
        ch(&mut p, 'j');
    }
    assert_eq!(hovered(&p), "notes.txt");
    ch(&mut p, 'J');
    ch(&mut p, 'J');
    let after_down = p.preview.as_ref().unwrap().skip;
    ch(&mut p, 'K');
    // floor(-5 * 21 / 10) = -11, one more than truncation would give.
    assert_eq!(p.preview.as_ref().unwrap().skip, after_down - 11);
}

#[test]
fn paste_cancels_a_pending_chord() {
    let fx = Fixture::new("paste-chord");
    let mut p = fx.picker();
    ch(&mut p, 'g');
    p.on_paste("/definitely/missing");
    assert!(p.pending.is_empty());
}

#[test]
fn filter_moves_trace_to_the_new_hovered_file() {
    let fx = Fixture::new("filter-trace");
    let mut p = fx.picker();
    ch(&mut p, 'j');
    ch(&mut p, 'j'); // answer.pdf
    ch(&mut p, 'f');
    typed(&mut p, r"\d");
    key(&mut p, KeyCode::Enter);
    let before = hovered(&p).to_string();
    ch(&mut p, ',');
    ch(&mut p, 'n');
    assert_eq!(hovered(&p), before);
}

#[test]
fn listings_refresh_when_the_folder_changes() {
    let fx = Fixture::new("refresh");
    let mut p = fx.picker();
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(fx.dir.join("new-file.pdf"), b"x").unwrap();
    // Some filesystems keep a coarse directory mtime; force a visible change.
    let later = std::time::SystemTime::now() + Duration::from_secs(5);
    std::fs::File::open(&fx.dir)
        .and_then(|f| f.set_modified(later))
        .ok();
    p.tick(Instant::now() + Duration::from_secs(2));
    assert!(names(&p).contains(&"new-file.pdf"));
}

#[test]
fn notices_leave_the_header_alone() {
    let fx = Fixture::new("notice");
    let mut p = fx.picker();
    ch(&mut p, ' ');
    ch(&mut p, ' ');
    key(&mut p, KeyCode::Enter); // refused: two selected
    let screen = render(&mut p, 80, 24);
    let header = screen.lines().nth(1).unwrap();
    assert!(
        header.trim_end().ends_with('2'),
        "selected count stays visible: {header}"
    );
}

#[test]
fn long_names_stay_whole_before_the_link_target() {
    let fx = Fixture::new("longlink");
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        "../shared/some/far/away/target/hw3.pdf",
        fx.dir.join("Homework3_report_final_v2.pdf"),
    )
    .unwrap();
    #[cfg(unix)]
    {
        let mut p = fx.picker();
        let screen = render(&mut p, 80, 16);
        assert!(screen.contains("Homework3_report_final_v2.pdf -> "));
    }
}

#[test]
fn tiny_terminals_do_not_panic() {
    let fx = Fixture::new("tiny");
    let mut p = fx.picker();
    let sizes = [(1, 1), (2, 2), (10, 3), (20, 5), (40, 8), (80, 4)];
    let setups: [&dyn Fn(&mut Picker); 6] = [
        &|_| {},
        &|p| {
            ch(p, 'g');
        },
        &|p| {
            ch(p, 'f');
        },
        &|p| {
            ch(p, '~');
        },
        &|p| {
            key(p, KeyCode::Tab);
        },
        &|p| {
            ch(p, 'd');
            ch(p, 'g');
            ch(p, ' ');
            typed(p, &format!("{}/", fx.dir.display()));
        },
    ];
    for setup in setups {
        let mut fresh = fx.picker();
        setup(&mut fresh);
        for (w, h) in sizes {
            render(&mut fresh, w, h);
        }
    }
    p.on_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
}

fn mouse(p: &mut Picker, kind: MouseEventKind, (column, row): (u16, u16)) -> Outcome {
    p.on_mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn click(p: &mut Picker, at: (u16, u16)) -> Outcome {
    mouse(p, MouseEventKind::Down(MouseButton::Left), at)
}

/// Cell position of the first `needle` on screen (fixture names are narrow).
fn find(screen: &str, needle: &str) -> (u16, u16) {
    screen
        .lines()
        .enumerate()
        .find_map(|(y, line)| {
            let byte = line.find(needle)?;
            Some((line[..byte].chars().count() as u16, y as u16))
        })
        .unwrap_or_else(|| panic!("{needle:?} not on screen:\n{screen}"))
}

fn hovered_name(p: &Picker) -> String {
    p.hovered().map(|e| e.name.clone()).unwrap_or_default()
}

#[test]
fn mouse_follows_yazi_click_and_scroll() {
    let fx = Fixture::new("mouse");
    let mut p = fx.picker();
    let screen = render(&mut p, 120, 16);

    // In the current column a click hovers, the wheel moves one entry, and a
    // middle click does nothing.
    let notes = find(&screen, "notes.txt");
    click(&mut p, notes);
    assert_eq!(hovered_name(&p), "notes.txt");
    let names: Vec<String> = p.current.items.iter().map(|e| e.name.clone()).collect();
    let i = names.iter().position(|n| n == "notes.txt").unwrap();
    mouse(&mut p, MouseEventKind::ScrollDown, notes);
    assert_eq!(hovered_name(&p), names[i + 1]);
    mouse(&mut p, MouseEventKind::ScrollUp, notes);
    mouse(
        &mut p,
        MouseEventKind::Down(MouseButton::Middle),
        find(&screen, "answer.pdf"),
    );
    assert_eq!(hovered_name(&p), "notes.txt");

    // The wheel over a file preview seeks it.
    render(&mut p, 120, 16);
    let preview = (p.areas.preview.x + 2, p.areas.preview.y + 2);
    mouse(&mut p, MouseEventKind::ScrollDown, preview);
    assert!(p.preview.as_ref().unwrap().skip > 0);

    // A right click opens the entry: a file asks for the comment, and the
    // mouse is ignored while that popup is open.
    mouse(
        &mut p,
        MouseEventKind::Down(MouseButton::Right),
        find(&screen, "answer.pdf"),
    );
    assert!(matches!(p.popup, Some(Popup::Input { .. })));
    click(&mut p, notes);
    assert_eq!(hovered_name(&p), "answer.pdf");
    key(&mut p, KeyCode::Esc);
    key(&mut p, KeyCode::Esc);
    assert!(p.popup.is_none());

    // A click on an entry of a previewed folder reveals it.
    click(&mut p, find(&screen, "Slides"));
    let screen = render(&mut p, 120, 16);
    click(&mut p, find(&screen, "lecture1.pdf"));
    assert_eq!(p.cwd, fx.dir.join("Slides"));
    assert_eq!(hovered_name(&p), "lecture1.pdf");

    // In the parent column a click reveals an entry.
    let screen = render(&mut p, 120, 16);
    click(&mut p, find(&screen, "answer.pdf"));
    assert_eq!(p.cwd, fx.dir);
    assert_eq!(hovered_name(&p), "answer.pdf");

    // A right click on a folder enters it; a click below the parent
    // column's last entry leaves it again.
    let screen = render(&mut p, 120, 16);
    mouse(
        &mut p,
        MouseEventKind::Down(MouseButton::Right),
        find(&screen, "Slides"),
    );
    assert_eq!(p.cwd, fx.dir.join("Slides"));
    render(&mut p, 120, 16);
    let parent = p.areas.parent;
    click(&mut p, (parent.x, parent.y + parent.height - 1));
    assert_eq!(p.cwd, fx.dir);
    assert_eq!(hovered_name(&p), "Slides");
}
