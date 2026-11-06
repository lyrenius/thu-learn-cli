//! Rendering for the TUI. Everything here reads [`App`] state and draws it;
//! the only writes back are scroll bookkeeping the layout decides.

use chrono::{DateTime, Datelike, Local};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Scrollbar,
    ScrollbarOrientation, ScrollbarState, Table, TableState, Tabs, Wrap,
};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::app::{
    is_overdue, AnnouncementFilter, App, DetailState, Hits, HomeworkFilter, Overlay, Session,
    StatusKind, SubmitState, Tab,
};
use super::picker::view as picker_view;
use crate::cli::strip_terminal_controls;
use crate::models::{short_id, Course, CourseFile, Homework, HomeworkStatus, Notification};

const ACCENT: Color = Color::Cyan;
const MUTED: Color = Color::DarkGray;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Side-by-side list and detail panes from this width; stacked below it.
const WIDE: u16 = 110;

fn muted() -> Style {
    Style::new().fg(MUTED)
}

fn heading(text: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("▍", Style::new().fg(ACCENT)),
        Span::styled(text.to_string(), Style::new().fg(ACCENT).bold()),
    ])
}

fn key_hint(key: &str, desc: &str) -> Vec<Span<'static>> {
    vec![
        Span::styled(key.to_string(), Style::new().fg(ACCENT).bold()),
        Span::styled(format!(" {desc}  "), muted()),
    ]
}

fn block(title: Line<'static>) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(muted())
        .title(title)
}

/// Sanitizes one line of server text for display.
fn clean(s: &str) -> String {
    strip_terminal_controls(s).trim().to_string()
}

/// Sanitizes multi-line server text, keeping line breaks.
fn clean_lines(s: &str) -> Vec<Line<'static>> {
    s.lines()
        .map(|l| {
            Line::raw(
                strip_terminal_controls(&l.replace('\t', "    "))
                    .trim_end()
                    .to_string(),
            )
        })
        .collect()
}

/// Truncates to `width` display cells, ending with `…` when text was cut.
fn fit(s: &str, width: usize) -> String {
    if UnicodeWidthStr::width(s) <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > width - 1 {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push('…');
    out
}

fn course_label(name: &str) -> String {
    if name.is_empty() {
        "(previous course)".to_string()
    } else {
        clean(name)
    }
}

/// Compact time remaining, such as `45m`, `5h`, `2d 3h`, or `-4d` when overdue.
pub fn time_left(deadline: DateTime<Local>, now: DateTime<Local>) -> (String, Color) {
    let remain = deadline.signed_duration_since(now);
    let mins = remain.num_minutes();
    if mins < 0 {
        let ago = -mins;
        let text = if ago >= 24 * 60 {
            format!("-{}d", ago / (24 * 60))
        } else if ago >= 60 {
            format!("-{}h", ago / 60)
        } else {
            format!("-{ago}m")
        };
        return (text, Color::Red);
    }
    let (d, h, m) = (mins / (24 * 60), (mins / 60) % 24, mins % 60);
    if d == 0 && h == 0 {
        (format!("{m}m"), Color::LightRed)
    } else if d == 0 {
        (format!("{h}h"), Color::LightRed)
    } else if d < 3 {
        (format!("{d}d {h}h"), Color::Yellow)
    } else {
        (format!("{d}d"), Color::Green)
    }
}

fn status_style(h: &Homework, now: DateTime<Local>) -> (&'static str, Color) {
    match h.status {
        HomeworkStatus::Pending if is_overdue(h, now) => ("Overdue", Color::Red),
        HomeworkStatus::Pending => ("Pending", Color::Yellow),
        HomeworkStatus::Submitted => ("Submitted", Color::Green),
        HomeworkStatus::Graded => ("Graded", Color::Cyan),
    }
}

/// Format and width for deadlines in one list: `MM-DD HH:MM` while they all
/// fall in the current year, otherwise with the year, because without it a
/// list sorted by deadline looks shuffled across New Year.
fn deadline_format(
    now: DateTime<Local>,
    deadlines: impl IntoIterator<Item = DateTime<Local>>,
) -> (&'static str, u16) {
    if deadlines.into_iter().all(|d| d.year() == now.year()) {
        ("%m-%d %H:%M", 11)
    } else {
        ("%Y-%m-%d %H:%M", 16)
    }
}

fn date_only(s: &str) -> String {
    clean(s).chars().take(10).collect()
}

fn attachment_section(section: &str) -> &str {
    // Learn section labels are Chinese protocol literals; show English names.
    match section {
        "作业附件" => "Homework",
        "答案附件" => "Answer",
        "上交作业附件" => "Submitted",
        "评语附件" => "Feedback",
        other => other,
    }
}

// ---------- Entry point ----------

pub fn draw(f: &mut Frame, app: &mut App) {
    app.sync_detail_key();
    app.hits = Hits::default();
    let [header, body, status, keys] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(f.area());

    draw_header(f, app, header);
    if app.session == Session::Ready {
        if app.zoomed {
            draw_detail(f, app, body);
        } else if body.width >= WIDE {
            let [list, detail] =
                Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
                    .areas(body);
            draw_list(f, app, list);
            draw_detail(f, app, detail);
        } else {
            let [list, detail] =
                Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)])
                    .areas(body);
            draw_list(f, app, list);
            draw_detail(f, app, detail);
        }
    } else {
        draw_session(f, app, body);
    }
    draw_status(f, app, status);
    draw_keys(f, app, keys);

    // Taken out while drawing so the picker can update its caches and scroll offset.
    let mut overlay = app.overlay.take();
    match &mut overlay {
        Some(Overlay::Help) => draw_help(f, app),
        Some(Overlay::CoursePicker { selected }) => draw_course_picker(f, app, *selected),
        Some(Overlay::Submit(submit)) => draw_submit(f, submit),
        Some(Overlay::Confirm {
            title, lines, back, ..
        }) => {
            if let Some(submit) = back {
                submit.picker.notices.clear();
                draw_submit(f, submit);
            }
            (app.hits.yes, app.hits.no) = draw_confirm(f, title, lines);
        }
        Some(Overlay::DownloadDir { input, error }) => {
            draw_download_dir(f, input, error.as_deref())
        }
        None => {}
    }
    app.overlay = overlay;
}

// ---------- Chrome ----------

fn draw_header(f: &mut Frame, app: &mut App, area: Rect) {
    let spinner = if app.is_busy() {
        SPINNER[app.spinner % SPINNER.len()]
    } else {
        " "
    };
    let right = match app.courses.semester.as_str() {
        "" => format!("{spinner} "),
        sem => format!("{sem} {spinner} "),
    };
    let [left, right_area] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(UnicodeWidthStr::width(right.as_str()) as u16),
    ])
    .areas(area);

    let ready = app.session == Session::Ready;
    let badge = |n: usize, color: Color| -> Span<'static> {
        if ready && n > 0 {
            Span::styled(format!(" {n}"), Style::new().fg(color).bold())
        } else {
            Span::raw("")
        }
    };
    let titles: Vec<Line<'static>> = Tab::ALL
        .iter()
        .map(|tab| {
            let num = Span::styled(format!("{} ", tab.index() + 1), muted());
            let extra = match tab {
                Tab::Homework => badge(app.pending_count(None), Color::Yellow),
                Tab::Announcements => badge(app.unread_count(None), ACCENT),
                Tab::Files => Span::raw(""),
                Tab::Courses => Span::raw(""),
            };
            Line::from(vec![num, Span::raw(tab.title()), extra])
        })
        .collect();
    let [brand, tabs_area] =
        Layout::horizontal([Constraint::Length(11), Constraint::Fill(1)]).areas(left);
    if ready {
        // `Tabs` draws each title with one cell of padding on both sides and a
        // one-cell divider in between.
        let mut x = tabs_area.x;
        for (tab, title) in Tab::ALL.iter().zip(&titles) {
            let width = title.width() as u16 + 2;
            let rect = Rect::new(x, tabs_area.y, width, 1).intersection(tabs_area);
            app.hits.tabs.push((rect, *tab));
            x = x.saturating_add(width + 1);
        }
    }
    f.render_widget(
        Paragraph::new(Span::styled(" thu-learn", Style::new().fg(ACCENT).bold())),
        brand,
    );
    let tabs = Tabs::new(titles)
        .select(ready.then_some(app.tab.index()))
        .style(muted())
        .highlight_style(Style::new().fg(Color::White).bold().underlined())
        .divider(Span::styled("│", muted()))
        .padding(" ", " ");
    f.render_widget(tabs, tabs_area);
    f.render_widget(Paragraph::new(Span::styled(right, muted())), right_area);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let search = &app.search[app.tab.index()];
    if app.session == Session::Ready && (app.searching || !search.is_empty()) {
        let prefix = " / ";
        let avail = (area.width as usize).saturating_sub(prefix.len() + 1);
        let (visible, cursor) = search.view(avail);
        let style = if app.searching {
            Style::new().fg(Color::White)
        } else {
            Style::new().fg(ACCENT)
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(prefix, Style::new().fg(ACCENT).bold()),
                Span::styled(visible, style),
            ])),
            area,
        );
        if app.searching && app.overlay.is_none() {
            f.set_cursor_position((area.x + (prefix.len() + cursor) as u16, area.y));
        }
        return;
    }
    let Some(status) = &app.status else {
        return;
    };
    let (icon, color) = match status.kind {
        StatusKind::Info => ("•", ACCENT),
        StatusKind::Success => ("✓", Color::Green),
        StatusKind::Error => ("✗", Color::Red),
    };
    let text = fit(
        &clean(&status.text),
        (area.width as usize).saturating_sub(4),
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {icon} "), Style::new().fg(color).bold()),
            Span::styled(text, Style::new().fg(color)),
        ])),
        area,
    );
}

fn draw_keys(f: &mut Frame, app: &mut App, area: Rect) {
    let hints: Vec<(&str, &str)> = if let Some(overlay) = &app.overlay {
        match overlay {
            Overlay::Help => vec![("Esc", "close")],
            Overlay::CoursePicker { .. } => {
                vec![("↑↓", "choose"), ("⏎", "apply"), ("Esc", "cancel")]
            }
            // The picker draws its own header and status bar over everything.
            Overlay::Submit(_) => Vec::new(),
            Overlay::Confirm { .. } => vec![("y", "confirm"), ("n", "cancel")],
            Overlay::DownloadDir { .. } => {
                vec![("Tab", "complete"), ("⏎", "save"), ("Esc", "cancel")]
            }
        }
    } else {
        match &app.session {
            Session::Checking => vec![("q", "quit")],
            Session::LoggedOut(_) => vec![("r", "retry"), ("q", "quit")],
            Session::Ready if app.searching => {
                vec![("type", "to filter"), ("⏎", "keep"), ("Esc", "clear")]
            }
            Session::Ready if app.zoomed => {
                let mut v = vec![("↑↓", "scroll"), ("n/p", "next/prev")];
                match app.tab {
                    Tab::Homework => v.extend([("d", "download"), ("s", "submit")]),
                    Tab::Files => v.push(("d", download_hint(app))),
                    _ => {}
                }
                v.push(("Esc", "back"));
                v
            }
            Session::Ready => {
                let mut v = vec![("↑↓", "move"), ("⏎", "open")];
                match app.tab {
                    Tab::Homework => {
                        v.extend([("d", "download"), ("s", "submit"), ("f", "filter")])
                    }
                    Tab::Announcements => v.push(("f", "unread")),
                    Tab::Files if app.visual.is_some() => {
                        v.extend([("Esc", "apply"), ("d", "download selected")])
                    }
                    Tab::Files => v.extend([
                        ("Space", "select"),
                        ("v", "visual"),
                        ("d", download_hint(app)),
                    ]),
                    Tab::Courses => v[1] = ("⏎", "show homework"),
                }
                v.extend([
                    ("c", "course"),
                    ("/", "search"),
                    ("r", "refresh"),
                    ("?", "help"),
                    ("q", "quit"),
                ]);
                v
            }
        }
    };
    let (spans, cells) = fit_hints(&hints, area.width as usize);
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    // The picker covers this bar, its confirmation included.
    let covered = matches!(
        app.overlay,
        Some(Overlay::Submit(_) | Overlay::Confirm { back: Some(_), .. })
    );
    if !covered {
        for (x, width, label) in cells {
            if let Some(key) = hint_key(label) {
                let rect = Rect::new(area.x + x, area.y, width, 1).intersection(area);
                app.hits.keys.push((rect, key));
            }
        }
    }
}

/// The key a hint stands for, so clicking the hint can press it. Hints for
/// several keys, such as `↑↓`, have none.
fn hint_key(label: &str) -> Option<KeyEvent> {
    let code = match label {
        "⏎" => KeyCode::Enter,
        "Esc" => KeyCode::Esc,
        "Tab" => KeyCode::Tab,
        "Space" => KeyCode::Char(' '),
        _ => {
            let mut chars = label.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_graphic() => KeyCode::Char(c),
                _ => return None,
            }
        }
    };
    Some(KeyEvent::new(code, KeyModifiers::NONE))
}

/// Width of a hint without its trailing spaces: the part a click hits.
fn hint_width(key: &str, desc: &str) -> u16 {
    (UnicodeWidthStr::width(key) + 1 + UnicodeWidthStr::width(desc)) as u16
}

fn download_hint(app: &App) -> &'static str {
    if app.marked_files().is_empty() {
        "download"
    } else {
        "download selected"
    }
}

/// Lays out key hints in `width` cells. Trailing `?`, `q`, and `Esc` hints are
/// kept when space runs out; middle hints are dropped whole instead of cut.
/// Also returns the column, width, and key label of each hint shown.
fn fit_hints<'a>(
    hints: &[(&'a str, &str)],
    width: usize,
) -> (Vec<Span<'static>>, Vec<(u16, u16, &'a str)>) {
    let w = |(k, d): &(&str, &str)| UnicodeWidthStr::width(*k) + UnicodeWidthStr::width(*d) + 3;
    let pinned_from = hints
        .iter()
        .rposition(|(k, _)| !matches!(*k, "?" | "q" | "Esc"))
        .map_or(0, |i| i + 1);
    let (main, pinned) = hints.split_at(pinned_from);
    // The last hint's two trailing spaces may run past the edge.
    let budget = width + 2;
    let pinned_w: usize = pinned.iter().map(w).sum();
    let mut used = 1;
    let mut kept: Vec<&(&str, &str)> = Vec::new();
    for h in main {
        if used + w(h) + pinned_w > budget {
            break;
        }
        used += w(h);
        kept.push(h);
    }
    for h in pinned {
        if used + w(h) > budget {
            break;
        }
        used += w(h);
        kept.push(h);
    }
    let mut spans = vec![Span::raw(" ")];
    let mut cells = Vec::new();
    let mut x = 1;
    for &(k, d) in kept {
        spans.extend(key_hint(k, d));
        cells.push((x, hint_width(k, d), k));
        x += hint_width(k, d) + 2;
    }
    (spans, cells)
}

// ---------- Lists ----------

fn list_title(app: &App) -> Line<'static> {
    let mut parts = vec![match app.tab {
        Tab::Homework => format!("{} homework", app.homework_filter.label()),
        Tab::Announcements => match app.announcement_filter {
            AnnouncementFilter::All => "Announcements".to_string(),
            AnnouncementFilter::Unread => "Unread announcements".to_string(),
        },
        Tab::Files => "Files".to_string(),
        Tab::Courses => "Courses".to_string(),
    }];
    if app.tab != Tab::Courses {
        if let Some(name) = app
            .course_filter
            .as_deref()
            .and_then(|id| app.course_name(id))
        {
            parts.push(clean(name));
        }
    }
    let mut spans = vec![Span::styled(
        format!(" {} ", parts[0]),
        Style::new().fg(Color::White).bold(),
    )];
    if let Some(course) = parts.get(1) {
        spans.push(Span::styled(
            format!("· {course} "),
            Style::new().fg(ACCENT),
        ));
    }
    let marked = app.marked_files().len();
    if app.tab == Tab::Files && (marked > 0 || app.visual.is_some()) {
        spans.push(Span::styled(
            format!("· {marked} selected "),
            Style::new().fg(Color::LightYellow),
        ));
    }
    if let (Tab::Files, Some(visual)) = (app.tab, &app.visual) {
        // yazi's mode badge for visual select and unset modes.
        let mode = if visual.unset { " UNS " } else { " SEL " };
        spans.push(Span::styled(
            mode,
            Style::new().fg(Color::Black).bg(Color::LightYellow).bold(),
        ));
        spans.push(Span::raw(" "));
    }
    Line::from(spans)
}

/// One list pane: column layout, rows, and what to show when it is empty.
struct ListTable {
    header: Vec<&'static str>,
    widths: Vec<Constraint>,
    rows: Vec<Row<'static>>,
    empty: &'static str,
    loading: bool,
}

/// Renders a selectable table with stable scrolling and a count in the border.
fn draw_table(f: &mut Frame, app: &mut App, area: Rect, table: ListTable) {
    let ListTable {
        header,
        widths,
        rows,
        empty,
        loading,
    } = table;
    let tab = app.tab;
    app.hits.list = area;
    let total = rows.len();
    let selected = app.selected_index(tab);
    let count = match selected {
        Some(i) => format!(" {} of {total} ", i + 1),
        None => String::new(),
    };
    let b = block(list_title(app))
        .title_bottom(Line::styled(count, muted()).right_aligned())
        .border_style(Style::new().fg(ACCENT));

    if rows.is_empty() {
        let text = if loading {
            format!("{} Loading…", SPINNER[app.spinner % SPINNER.len()])
        } else {
            empty.to_string()
        };
        f.render_widget(
            Paragraph::new(Line::styled(text, muted()).centered())
                .block(b)
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    let table = Table::new(rows, widths)
        .header(Row::new(header).style(muted().bold()).bottom_margin(0))
        .block(b)
        .column_spacing(1)
        .row_highlight_style(Style::new().bg(Color::DarkGray).fg(Color::White).bold())
        .highlight_symbol("▌");
    // Borders and the header row are not list rows.
    app.list_height = area.height.saturating_sub(3) as usize;
    app.hits.rows = Rect {
        x: area.x + 1,
        y: area.y + 2,
        width: area.width.saturating_sub(2),
        height: app.list_height as u16,
    }
    .intersection(area);
    let mut state = TableState::default()
        .with_offset(app.offsets[tab.index()])
        .with_selected(selected);
    f.render_stateful_widget(table, area, &mut state);
    app.offsets[tab.index()] = state.offset();
}

/// Width left for a flexible column after fixed columns, borders, and the highlight bar.
fn flex_width(area: Rect, fixed: &[u16]) -> usize {
    let spacing = fixed.len() as u16; // one space between each pair of columns
    area.width
        .saturating_sub(2 + 1 + spacing + fixed.iter().sum::<u16>())
        .max(8) as usize
}

fn course_width<'a>(names: impl Iterator<Item = &'a str>, cap: usize) -> u16 {
    names
        .map(|n| UnicodeWidthStr::width(course_label(n).as_str()))
        .max()
        .unwrap_or(6)
        .clamp(6, cap) as u16
}

fn draw_list(f: &mut Frame, app: &mut App, area: Rect) {
    match app.tab {
        Tab::Homework => draw_homework_list(f, app, area),
        Tab::Announcements => draw_announcement_list(f, app, area),
        Tab::Files => draw_file_list(f, app, area),
        Tab::Courses => draw_course_list(f, app, area),
    }
}

fn draw_homework_list(f: &mut Frame, app: &mut App, area: Rect) {
    let now = app.now;
    let list = app.visible_homework();
    let course_w = course_width(list.iter().map(|h| h.course_name.as_str()), 18);
    let (due_fmt, due_w) = deadline_format(now, list.iter().filter_map(|h| h.deadline));
    let fixed = [9, course_w, due_w, 7];
    let title_w = flex_width(area, &fixed);
    let rows: Vec<Row<'static>> = list
        .iter()
        .map(|h| {
            let (label, color) = status_style(h, now);
            let (due, left) = match h.deadline {
                Some(d) => {
                    let left = if h.status == HomeworkStatus::Pending {
                        let (t, c) = time_left(d, now);
                        Span::styled(t, Style::new().fg(c))
                    } else {
                        Span::raw("")
                    };
                    (d.format(due_fmt).to_string(), left)
                }
                None => (fit(&clean(&h.deadline_raw), due_w as usize), Span::raw("")),
            };
            Row::new(vec![
                Cell::from(Span::styled(label, Style::new().fg(color))),
                Cell::from(Span::styled(
                    fit(&course_label(&h.course_name), course_w as usize),
                    Style::new().fg(ACCENT),
                )),
                Cell::from(fit(&clean(&h.title), title_w)),
                Cell::from(due),
                Cell::from(Line::from(left).right_aligned()),
            ])
        })
        .collect();
    let empty = match app.homework_filter {
        HomeworkFilter::Pending => "No pending homework. Press f to see overdue, done, or all.",
        HomeworkFilter::Overdue => "Nothing overdue.",
        HomeworkFilter::Done => "No submitted homework yet.",
        HomeworkFilter::All => "No homework.",
    };
    let loading = app.homework.loading || !app.homework.loaded;
    let widths = vec![
        Constraint::Length(9),
        Constraint::Length(course_w),
        Constraint::Fill(1),
        Constraint::Length(due_w),
        Constraint::Length(7),
    ];
    let header = vec!["Status", "Course", "Title", "Deadline", "Left"];
    draw_table(
        f,
        app,
        area,
        ListTable {
            header,
            widths,
            rows,
            empty,
            loading,
        },
    );
}

fn draw_announcement_list(f: &mut Frame, app: &mut App, area: Rect) {
    let list = app.visible_announcements();
    let course_w = course_width(list.iter().map(|n| n.course_name.as_str()), 16);
    let fixed = [1, course_w, 10];
    let title_w = flex_width(area, &fixed);
    let rows: Vec<Row<'static>> = list
        .iter()
        .map(|n| {
            let (dot, title_style) = if n.read {
                (Span::raw(" "), Style::new())
            } else {
                (
                    Span::styled("●", Style::new().fg(ACCENT)),
                    Style::new().fg(Color::White).bold(),
                )
            };
            Row::new(vec![
                Cell::from(dot),
                Cell::from(Span::styled(
                    fit(&course_label(&n.course_name), course_w as usize),
                    Style::new().fg(ACCENT),
                )),
                Cell::from(Span::styled(fit(&clean(&n.title), title_w), title_style)),
                Cell::from(Span::styled(date_only(&n.publish_time), muted())),
            ])
        })
        .collect();
    let empty = match app.announcement_filter {
        AnnouncementFilter::All => "No announcements.",
        AnnouncementFilter::Unread => "No unread announcements.",
    };
    let loading = app.announcements.loading || !app.announcements.loaded;
    let widths = vec![
        Constraint::Length(1),
        Constraint::Length(course_w),
        Constraint::Fill(1),
        Constraint::Length(10),
    ];
    let table = ListTable {
        header: vec!["", "Course", "Title", "Published"],
        widths,
        rows,
        empty,
        loading,
    };
    draw_table(f, app, area, table);
}

fn draw_file_list(f: &mut Frame, app: &mut App, area: Rect) {
    let list = app.visible_files();
    let marked_ids = app.marked_files();
    let marked: Vec<bool> = list.iter().map(|x| marked_ids.contains(&x.id)).collect();
    let course_w = course_width(list.iter().map(|x| x.course_name.as_str()), 16);
    let fixed = [course_w, 5, 9, 10];
    let title_w = flex_width(area, &fixed);
    let rows: Vec<Row<'static>> = list
        .iter()
        .map(|x| {
            Row::new(vec![
                Cell::from(Span::styled(
                    fit(&course_label(&x.course_name), course_w as usize),
                    Style::new().fg(ACCENT),
                )),
                Cell::from(fit(&clean(&x.title), title_w)),
                Cell::from(Span::styled(fit(&clean(&x.file_type), 5), muted())),
                Cell::from(Line::from(fit(&clean(&x.size), 9)).right_aligned()),
                Cell::from(Span::styled(date_only(&x.upload_time), muted())),
            ])
        })
        .collect();
    let loading = app.files.loading || !app.files.loaded;
    let widths = vec![
        Constraint::Length(course_w),
        Constraint::Fill(1),
        Constraint::Length(5),
        Constraint::Length(9),
        Constraint::Length(10),
    ];
    let table = ListTable {
        header: vec!["Course", "Title", "Type", "Size", "Uploaded"],
        widths,
        rows,
        empty: "No course files.",
        loading,
    };
    draw_table(f, app, area, table);
    // Like yazi, selected rows get a marker on the pane's left edge, below
    // the top border and the header row.
    let offset = app.offsets[Tab::Files.index()];
    let shown = marked.iter().enumerate().skip(offset).take(app.list_height);
    for (i, _) in shown.filter(|(_, m)| **m) {
        picker_view::draw_marker(f, area.x, area.y + 2 + (i - offset) as u16);
    }
}

fn draw_course_list(f: &mut Frame, app: &mut App, area: Rect) {
    let list = app.visible_courses();
    let teacher_w = list
        .iter()
        .map(|c| UnicodeWidthStr::width(clean(&c.teacher).as_str()))
        .max()
        .unwrap_or(7)
        .clamp(7, 14) as u16;
    let fixed = [teacher_w, 7, 6, 5];
    let name_w = flex_width(area, &fixed);
    let count = |n: usize, color: Color| -> Cell<'static> {
        if n == 0 {
            Cell::from(Line::styled("·", muted()).right_aligned())
        } else {
            Cell::from(Line::styled(n.to_string(), Style::new().fg(color).bold()).right_aligned())
        }
    };
    let rows: Vec<Row<'static>> = list
        .iter()
        .map(|c| {
            Row::new(vec![
                Cell::from(Span::styled(
                    fit(&clean(&c.name), name_w),
                    Style::new().fg(ACCENT),
                )),
                Cell::from(fit(&clean(&c.teacher), teacher_w as usize)),
                count(app.pending_count(Some(&c.id)), Color::Yellow),
                count(app.unread_count(Some(&c.id)), ACCENT),
                count(app.file_count(Some(&c.id)), Color::White),
            ])
        })
        .collect();
    let loading = app.courses_loading;
    let widths = vec![
        Constraint::Fill(1),
        Constraint::Length(teacher_w),
        Constraint::Length(7),
        Constraint::Length(6),
        Constraint::Length(5),
    ];
    let table = ListTable {
        header: vec!["Course", "Teacher", "Pending", "Unread", "Files"],
        widths,
        rows,
        empty: "No courses this semester.",
        loading,
    };
    draw_table(f, app, area, table);
}

// ---------- Details ----------

fn draw_detail(f: &mut Frame, app: &mut App, area: Rect) {
    app.hits.detail = area;
    let title = if app.zoomed {
        Line::styled(" Details ", Style::new().fg(Color::White).bold())
    } else {
        Line::styled(" Details ", muted())
    };
    let mut b = block(title);
    if app.zoomed {
        b = b.border_style(Style::new().fg(ACCENT));
    }
    // A blank row below the top border and above the bottom one; the text
    // lines up with the title on the border.
    let inner = b.inner(area).inner(Margin::new(1, 1));
    // Sections wrap their bodies to this width themselves to keep them indented.
    let width = inner.width as usize;
    let lines = match app.tab {
        Tab::Homework => app
            .selected_homework()
            .map(|h| homework_detail(app, h, width)),
        Tab::Announcements => app.selected_announcement().map(announcement_detail),
        Tab::Files => app.selected_file().map(|x| file_detail(x, width)),
        Tab::Courses => app.selected_course().map(|c| course_detail(app, c, width)),
    };
    let Some(mut lines) = lines else {
        f.render_widget(b, area);
        return;
    };
    let actions = detail_actions(app);
    let action_line = (!actions.is_empty()).then(|| {
        let spans: Vec<Span<'static>> = actions.iter().flat_map(|(k, d)| key_hint(k, d)).collect();
        Line::from(spans)
    });
    if let Some(line) = &action_line {
        lines.push(Line::raw(""));
        lines.push(line.clone());
    }

    let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
    let total = paragraph.line_count(inner.width) as u16;
    app.detail_height = inner.height;
    app.detail_max_scroll = total.saturating_sub(inner.height);
    app.detail_scroll = app.detail_scroll.min(app.detail_max_scroll);
    if let Some(line) = action_line {
        // The actions are the last line. Only its first row can be clicked,
        // so an action after one that wraps is left out.
        let rows = Paragraph::new(line)
            .wrap(Wrap { trim: false })
            .line_count(inner.width) as u16;
        let row = total - rows;
        let y = row
            .checked_sub(app.detail_scroll)
            .filter(|y| *y < inner.height);
        let mut x = 0;
        for (k, d) in actions.iter().take_while(|_| y.is_some()) {
            if x >= inner.width {
                break;
            }
            let width = hint_width(k, d).min(inner.width - x);
            if let (Some(key), Some(y)) = (hint_key(k), y) {
                app.hits
                    .actions
                    .push((Rect::new(inner.x + x, inner.y + y, width, 1), key));
            }
            x = x.saturating_add(hint_width(k, d) + 2);
        }
    }
    if app.detail_max_scroll > 0 {
        let pct = (app.detail_scroll as usize * 100) / app.detail_max_scroll.max(1) as usize;
        b = b.title_bottom(Line::styled(format!(" {pct}% "), muted()).right_aligned());
    }
    f.render_widget(b, area);
    f.render_widget(paragraph.scroll((app.detail_scroll, 0)), inner);

    if app.detail_max_scroll > 0 {
        let mut state = ScrollbarState::new(app.detail_max_scroll as usize + 1)
            .position(app.detail_scroll as usize)
            .viewport_content_length(inner.height as usize);
        let color = if app.zoomed { ACCENT } else { MUTED };
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some("│"))
                .track_style(Style::new().fg(color))
                .thumb_symbol("┃")
                .thumb_style(Style::new().fg(Color::White)),
            area.inner(Margin::new(0, 1)),
            &mut state,
        );
    }
}

/// Minimum gap between a label and its value in detail panes.
const LABEL_GAP: usize = 4;
/// Label column of detail fields: the longest label ("Submitted") plus [`LABEL_GAP`].
const FIELD_WIDTH: usize = 9 + LABEL_GAP;
/// Indent of section bodies under their heading.
const SECTION_INDENT: usize = 2;

/// Pads `label` to `width` terminal cells, always leaving at least [`LABEL_GAP`]
/// spaces so a long label never runs into the value after it.
/// Actions for the selected item, shown as key hints at the end of its details.
fn detail_actions(app: &App) -> Vec<(&'static str, String)> {
    match app.tab {
        Tab::Homework => {
            let detail = app
                .selected_homework()
                .and_then(|h| app.details.get(&h.student_homework_id));
            let Some(DetailState::Loaded(d)) = detail else {
                return Vec::new();
            };
            let mut actions = Vec::new();
            if !d.attachments.is_empty() {
                actions.push(("d", "download all".to_string()));
            }
            actions.push(("s", "submit a file".to_string()));
            actions
        }
        Tab::Files if app.selected_file().is_some() => {
            let dir = app.download_dir.display();
            let desc = match app.marked_files().len() {
                0 => format!("download to {dir}"),
                n => format!("download {n} selected to {dir}, one folder per course"),
            };
            vec![("d", desc)]
        }
        Tab::Courses if app.selected_course().is_some() => {
            vec![("⏎", "show this course's homework".to_string())]
        }
        _ => Vec::new(),
    }
}

fn pad_label(label: &str, width: usize) -> String {
    let gap = width
        .saturating_sub(UnicodeWidthStr::width(label))
        .max(LABEL_GAP);
    format!("{label}{}", " ".repeat(gap))
}

/// Appends a section: its heading, a blank line, then `body` indented under it.
/// `width` is the pane's text width, used to keep wrapped rows indented too.
fn push_section(
    lines: &mut Vec<Line<'static>>,
    title: &str,
    body: Vec<Line<'static>>,
    width: usize,
) {
    lines.push(heading(title));
    lines.push(Line::raw(""));
    for line in body {
        lines.extend(wrap_indented(line, SECTION_INDENT, width));
    }
}

/// Wraps `line` to `width` cells and indents every row by `indent` spaces, so the
/// paragraph's own wrapping never sends a continuation row back to column 0.
/// Rows break after spaces and around wide (CJK) characters, and inside a word
/// only when it does not fit on a row by itself.
fn wrap_indented(line: Line<'static>, indent: usize, width: usize) -> Vec<Line<'static>> {
    let avail = width.saturating_sub(indent).max(1);
    let base = line.style;
    let mut rows: Vec<Vec<(char, Style)>> = Vec::new();
    let mut row: Vec<(char, Style)> = Vec::new();
    let mut used = 0;
    // Length of `row` at the last point where it may be broken.
    let mut brk: Option<usize> = None;
    let cells = line.spans.iter().flat_map(|s| {
        let style = base.patch(s.style);
        s.content
            .chars()
            .map(move |c| (c, style))
            .collect::<Vec<_>>()
    });
    for (c, style) in cells {
        let w = c.width().unwrap_or(0);
        while used + w > avail && !row.is_empty() {
            let rest = brk.take().map(|i| row.split_off(i)).unwrap_or_default();
            rows.push(std::mem::take(&mut row));
            row = rest.into_iter().skip_while(|(c, _)| *c == ' ').collect();
            used = row.iter().map(|(c, _)| c.width().unwrap_or(0)).sum();
        }
        if c == ' ' && row.is_empty() && !rows.is_empty() {
            continue; // No leading spaces on continuation rows.
        }
        if w > 1 && !row.is_empty() {
            brk = Some(row.len());
        }
        row.push((c, style));
        used += w;
        if c == ' ' || w > 1 {
            brk = Some(row.len());
        }
    }
    rows.push(row);

    rows.into_iter()
        .map(|mut row| {
            while row.last().is_some_and(|(c, _)| *c == ' ') {
                row.pop();
            }
            let Some(&(_, first)) = row.first() else {
                return Line::raw("");
            };
            let mut spans = vec![Span::raw(" ".repeat(indent))];
            let (mut text, mut style) = (String::new(), first);
            for (c, s) in row {
                if s != style {
                    spans.push(Span::styled(std::mem::take(&mut text), style));
                    style = s;
                }
                text.push(c);
            }
            spans.push(Span::styled(text, style));
            Line::from(spans)
        })
        .collect()
}

fn field(label: &str, value: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![Span::styled(pad_label(label, FIELD_WIDTH), muted())];
    spans.extend(value);
    Line::from(spans)
}

fn title_lines(title: &str, meta: Vec<String>) -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled(
        clean(title),
        Style::new().fg(Color::White).bold(),
    )];
    let meta: Vec<String> = meta.into_iter().filter(|m| !m.is_empty()).collect();
    lines.push(Line::styled(meta.join(" · "), muted()));
    lines.push(Line::raw(""));
    lines
}

fn homework_detail(app: &App, h: &Homework, width: usize) -> Vec<Line<'static>> {
    let now = app.now;
    let mut lines = title_lines(
        &h.title,
        vec![
            course_label(&h.course_name),
            format!("ID {}", short_id(&h.student_homework_id)),
        ],
    );
    let (label, color) = status_style(h, now);
    lines.push(field(
        "Status",
        vec![Span::styled(label, Style::new().fg(color).bold())],
    ));
    match h.deadline {
        Some(d) => {
            let mut v = vec![Span::raw(d.format("%Y-%m-%d %H:%M").to_string())];
            if h.status == HomeworkStatus::Pending {
                let (left, c) = time_left(d, now);
                let left = match left.strip_prefix('-') {
                    Some(ago) => format!("  overdue by {ago}"),
                    None => format!("  {left} left"),
                };
                v.push(Span::styled(left, Style::new().fg(c)));
            }
            lines.push(field("Deadline", v));
        }
        None if !h.deadline_raw.is_empty() => {
            lines.push(field("Deadline", vec![Span::raw(clean(&h.deadline_raw))]));
        }
        None => {}
    }
    if !h.submit_time.is_empty() {
        lines.push(field("Submitted", vec![Span::raw(clean(&h.submit_time))]));
    }
    if let Some(g) = &h.grade {
        let mut v = vec![Span::styled(clean(g), Style::new().fg(Color::Cyan).bold())];
        let who: Vec<String> = [&h.grader, &h.grade_time]
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| clean(s))
            .collect();
        if !who.is_empty() {
            v.push(Span::styled(format!("  {}", who.join(" · ")), muted()));
        }
        lines.push(field("Grade", v));
    }

    match app.details.get(&h.student_homework_id) {
        None | Some(DetailState::Loading) => {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                format!(
                    "{} Loading description and attachments…",
                    SPINNER[app.spinner % SPINNER.len()]
                ),
                muted(),
            ));
        }
        Some(DetailState::Failed(e)) => {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                format!("Could not load details: {}", clean(e)),
                Style::new().fg(Color::Red),
            ));
        }
        Some(DetailState::Loaded(d)) => {
            if !d.description.trim().is_empty() {
                lines.push(Line::raw(""));
                push_section(
                    &mut lines,
                    "Description",
                    clean_lines(&d.description),
                    width,
                );
            }
            if !h.comment.trim().is_empty() {
                lines.push(Line::raw(""));
                push_section(&mut lines, "Grader comment", clean_lines(&h.comment), width);
            }
            lines.push(Line::raw(""));
            if d.attachments.is_empty() {
                lines.push(Line::styled("No attachments.", muted()));
            } else {
                // Align file names after the widest section label in this list.
                let label_width = d
                    .attachments
                    .iter()
                    .map(|a| UnicodeWidthStr::width(attachment_section(&a.section)))
                    .max()
                    .unwrap_or(0)
                    + LABEL_GAP;
                let rows = d
                    .attachments
                    .iter()
                    .map(|a| {
                        Line::from(vec![
                            Span::styled(
                                pad_label(attachment_section(&a.section), label_width),
                                muted(),
                            ),
                            Span::raw(clean(&a.filename)),
                        ])
                    })
                    .collect();
                push_section(&mut lines, "Attachments", rows, width);
            }
        }
    }
    lines
}

fn announcement_detail(n: &Notification) -> Vec<Line<'static>> {
    let mut lines = title_lines(
        &n.title,
        vec![
            course_label(&n.course_name),
            clean(&n.publisher),
            clean(&n.publish_time),
            if n.read {
                String::new()
            } else {
                "Unread".into()
            },
        ],
    );
    if n.content.trim().is_empty() {
        lines.push(Line::styled("(No body)", muted()));
    } else {
        lines.extend(clean_lines(&n.content));
    }
    lines
}

fn file_detail(x: &CourseFile, width: usize) -> Vec<Line<'static>> {
    let mut lines = title_lines(
        &x.title,
        vec![
            course_label(&x.course_name),
            format!("ID {}", short_id(&x.id)),
        ],
    );
    lines.push(field("Type", vec![Span::raw(clean(&x.file_type))]));
    lines.push(field("Size", vec![Span::raw(clean(&x.size))]));
    lines.push(field("Uploaded", vec![Span::raw(clean(&x.upload_time))]));
    if !x.description.trim().is_empty() {
        lines.push(Line::raw(""));
        push_section(
            &mut lines,
            "Description",
            clean_lines(&x.description),
            width,
        );
    }
    lines
}

fn course_detail(app: &App, c: &Course, width: usize) -> Vec<Line<'static>> {
    let mut lines = title_lines(
        &c.name,
        vec![clean(&c.teacher), format!("ID {}", short_id(&c.id))],
    );
    let now = app.now;
    let mut pending: Vec<&Homework> = app
        .homework
        .data
        .iter()
        .filter(|h| h.course_id == c.id && HomeworkFilter::Pending.matches(h, now))
        .collect();
    pending.sort_by_key(|h| h.deadline);
    let title = format!("Pending homework ({})", pending.len());
    let mut rows = Vec::new();
    if pending.is_empty() {
        rows.push(Line::styled("Nothing due.", muted()));
    }
    let (due_fmt, _) = deadline_format(now, pending.iter().filter_map(|h| h.deadline));
    for h in pending {
        let (due, left) = match h.deadline {
            Some(d) => {
                let (t, col) = time_left(d, now);
                (
                    d.format(due_fmt).to_string(),
                    Span::styled(format!("  {t}"), Style::new().fg(col)),
                )
            }
            None => (String::from("—"), Span::raw("")),
        };
        rows.push(Line::from(vec![
            Span::styled(format!("{due}  "), muted()),
            Span::raw(clean(&h.title)),
            left,
        ]));
    }
    push_section(&mut lines, &title, rows, width);

    let mut notes: Vec<&Notification> = app
        .announcements
        .data
        .iter()
        .filter(|n| n.course_id == c.id)
        .collect();
    notes.sort_by(|a, b| b.publish_time.cmp(&a.publish_time));
    let mut rows = Vec::new();
    if notes.is_empty() {
        rows.push(Line::styled("None.", muted()));
    }
    for n in notes.iter().take(5) {
        let dot = if n.read {
            Span::raw("  ")
        } else {
            Span::styled("● ", Style::new().fg(ACCENT))
        };
        rows.push(Line::from(vec![
            Span::styled(format!("{}  ", date_only(&n.publish_time)), muted()),
            dot,
            Span::raw(clean(&n.title)),
        ]));
    }
    lines.push(Line::raw(""));
    push_section(&mut lines, "Recent announcements", rows, width);
    lines.push(Line::raw(""));
    lines.push(field(
        "Files",
        vec![Span::raw(app.file_count(Some(&c.id)).to_string())],
    ));
    lines
}

// ---------- Session screen ----------

fn draw_session(f: &mut Frame, app: &App, area: Rect) {
    let spin = SPINNER[app.spinner % SPINNER.len()];
    let (title, lines): (&str, Vec<Line<'static>>) = match &app.session {
        Session::Checking => (
            " Connecting ",
            vec![Line::from(format!(
                "{spin} Checking the saved thu-learn session…"
            ))],
        ),
        Session::LoggedOut(reason) => (
            " Not logged in ",
            vec![
                Line::styled(clean(reason), Style::new().fg(Color::Yellow)),
                Line::raw(""),
                Line::from(vec![
                    Span::raw("Run "),
                    Span::styled("thu-learn login", Style::new().fg(ACCENT).bold()),
                    Span::raw(" in a shell, then press "),
                    Span::styled("r", Style::new().fg(ACCENT).bold()),
                    Span::raw(" to retry."),
                ]),
            ],
        ),
        Session::Ready => return,
    };
    let width = area.width.saturating_sub(4).min(72);
    let p = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: true });
    // Two border cells plus two padding cells on each axis.
    let height = p.line_count(width.saturating_sub(4)) as u16 + 4;
    let rect = centered(area, width, height.min(area.height));
    let p = p.block(
        block(Line::styled(title, Style::new().fg(ACCENT).bold()))
            .padding(ratatui::widgets::Padding::uniform(1)),
    );
    f.render_widget(p, rect);
}

// ---------- Overlays ----------

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

fn popup(f: &mut Frame, width: u16, height: u16, title: &str) -> (Rect, Block<'static>) {
    let area = centered(f.area(), width, height);
    f.render_widget(Clear, area);
    let b = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title(Line::styled(
            format!(" {title} "),
            Style::new().fg(Color::White).bold(),
        ));
    (area, b)
}

const HELP: [(&str, &[(&str, &str)]); 5] = [
    (
        "Navigate",
        &[
            ("1-4 Tab ←→", "switch tabs"),
            ("↑↓ j k", "move selection"),
            ("gg G", "top, bottom"),
            ("⏎", "open details"),
            ("J K", "scroll details"),
            ("n p", "next/previous in details"),
        ],
    ),
    (
        "Filter",
        &[
            ("/", "search the current list"),
            ("c", "choose a course"),
            ("f", "cycle status / toggle unread"),
            ("Esc", "clear search, then course"),
        ],
    ),
    (
        "Select files",
        &[
            ("Space", "toggle and move down"),
            ("v V", "visual select / unselect"),
            ("C-a C-r", "select all / invert"),
            ("Esc", "apply visual, then clear"),
            ("d", "download the selected files"),
        ],
    ),
    (
        "Act",
        &[
            ("d", "download attachments or file"),
            ("s", "submit homework"),
            ("D", "change download folder"),
            ("o", "open the last download"),
            ("r", "refresh from Learn"),
        ],
    ),
    ("App", &[("?", "toggle help"), ("q C-c", "quit")]),
];

fn help_section(name: &str, keys: &[(&str, &str)]) -> Vec<Line<'static>> {
    let mut lines = vec![heading(name)];
    for (k, d) in keys {
        lines.push(Line::from(vec![
            Span::styled(format!("  {k:<15}"), Style::new().fg(ACCENT).bold()),
            Span::raw(d.to_string()),
        ]));
    }
    lines.push(Line::raw(""));
    lines
}

fn draw_help(f: &mut Frame, app: &App) {
    let two_columns = f.area().width >= 100;
    let mut left: Vec<Line<'static>> = Vec::new();
    let mut right: Vec<Line<'static>> = Vec::new();
    for (i, (name, keys)) in HELP.iter().enumerate() {
        let target = if two_columns && i >= 2 {
            &mut right
        } else {
            &mut left
        };
        target.extend(help_section(name, keys));
    }
    let footer = vec![
        Line::from(vec![
            Span::styled("Downloads  ", muted()),
            Span::raw(app.download_dir.display().to_string()),
        ]),
        Line::styled(
            "Scripts and agents: use the subcommands, e.g. thu-learn hw --json",
            muted(),
        ),
    ];
    let body_h = left.len().max(right.len()) as u16;
    let width = if two_columns { 96 } else { 60 };
    let height = body_h + footer.len() as u16 + 2;
    let (area, b) = popup(f, width, height, "Keys");
    let inner = b.inner(area).inner(Margin::new(1, 0));
    f.render_widget(b, area);
    let [body, foot] =
        Layout::vertical([Constraint::Length(body_h), Constraint::Fill(1)]).areas(inner);
    if two_columns {
        let [l, r] = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).areas(body);
        f.render_widget(Paragraph::new(Text::from(left)), l);
        f.render_widget(Paragraph::new(Text::from(right)), r);
    } else {
        f.render_widget(Paragraph::new(Text::from(left)), body);
    }
    f.render_widget(
        Paragraph::new(Text::from(footer)).wrap(Wrap { trim: true }),
        foot,
    );
}

fn draw_course_picker(f: &mut Frame, app: &mut App, selected: usize) {
    let mut items = vec![ListItem::new(Line::from(vec![Span::raw("All courses")]))];
    for c in &app.courses.current {
        let pending = app.pending_count(Some(&c.id));
        let mut spans = vec![Span::raw(clean(&c.name))];
        if pending > 0 {
            spans.push(Span::styled(
                format!("  {pending} pending"),
                Style::new().fg(Color::Yellow),
            ));
        }
        let unread = app.unread_count(Some(&c.id));
        if unread > 0 {
            spans.push(Span::styled(
                format!("  {unread} unread"),
                Style::new().fg(ACCENT),
            ));
        }
        items.push(ListItem::new(Line::from(spans)));
    }
    let height = (items.len() as u16 + 2)
        .min(f.area().height.saturating_sub(4))
        .max(5);
    let (area, b) = popup(f, 60, height, "Course");
    app.hits.courses = b.inner(area);
    let list = List::new(items)
        .block(b)
        .highlight_style(Style::new().bg(Color::DarkGray).fg(Color::White).bold())
        .highlight_symbol("▌ ");
    let mut state = ListState::default().with_selected(Some(selected));
    f.render_stateful_widget(list, area, &mut state);
    app.hits.courses_offset = state.offset();
}

fn input_line(f: &mut Frame, area: Rect, label: &str, input: &super::input::Input, active: bool) {
    let label_w = 10u16;
    let [l, field_area] =
        Layout::horizontal([Constraint::Length(label_w), Constraint::Fill(1)]).areas(area);
    let label_style = if active {
        Style::new().fg(ACCENT).bold()
    } else {
        muted()
    };
    f.render_widget(
        Paragraph::new(Span::styled(label.to_string(), label_style)),
        l,
    );
    let (visible, cursor) = input.view(field_area.width.saturating_sub(1) as usize);
    let style = if active {
        Style::new().fg(Color::White).bg(Color::Indexed(236))
    } else {
        Style::new().bg(Color::Indexed(235))
    };
    f.render_widget(Paragraph::new(visible).style(style), field_area);
    if active {
        f.set_cursor_position((field_area.x + cursor as u16, field_area.y));
    }
}

fn draw_submit(f: &mut Frame, submit: &mut SubmitState) {
    let ctx = picker_view::Context {
        course: &submit.course,
        title: &submit.title,
    };
    picker_view::draw(f, f.area(), &mut submit.picker, &ctx);
}

/// A confirmation in yazi's style: centred title, body, and `[Y]es`/`(N)o`.
/// Returns the button areas.
fn draw_confirm(f: &mut Frame, title: &str, lines: &[String]) -> (Rect, Rect) {
    let width = f.area().width.saturating_sub(4).min(70);
    let text: Vec<Line<'static>> = lines.iter().map(|l| Line::raw(clean(l))).collect();
    let body = Paragraph::new(Text::from(text)).wrap(Wrap { trim: false });
    let inner_w = width.saturating_sub(4);
    let height = body.line_count(inner_w) as u16 + 6;
    let area = centered(f.area(), width, height);
    f.render_widget(Clear, area);
    let b = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Blue))
        .title(Line::styled(format!(" {title} "), Style::new().fg(Color::White).bold()).centered());
    let inner = b.inner(area).inner(Margin::new(1, 1));
    f.render_widget(b, area);
    let [body_area, _, buttons] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    f.render_widget(body, body_area);
    let [yes, no] =
        Layout::horizontal([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)]).areas(buttons);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  [", muted()),
            Span::styled("Y", Style::new().fg(ACCENT).bold()),
            Span::styled("]es  ", muted()),
        ]))
        .centered(),
        yes,
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  (", muted()),
            Span::styled("N", Style::new().fg(ACCENT).bold()),
            Span::styled(")o  ", muted()),
        ]))
        .centered(),
        no,
    );
    (yes, no)
}

fn draw_download_dir(f: &mut Frame, input: &super::input::Input, error: Option<&str>) {
    let width = f.area().width.saturating_sub(4).min(80);
    let (area, b) = popup(f, width, 7, "Download folder");
    let inner = b.inner(area).inner(Margin::new(1, 1));
    f.render_widget(b, area);
    let [field_area, msg] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);
    input_line(f, field_area, "Folder", input, true);
    let line = match error {
        Some(e) => Line::styled(e.to_string(), Style::new().fg(Color::Red)),
        None => Line::styled("Created on first download if missing.", muted()),
    };
    f.render_widget(Paragraph::new(line), msg);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::tests::{files_app, local_dt, ready_app};
    use crate::tui::app::Effect;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            let mut skip = 0;
            for x in 0..buffer.area.width {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let symbol = buffer[(x, y)].symbol();
                skip = UnicodeWidthStr::width(symbol).saturating_sub(1);
                out.push_str(symbol);
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn time_left_is_compact_and_colored() {
        let now = local_dt(2026, 6, 1, 12, 0);
        assert_eq!(
            time_left(local_dt(2026, 6, 1, 12, 30), now),
            ("30m".into(), Color::LightRed)
        );
        assert_eq!(
            time_left(local_dt(2026, 6, 1, 17, 15), now),
            ("5h".into(), Color::LightRed)
        );
        assert_eq!(
            time_left(local_dt(2026, 6, 3, 15, 0), now),
            ("2d 3h".into(), Color::Yellow)
        );
        assert_eq!(
            time_left(local_dt(2026, 6, 9, 12, 0), now),
            ("8d".into(), Color::Green)
        );
        assert_eq!(
            time_left(local_dt(2026, 5, 29, 12, 0), now),
            ("-3d".into(), Color::Red)
        );
    }

    #[test]
    fn narrow_key_hints_keep_help_and_quit() {
        let hints = [
            ("↑↓", "move"),
            ("⏎", "open"),
            ("/", "search"),
            ("r", "refresh"),
            ("?", "help"),
            ("q", "quit"),
        ];
        let (spans, _) = fit_hints(&hints, 40);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("? help"));
        assert!(text.contains("q quit"));
        assert!(text.contains("↑↓ move"));
        assert!(!text.contains("refresh"));
        assert!(UnicodeWidthStr::width(text.trim_end()) <= 40);
    }

    #[test]
    fn fit_truncates_by_display_width() {
        assert_eq!(fit("short", 10), "short");
        assert_eq!(fit("数学分析作业", 7), "数学分…");
        assert_eq!(fit("abcdef", 4), "abc…");
    }

    #[test]
    fn wide_layout_shows_list_and_details() {
        let mut app = ready_app();
        let screen = render(&mut app, 140, 24);
        assert!(screen.contains("thu-learn"));
        assert!(screen.contains("Pending homework"));
        assert!(screen.contains("Socket report"));
        assert!(screen.contains("Heap lab"));
        assert!(!screen.contains("Old quiz"));
        // Details for the first row, with the short ID for CLI hand-off.
        assert!(screen.contains(&format!("ID {}", short_id("soon"))));
        assert!(screen.contains("2025-2026-2"));
    }

    #[test]
    fn narrow_layout_stacks_panes() {
        let mut app = ready_app();
        let screen = render(&mut app, 80, 30);
        assert!(screen.contains("Socket report"));
        assert!(screen.contains("Details"));
    }

    #[test]
    fn detail_labels_keep_a_gap_before_values() {
        assert_eq!(pad_label("Status", FIELD_WIDTH), "Status       ");
        assert_eq!(pad_label("Submitted", FIELD_WIDTH), "Submitted    ");
        assert_eq!(pad_label("Longer than width", 10), "Longer than width    ");
        assert_eq!(pad_label("作业附件", 10), "作业附件    ");

        let mut app = ready_app();
        let attachment = |section: &str, filename: &str| crate::models::HomeworkAttachment {
            section: section.into(),
            filename: filename.into(),
            download_path: "https://learn.tsinghua.edu.cn/x".into(),
        };
        app.details.insert(
            "soon".into(),
            DetailState::Loaded(crate::tui::app::HomeworkDetail {
                description: "二选一完成".into(),
                // "Attachment" is the parser's fallback when no Learn section label precedes a file.
                attachments: vec![
                    attachment("作业附件", "spec.pdf"),
                    attachment("Attachment", "L8 slides.pptx"),
                ],
            }),
        );
        let h = app
            .homework
            .data
            .iter()
            .find(|h| h.student_homework_id == "soon")
            .unwrap()
            .clone();
        let text: Vec<String> = homework_detail(&app, &h, 60)
            .iter()
            .map(|l| l.to_string())
            .collect();
        assert!(
            text.contains(&"Status       Pending".to_string()),
            "{text:#?}"
        );
        // Headings are followed by a blank line, and their bodies are indented.
        let start = text.iter().position(|l| l == "▍Description").unwrap();
        assert_eq!(
            text[start..start + 8],
            [
                "▍Description",
                "",
                "  二选一完成",
                "",
                "▍Attachments",
                "",
                "  Homework      spec.pdf",
                "  Attachment    L8 slides.pptx",
            ],
            "{text:#?}"
        );
    }

    fn row_texts(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn wrapped_section_rows_keep_their_indent() {
        // Words move to the next row whole; every row keeps the indent.
        let rows = wrap_indented(Line::raw("submit the report as a pdf"), 2, 14);
        assert_eq!(row_texts(&rows), ["  submit the", "  report as a", "  pdf"]);
        // CJK text can break between any two characters.
        let rows = wrap_indented(Line::raw("二选一完成课后作业"), 2, 12);
        assert_eq!(row_texts(&rows), ["  二选一完成", "  课后作业"]);
        // A word longer than a row is split.
        let rows = wrap_indented(Line::raw("abcdefghij"), 2, 6);
        assert_eq!(row_texts(&rows), ["  abcd", "  efgh", "  ij"]);
        // Empty lines stay empty instead of holding only the indent.
        assert_eq!(row_texts(&wrap_indented(Line::raw(""), 2, 10)), [""]);
        // A pane narrower than the indent still makes progress.
        assert_eq!(wrap_indented(Line::raw("ab"), 2, 0).len(), 2);
    }

    #[test]
    fn wrapped_section_rows_keep_span_styles() {
        let line = Line::from(vec![
            Span::styled("Homework    ", muted()),
            Span::raw("spec.pdf"),
        ]);
        let rows = wrap_indented(line, 2, 40);
        assert_eq!(rows.len(), 1);
        let spans = &rows[0].spans;
        assert_eq!(spans[0].content, "  ");
        assert_eq!(spans[1].content, "Homework    ");
        assert_eq!(spans[1].style, muted());
        assert_eq!(spans[2].content, "spec.pdf");
        assert_eq!(spans[2].style, Style::new());
    }

    #[test]
    fn logged_out_screen_explains_login() {
        let mut app = App::new(".".into(), local_dt(2026, 6, 1, 12, 0));
        app.session = Session::LoggedOut("No credentials are saved.".into());
        let screen = render(&mut app, 100, 24);
        assert!(screen.contains("Not logged in"));
        assert!(screen.contains("No credentials are saved."));
        assert!(screen.contains("thu-learn login"));
    }

    #[test]
    fn server_text_cannot_inject_escape_sequences() {
        let mut app = ready_app();
        app.homework.data[1].title = "Evil\u{1b}]52;c;Zm9v\u{7} title\u{1b}[2J".into();
        let screen = render(&mut app, 140, 24);
        assert!(!screen.contains('\u{1b}'));
        assert!(screen.contains("Evil title"));
    }

    #[test]
    fn help_overlay_lists_actions() {
        let mut app = ready_app();
        app.overlay = Some(Overlay::Help);
        let screen = render(&mut app, 120, 40);
        assert!(screen.contains("submit homework"));
        assert!(screen.contains("visual select / unselect"));
        assert!(screen.contains("/tmp/downloads"));
    }

    #[test]
    fn selected_files_get_yazi_markers_and_a_count() {
        let mut app = files_app();
        app.selected_files = ["f4", "f2"].into_iter().map(String::from).collect();
        let screen = render(&mut app, 120, 20);
        assert!(screen.contains("Files · 2 selected"), "{screen}");
        assert!(screen.contains("download selected"), "{screen}");

        // Rows start below the top border (y = 1) and the header (y = 2):
        // f4, f3, f2, f1. Markers sit on the left border.
        let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let marker_rows: Vec<u16> = (0..20)
            .filter(|&y| terminal.backend().buffer()[(0, y)].bg == Color::LightYellow)
            .collect();
        assert_eq!(marker_rows, [3, 5]);

        app.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        let screen = render(&mut app, 120, 20);
        assert!(screen.contains("2 selected  SEL"), "{screen}");
        assert!(screen.contains("Esc apply"), "{screen}");
    }

    fn mouse(kind: MouseEventKind, (column, row): (u16, u16)) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn click(at: (u16, u16)) -> MouseEvent {
        mouse(MouseEventKind::Down(MouseButton::Left), at)
    }

    /// Cell position of `needle` on screen; the fixtures use narrow characters only.
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

    #[test]
    fn mouse_switches_tabs_selects_rows_and_scrolls() {
        let mut app = ready_app();
        app.homework_filter = HomeworkFilter::All;
        let screen = render(&mut app, 120, 20);
        // Rows start below the list's top border and header: y = 3 is the first.
        app.on_mouse(click((10, 4)));
        assert_eq!(app.selected_index(Tab::Homework), Some(1));
        app.on_mouse(mouse(MouseEventKind::ScrollDown, (10, 10)));
        assert_eq!(app.selected_index(Tab::Homework), Some(2));
        // Below the last row nothing is selected.
        app.on_mouse(click((10, 15)));
        assert_eq!(app.selected_index(Tab::Homework), Some(2));

        // The wheel over the details scrolls them, not the list.
        render(&mut app, 120, 20);
        app.detail_max_scroll = 10;
        app.on_mouse(mouse(MouseEventKind::ScrollDown, (100, 10)));
        assert_eq!(app.detail_scroll, 3);
        assert_eq!(app.selected_index(Tab::Homework), Some(2));

        // A right click opens the row, like Enter.
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Right), (10, 3)));
        assert!(app.zoomed);
        assert_eq!(app.selected_index(Tab::Homework), Some(0));
        app.zoomed = false;

        app.on_mouse(click(find(&screen, "3 Files")));
        assert_eq!(app.tab, Tab::Files);
        render(&mut app, 120, 20);
        // The divider between tabs is not a tab.
        let (x, y) = find(&screen, "│ 4 Courses");
        app.on_mouse(click((x, y)));
        assert_eq!(app.tab, Tab::Files);
        app.on_mouse(click((x + 3, y)));
        assert_eq!(app.tab, Tab::Courses);
    }

    #[test]
    fn mouse_clicks_course_picker_rows_and_confirm_buttons() {
        let mut app = ready_app();
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        let screen = render(&mut app, 120, 20);
        app.on_mouse(mouse(MouseEventKind::ScrollDown, (0, 0)));
        assert!(matches!(
            app.overlay,
            Some(Overlay::CoursePicker { selected: 1 })
        ));
        app.on_mouse(click(find(&screen, "Networks")));
        assert!(app.overlay.is_none());
        assert_eq!(app.course_filter.as_deref(), Some("c2"));

        let mut app = files_app();
        app.on_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        let screen = render(&mut app, 120, 20);
        // Clicks elsewhere keep the confirmation open.
        app.on_mouse(click((0, 0)));
        assert!(matches!(app.overlay, Some(Overlay::Confirm { .. })));
        let effects = app.on_mouse(click(find(&screen, "[Y]es")));
        assert!(
            matches!(effects.as_slice(), [Effect::DownloadFiles { .. }]),
            "{effects:?}"
        );
    }

    #[test]
    fn clicking_a_hint_presses_its_key() {
        use crate::models::HomeworkAttachment;
        use crate::tui::app::HomeworkDetail;
        use std::path::PathBuf;

        let mut app = ready_app();
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
        let screen = render(&mut app, 120, 20);
        let effects = app.on_mouse(click(find(&screen, "d download all")));
        assert_eq!(
            effects,
            vec![Effect::DownloadAttachments {
                attachments: vec![attachment],
                dir: PathBuf::from("/tmp/downloads"),
            }]
        );
        // The gap before a hint is not part of it.
        let (x, y) = find(&screen, "s submit a file");
        app.on_mouse(click((x - 1, y)));
        assert!(app.overlay.is_none());
        app.submit_dir = std::env::temp_dir();
        app.on_mouse(click((x, y)));
        assert!(matches!(app.overlay, Some(Overlay::Submit(_))));
        // The picker covers the key bar, so its hints are not clickable there.
        render(&mut app, 120, 20);
        assert!(app.hits.keys.is_empty());
        app.overlay = None;

        // In the key bar, "/ search" starts a search and "Esc clear" ends it.
        let screen = render(&mut app, 120, 20);
        app.on_mouse(click(find(&screen, "/ search")));
        assert!(app.searching);
        let screen = render(&mut app, 120, 20);
        app.on_mouse(click(find(&screen, "Esc clear")));
        assert!(!app.searching);
    }

    #[test]
    fn deadlines_show_the_year_once_a_list_crosses_new_year() {
        use crate::tui::app::tests::homework;

        let row = |screen: &str| -> String {
            let line = screen.lines().find(|l| l.contains("Socket report"));
            line.unwrap().to_string()
        };
        let mut app = ready_app();
        let screen = render(&mut app, 120, 20);
        assert!(row(&screen).contains(" 06-02 08:00"), "{screen}");
        assert!(!row(&screen).contains("2026-"), "{screen}");

        app.homework.data.push(homework(
            "next",
            "c1",
            "Final project",
            Some(local_dt(2027, 1, 3, 23, 59)),
            HomeworkStatus::Pending,
        ));
        let screen = render(&mut app, 120, 20);
        assert!(row(&screen).contains("2026-06-02 08:00"), "{screen}");
        assert!(screen.contains("2027-01-03 23:59"), "{screen}");
    }
}
