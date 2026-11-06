//! Rendering for the picker, following yazi's default layout and theme:
//! a header, parent/current/preview columns in a 1:4:3 ratio, and a status bar.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::editor::Editor;
use super::fs::{self, Category, Entry, Preview};
use super::keymap::{self, Linemode};
use super::{
    Areas, Cmp, Folder, Level, Matcher, Picker, Popup, PreviewContent, CMP_ROWS, HELP_ROWS,
};
use crate::cli::strip_terminal_controls;

/// What the picker is choosing a file for, shown above yazi's header.
pub struct Context<'a> {
    pub course: &'a str,
    pub title: &'a str,
}

const BORDER: Color = Color::Blue;

fn clean(s: &str) -> String {
    strip_terminal_controls(s)
}

fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Truncates by display width, ending with `…`.
fn fit(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > max - 1 {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push('…');
    out
}

/// Keeps the end of `s` (yazi truncates the header path from the left).
fn fit_left(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut tail: Vec<char> = Vec::new();
    let mut used = 0;
    for c in s.chars().rev() {
        let w = c.width().unwrap_or(0);
        if used + w > max - 1 {
            break;
        }
        used += w;
        tail.push(c);
    }
    tail.reverse();
    format!("…{}", tail.into_iter().collect::<String>())
}

fn category_style(e: &Entry) -> Style {
    match fs::category(e) {
        Category::Dir => Style::new().fg(Color::Blue),
        Category::Exec => Style::new().fg(Color::Green),
        Category::Orphan => Style::new().bg(Color::Red),
        Category::Image => Style::new().fg(Color::Yellow),
        Category::Media => Style::new().fg(Color::Magenta),
        Category::Archive => Style::new().fg(Color::Red),
        Category::Document => Style::new().fg(Color::Cyan),
        Category::Plain => Style::new(),
    }
}

/// The name, cut to `max` cells; files keep their extension as `….ext`.
fn fit_name(e: &Entry, name: &str, max: usize) -> (String, bool) {
    if width(name) <= max {
        return (name.to_string(), false);
    }
    if let Some(ext) = e.ext().filter(|_| !e.is_dir()) {
        let tail = format!("….{}", clean(&ext));
        if width(&tail) < max {
            let head = fit(name, max - width(&tail) + 1);
            let head = head.trim_end_matches('…');
            return (format!("{head}{tail}"), true);
        }
    }
    (fit(name, max), true)
}

struct Row<'a> {
    hovered: bool,
    /// Underline instead of reverse, for the cursor of a directory preview.
    underline: bool,
    finder: Option<&'a Matcher>,
    found: Option<(usize, usize)>,
    linemode: String,
}

/// One entry line padded to `w` cells, styled like yazi's `Entity`.
fn entity_line(e: &Entry, w: usize, row: Row) -> Line<'static> {
    let name = clean(&e.name);
    let base = category_style(e);
    let mut tail: Vec<Span<'static>> = Vec::new();
    if let Some((k, n)) = row.found.filter(|(k, _)| *k <= 99) {
        let n = if n >= 100 {
            "99+".to_string()
        } else {
            n.to_string()
        };
        tail.push(Span::styled(
            format!("  [{k}/{n}]"),
            Style::new()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ));
    }
    if let Some(link) = &e.link_to {
        tail.push(Span::styled(
            format!(" -> {}", clean(&link.display().to_string())),
            Style::new().add_modifier(Modifier::ITALIC),
        ));
    }
    let right = if row.linemode.is_empty() {
        " ".to_string()
    } else {
        format!(" {} ", row.linemode)
    };
    // Like yazi, the whole line is cut at the end: the name stays whole and
    // the find tag or link target is shortened, unless the name itself does
    // not fit, in which case it gets the `….ext` ellipsis and no tail.
    let room = w.saturating_sub(1 + width(&right));
    let (shown, truncated) = if width(&name) >= room {
        tail.clear();
        fit_name(e, &name, room)
    } else {
        (name.clone(), false)
    };
    let mut left = room.saturating_sub(width(&shown));
    let mut fitted = Vec::new();
    for span in tail {
        let sw = width(&span.content);
        if sw <= left {
            left -= sw;
            fitted.push(span);
        } else {
            if left > 0 {
                fitted.push(Span::styled(fit(&span.content, left), span.style));
            }
            break;
        }
    }
    let tail = fitted;

    let mut spans = vec![Span::raw(" ")];
    // Highlight the first find match, as far as it is still visible.
    let highlight = row
        .finder
        .and_then(|f| f.find(&name))
        .map(|r| {
            let limit = if truncated {
                shown
                    .trim_end_matches(|c| c != '…')
                    .len()
                    .saturating_sub('…'.len_utf8())
            } else {
                shown.len()
            };
            (r.start.min(limit), r.end.min(limit))
        })
        .filter(|(a, b)| a < b);
    match highlight {
        Some((a, b)) if shown.is_char_boundary(a) && shown.is_char_boundary(b) => {
            spans.push(Span::styled(shown[..a].to_string(), base));
            spans.push(Span::styled(
                shown[a..b].to_string(),
                base.fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD | Modifier::ITALIC | Modifier::UNDERLINED),
            ));
            spans.push(Span::styled(shown[b..].to_string(), base));
        }
        _ => spans.push(Span::styled(shown.clone(), base)),
    }
    spans.extend(tail);
    let used: usize = spans.iter().map(|s| width(&s.content)).sum();
    spans.push(Span::raw(
        " ".repeat(w.saturating_sub(used + width(&right))),
    ));
    spans.push(Span::raw(right));

    let mut line = Line::from(spans);
    if row.hovered {
        line = line.style(base.add_modifier(Modifier::REVERSED));
    } else if row.underline {
        line = line.style(base.add_modifier(Modifier::UNDERLINED));
    }
    line
}

fn linemode_value(p: &Picker, e: &Entry) -> String {
    match p.linemode {
        Linemode::None => String::new(),
        Linemode::Size if e.is_dir() => p
            .counts
            .get(&e.path)
            .map(|n| n.to_string())
            .unwrap_or_default(),
        Linemode::Size => fs::readable_size(e.len),
        Linemode::Permissions => fs::permissions(e).unwrap_or_default(),
        Linemode::Btime => fs::linemode_time(e.btime),
        Linemode::Mtime => fs::linemode_time(e.mtime),
        Linemode::Owner => fs::owner(e),
    }
}

/// Draws the whole picker into `area` (normally the full terminal).
pub fn draw(f: &mut Frame, area: Rect, p: &mut Picker, ctx: &Context) {
    f.render_widget(Clear, area);
    let [banner, header, main, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let [c1, c2, c3] = Layout::horizontal([
        Constraint::Ratio(1, 8),
        Constraint::Ratio(4, 8),
        Constraint::Ratio(3, 8),
    ])
    .areas(main);
    let parent_area = Rect {
        x: c1.x + 1.min(c1.width),
        width: c1.width.saturating_sub(1),
        ..c1
    };
    let current_area = Rect {
        x: c2.x + 1,
        width: c2.width.saturating_sub(2),
        ..c2
    };
    let preview_area = Rect {
        width: c3.width.saturating_sub(1),
        ..c3
    };
    p.set_limits(main.height as usize, preview_area.height as usize);
    p.areas = Areas {
        parent: parent_area,
        current: current_area,
        preview: preview_area,
    };
    p.sync_preview();

    draw_banner(f, banner, ctx);
    draw_header(f, header, p);
    // Rails: the gray separators belong to the current column's padding cells.
    let rail = Style::new().fg(Color::Gray);
    for y in main.y..main.y + main.height {
        if c1.width > 0 && c2.width > 0 {
            f.render_widget(
                Paragraph::new(Span::styled("│", rail)),
                Rect::new(c2.x, y, 1, 1),
            );
        }
        if c3.width > 0 && c2.width > 1 {
            f.render_widget(
                Paragraph::new(Span::styled("│", rail)),
                Rect::new(c2.x + c2.width - 1, y, 1, 1),
            );
        }
    }
    if let Some(parent) = p.parent.clone() {
        draw_folder(f, parent_area, p, &parent, FolderPane::Parent, c1.x);
    }
    let current = p.current.clone();
    draw_folder(f, current_area, p, &current, FolderPane::Current, c2.x);
    draw_preview(f, preview_area, p, c3.x.saturating_sub(1));
    draw_status(f, status, p);

    if !p.pending.is_empty() {
        draw_which(f, area, status, p);
    }
    let mut popup = p.popup.take();
    match &mut popup {
        Some(Popup::Input {
            kind,
            editor,
            cmp,
            invalid,
        }) => draw_input(f, area, kind.title(), editor, cmp.as_ref(), *invalid),
        Some(Popup::Help {
            editor,
            scroll,
            spot,
        }) => {
            if let Some(s) = spot {
                let rows = p.spot_rows(s.size.as_deref());
                draw_spot(f, area, &rows, s.row, false);
            }
            let items = keymap::help_items(spot.is_some(), &editor.value());
            let rows = help_rect(area).height.saturating_sub(4) as usize;
            p.help_limit = rows.max(1);
            // Keep the cursor visible for the height actually drawn.
            scroll.arrow(
                super::scroll::Step::Offset(0),
                items.len(),
                p.help_limit,
                p.help_limit / 2,
            );
            draw_help(f, area, editor, &items, scroll.cursor, scroll.offset);
        }
        Some(Popup::Spot {
            row,
            size,
            pending_c,
        }) => {
            let rows = p.spot_rows(size.as_deref());
            draw_spot(f, area, &rows, *row, *pending_c);
        }
        None => {}
    }
    p.popup = popup;
    // Notices stay between the header and the status bar.
    draw_notices(f, main, p);
}

fn draw_banner(f: &mut Frame, area: Rect, ctx: &Context) {
    let hint = " ⏎ choose  Space select  ~ help  q cancel ";
    let course = fit(&clean(ctx.course), 24);
    let title_w = (area.width as usize).saturating_sub(width(&course) + width(hint) + 14);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " Submit · ",
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(course, Style::new().fg(Color::Cyan)),
            Span::styled(" | ", Style::new().fg(Color::DarkGray)),
            Span::styled(
                fit(&clean(ctx.title), title_w),
                Style::new().add_modifier(Modifier::BOLD),
            ),
        ])),
        area,
    );
    let hint_area = Rect {
        x: area.x + area.width.saturating_sub(width(hint) as u16),
        width: (width(hint) as u16).min(area.width),
        ..area
    };
    f.render_widget(
        Paragraph::new(Span::styled(hint, Style::new().fg(Color::DarkGray))),
        hint_area,
    );
}

fn draw_header(f: &mut Frame, area: Rect, p: &Picker) {
    let mut flags = Vec::new();
    if let Some(filter) = &p.current.filter {
        flags.push(format!("filter: {}", filter.raw));
    }
    if let Some(finder) = &p.finder {
        flags.push(format!("find: {}", finder.raw));
    }
    let mut left = clean(&fs::tilde(&p.cwd));
    if !flags.is_empty() {
        left.push_str(&format!(" ({})", clean(&flags.join(", "))));
    }
    let right = if p.selected.is_empty() {
        String::new()
    } else {
        format!(" {} ", p.selected.len())
    };
    let right_w = if right.is_empty() {
        0
    } else {
        width(&right) + 1
    };
    let left = fit_left(&left, (area.width as usize).saturating_sub(right_w));
    f.render_widget(
        Paragraph::new(Span::styled(left, Style::new().fg(Color::Cyan))),
        area,
    );
    if !right.is_empty() {
        let r = Rect {
            x: area.x + area.width.saturating_sub(right_w as u16),
            width: right_w as u16,
            ..area
        };
        f.render_widget(
            Paragraph::new(Span::styled(
                right,
                Style::new().fg(Color::Black).bg(Color::Yellow),
            )),
            r,
        );
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FolderPane {
    Parent,
    Current,
}

/// Draws a folder's visible window; `marker_x` is where selection markers go.
fn draw_folder(
    f: &mut Frame,
    area: Rect,
    p: &Picker,
    folder: &Folder,
    pane: FolderPane,
    marker_x: u16,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if folder.items.is_empty() {
        if pane == FolderPane::Current {
            let text = if folder.filter.is_some() {
                "No filter results".to_string()
            } else if let Some(e) = &folder.error {
                format!("Error: {}", clean(e))
            } else {
                "No items".to_string()
            };
            draw_centered(f, area, &text);
        }
        return;
    }
    let found = (pane == FolderPane::Current)
        .then(|| p.found_position())
        .flatten();
    let rows = area.height as usize;
    for (i, e) in folder
        .items
        .iter()
        .enumerate()
        .skip(folder.scroll.offset)
        .take(rows)
    {
        let y = area.y + (i - folder.scroll.offset) as u16;
        let hovered = i == folder.scroll.cursor;
        let row = Row {
            hovered,
            underline: false,
            finder: (pane == FolderPane::Current)
                .then_some(p.finder.as_ref())
                .flatten(),
            found: if hovered { found } else { None },
            linemode: if pane == FolderPane::Current {
                clean(&linemode_value(p, e))
            } else {
                String::new()
            },
        };
        let line = entity_line(e, area.width as usize, row);
        f.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
        if p.selected.contains(&e.path) {
            draw_marker(f, marker_x, y);
        }
    }
}

/// yazi's selection marker; the main Files list draws it too.
pub fn draw_marker(f: &mut Frame, x: u16, y: u16) {
    f.render_widget(
        Paragraph::new(Span::styled(
            "│",
            Style::new().fg(Color::LightYellow).bg(Color::LightYellow),
        )),
        Rect::new(x, y, 1, 1),
    );
}

/// yazi draws empty-folder messages centred on the pane's first row.
fn draw_centered(f: &mut Frame, area: Rect, text: &str) {
    f.render_widget(
        Paragraph::new(Line::from(text.to_string()).centered()).wrap(Wrap { trim: true }),
        area,
    );
}

fn draw_preview(f: &mut Frame, area: Rect, p: &Picker, marker_x: u16) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(preview) = &p.preview else {
        return;
    };
    let w = area.width as usize;
    match &preview.content {
        PreviewContent::Dir(folder) => {
            if folder.items.is_empty() {
                let text = match &folder.error {
                    Some(e) => format!("Error: {}", clean(e)),
                    None => "No items".into(),
                };
                draw_centered(f, area, &text);
                return;
            }
            for (i, e) in folder
                .items
                .iter()
                .enumerate()
                .skip(preview.skip)
                .take(area.height as usize)
            {
                let y = area.y + (i - preview.skip) as u16;
                let row = Row {
                    hovered: false,
                    underline: i == folder.scroll.cursor,
                    finder: None,
                    found: None,
                    linemode: String::new(),
                };
                f.render_widget(
                    Paragraph::new(entity_line(e, w, row)),
                    Rect::new(area.x, y, area.width, 1),
                );
                if p.selected.contains(&e.path) {
                    draw_marker(f, marker_x, y);
                }
            }
        }
        PreviewContent::File(Preview::Text(lines)) => {
            let shown: Vec<Line> = lines
                .iter()
                .skip(preview.skip)
                .take(area.height as usize)
                .map(|l| Line::raw(fit(&clean(l), w)))
                .collect();
            f.render_widget(Paragraph::new(shown), area);
        }
        PreviewContent::File(other) => {
            let Some(e) = p.hovered() else {
                return;
            };
            let what = match other {
                Preview::Binary => "binary file".to_string(),
                Preview::Special(kind) => (*kind).to_string(),
                Preview::Unreadable(err) => format!("cannot read: {}", clean(err)),
                Preview::Text(_) => String::new(),
            };
            let muted = Style::new().fg(Color::DarkGray);
            let lines = vec![
                Line::styled(
                    fit(&clean(&e.name), w),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Line::styled(
                    fit(
                        &format!(
                            "{} · {}",
                            fs::readable_size(e.len),
                            fs::linemode_time(e.mtime)
                        ),
                        w,
                    ),
                    muted,
                ),
                Line::raw(""),
                Line::styled(fit(&what, w), muted),
            ];
            f.render_widget(Paragraph::new(lines), area);
        }
    }
}

fn draw_status(f: &mut Frame, area: Rect, p: &Picker) {
    let main = Style::new().bg(Color::Blue).add_modifier(Modifier::BOLD);
    let alt = Style::new().fg(Color::Blue).bg(Color::Gray);
    let hovered = p.hovered();
    let mut left = vec![
        Span::styled(" NOR ", main),
        Span::styled(
            format!(" {} ", fs::readable_size(hovered.map_or(0, |e| e.len))),
            alt,
        ),
    ];
    if let Some(e) = hovered {
        left.push(Span::raw(format!(" {}", clean(&e.name))));
    }

    let len = p.current.items.len();
    let cursor = p.current.scroll.cursor;
    let percent = if cursor == 0 || len == 0 {
        0
    } else {
        (cursor + 1) * 100 / len
    };
    let percent = match percent {
        0 => " Top ".to_string(),
        100 => " Bot ".to_string(),
        n => format!(" {n:2}% "),
    };
    let position = format!(" {:2}/{:<2} ", (cursor + 1).min(len), len);
    let mut right: Vec<Span> = Vec::new();
    if let Some(perm) = hovered.and_then(fs::permissions) {
        for (i, ch) in perm.chars().enumerate() {
            let color = match ch {
                '-' | '?' => Color::DarkGray,
                _ if i == 0 => Color::Green,
                'r' => Color::Yellow,
                'w' => Color::Red,
                _ => Color::Cyan,
            };
            right.push(Span::styled(ch.to_string(), Style::new().fg(color)));
        }
        right.push(Span::raw(" "));
    }
    right.push(Span::styled(percent, alt));
    right.push(Span::styled(position, main));
    let right_w: usize = right.iter().map(|s| width(&s.content)).sum();

    let left_w = (area.width as usize).saturating_sub(right_w);
    f.render_widget(
        Paragraph::new(Line::from(left)),
        Rect {
            width: left_w as u16,
            ..area
        },
    );
    f.render_widget(
        Paragraph::new(Line::from(right)),
        Rect {
            x: area.x + left_w as u16,
            width: right_w as u16,
            ..area
        },
    );
}

fn draw_which(f: &mut Frame, area: Rect, status: Rect, p: &Picker) {
    let cands = keymap::candidates(&p.pending);
    if cands.is_empty() {
        return;
    }
    let cols = 3usize;
    let height = (cands.len().div_ceil(cols) as u16 + 2).min(area.height);
    let rect = Rect {
        x: area.x + 1,
        y: status.y.saturating_sub(height + 1),
        width: area.width.saturating_sub(2),
        height,
    };
    f.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(BORDER));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let chunks = Layout::horizontal([
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
    ])
    .split(inner);
    for (i, b) in cands.iter().enumerate() {
        let (row, col) = (i / cols, i % cols);
        if row as u16 >= inner.height {
            break;
        }
        let rest = keymap::chord_label(&b.on[p.pending.len()..]);
        let chunk = chunks[col];
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!(" {rest}"), Style::new().fg(Color::LightCyan)),
                Span::styled("  ", Style::new().fg(Color::DarkGray)),
                Span::styled(b.desc, Style::new().fg(Color::LightMagenta)),
            ])),
            Rect::new(chunk.x, chunk.y + row as u16, chunk.width, 1),
        );
    }
}

/// yazi's input position: top-center, two rows into the file area, 80x3.
fn input_rect(area: Rect) -> Rect {
    let w = area.width.min(80);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: (area.y + 3).min(area.y + area.height.saturating_sub(3)),
        width: w,
        height: 3.min(area.height),
    }
}

/// Draws the editor's visible text and places the terminal cursor; the
/// runtime switches its shape between a bar (insert) and a block (normal).
fn draw_editor_line(f: &mut Frame, area: Rect, editor: &mut Editor) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (visible, cursor_x) = editor.view(area.width.saturating_sub(1) as usize);
    f.render_widget(Paragraph::new(visible), area);
    let x = (area.x + cursor_x as u16).min(area.x + area.width - 1);
    f.set_cursor_position((x, area.y));
}

fn draw_input(
    f: &mut Frame,
    area: Rect,
    title: &str,
    editor: &mut Editor,
    cmp: Option<&Cmp>,
    invalid: bool,
) {
    let rect = input_rect(area);
    f.render_widget(Clear, rect);
    let mut title_spans = vec![Span::raw(title.to_string())];
    if invalid {
        title_spans.push(Span::styled(
            " (invalid pattern)",
            Style::new().fg(Color::Red),
        ));
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(BORDER))
        .title(Line::from(title_spans));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    draw_editor_line(f, inner, editor);

    let Some(cmp) = cmp.filter(|c| !c.items.is_empty()) else {
        return;
    };
    let rows = cmp.items.len().min(CMP_ROWS);
    let list = Rect {
        x: rect.x + 1,
        y: rect.y + 2,
        width: rect.width.saturating_sub(2),
        height: (rows as u16 + 2).min(area.height.saturating_sub(rect.y + 2 - area.y)),
    };
    f.render_widget(Clear, list);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(BORDER));
    let inner = block.inner(list);
    f.render_widget(block, list);
    for (i, (name, is_dir)) in cmp
        .items
        .iter()
        .enumerate()
        .skip(cmp.scroll.offset)
        .take(inner.height as usize)
    {
        let text = format!(" {}{}", clean(name), if *is_dir { "/" } else { "" });
        let style = if i == cmp.scroll.cursor {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new()
        };
        let y = inner.y + (i - cmp.scroll.offset) as u16;
        let text = format!(
            "{:<w$}",
            fit(&text, inner.width as usize),
            w = inner.width as usize
        );
        f.render_widget(
            Paragraph::new(Span::styled(text, style)),
            Rect::new(inner.x, y, inner.width, 1),
        );
    }
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn help_rect(area: Rect) -> Rect {
    centered(area, 80, HELP_ROWS as u16 + 4)
}

fn draw_help(
    f: &mut Frame,
    area: Rect,
    editor: &mut Editor,
    items: &[keymap::HelpItem],
    cursor: usize,
    offset: usize,
) {
    let rect = help_rect(area);
    f.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(BORDER))
        .title(Line::from("Help").centered());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    if inner.height < 3 {
        return;
    }
    let filter = Rect { height: 1, ..inner };
    draw_editor_line(f, filter, editor);
    f.render_widget(
        Paragraph::new(Span::styled(
            "─".repeat(inner.width as usize),
            Style::new().fg(Color::DarkGray),
        )),
        Rect {
            y: inner.y + 1,
            height: 1,
            ..inner
        },
    );
    let list = Rect {
        y: inner.y + 2,
        height: inner.height - 2,
        ..inner
    };
    for (i, b) in items
        .iter()
        .enumerate()
        .skip(offset)
        .take(list.height as usize)
    {
        let keys = format!("{:<20}", fit(&b.keys, 19));
        let text = format!("{keys}{}", b.desc);
        let text = format!(
            "{:<w$}",
            fit(&text, list.width as usize),
            w = list.width as usize
        );
        let style = if i == cursor {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new()
        };
        f.render_widget(
            Paragraph::new(Span::styled(text, style)),
            Rect::new(list.x, list.y + (i - offset) as u16, list.width, 1),
        );
    }
}

fn draw_spot(
    f: &mut Frame,
    area: Rect,
    rows: &[(String, Option<String>)],
    selected: usize,
    pending_c: bool,
) {
    let rect = centered(area, 60, 20);
    f.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(BORDER))
        .title(Line::from("Spot").centered());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let inner = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    let label_w = 16u16.min(inner.width);
    let value_w = inner.width.saturating_sub(label_w);
    // Scroll so the selected row stays visible on short terminals.
    let visible = inner.height as usize;
    let offset = (selected + 1).saturating_sub(visible);
    for (i, (label, value)) in rows.iter().enumerate().skip(offset).take(visible) {
        let y = inner.y + (i - offset) as u16;
        match value {
            None => f.render_widget(
                Paragraph::new(Span::styled(clean(label), Style::new().fg(Color::Green))),
                Rect::new(inner.x, y, inner.width, 1),
            ),
            Some(v) => {
                f.render_widget(
                    Paragraph::new(clean(label)),
                    Rect::new(inner.x, y, label_w, 1),
                );
                f.render_widget(
                    Paragraph::new(fit(&clean(v), value_w as usize)),
                    Rect::new(inner.x + label_w, y, value_w, 1),
                );
            }
        }
        // yazi highlights the value column of the selected row, header rows too.
        if i == selected {
            f.buffer_mut().set_style(
                Rect::new(inner.x + label_w, y, value_w, 1),
                Style::new()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::REVERSED),
            );
        }
    }
    if pending_c {
        let w = rect.width.saturating_sub(2);
        let which = Rect::new(rect.x + 1, (rect.y + rect.height).saturating_sub(4), w, 3);
        f.render_widget(Clear, which);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" c", Style::new().fg(Color::LightCyan)),
                Span::styled("  ", Style::new().fg(Color::DarkGray)),
                Span::styled("Copy selected cell", Style::new().fg(Color::LightMagenta)),
            ]))
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(BORDER)),
            ),
            which,
        );
    }
}

/// Notices in the right part of `area`, stacked upward from its bottom like
/// yazi's; a notice that does not fit completely is not drawn.
fn draw_notices(f: &mut Frame, area: Rect, p: &Picker) {
    let w = 40u16.min(area.width);
    let mut bottom = area.y + area.height;
    for n in p.notices.iter().rev() {
        let text = Paragraph::new(clean(&n.message)).wrap(Wrap { trim: true });
        let h = text.line_count(w.saturating_sub(2)) as u16 + 2;
        if bottom < area.y + h {
            break;
        }
        bottom -= h;
        let y = bottom;
        let rect = Rect::new(area.x + area.width - w, y, w, h);
        let color = match n.level {
            Level::Info => Color::Green,
            Level::Warn => Color::Yellow,
        };
        f.render_widget(Clear, rect);
        f.render_widget(
            text.block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(color))
                    .title(Span::styled(clean(&n.title), Style::new().fg(color))),
            ),
            rect,
        );
    }
}
