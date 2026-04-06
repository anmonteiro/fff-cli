use std::cmp::max;
use std::fs::OpenOptions;
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{ArgAction, Parser, Subcommand};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{self, disable_raw_mode, enable_raw_mode};

use fff_tui::{
    FileMatch, FileSearchEngine, FileSearchView, GitKind, GrepCliOptions, HistoryDirection,
    HistoryMatch, HistorySearchEngine, HistorySearchView, PickerMode, clamp_selected,
    ensure_selection_visible, format_grep_context, format_grep_match, fuzzy_match_indices,
    grep_cli_mode, grep_cli_search, load_history_commands, move_selection_down, move_selection_up,
    selected_label, truncate, truncate_path,
};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const CYAN: &str = "\x1b[36m";
const YELLOW: &str = "\x1b[33m";
const MAGENTA: &str = "\x1b[35m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const GRAY: &str = "\x1b[38;5;244m";
const BORDER: &str = "\x1b[38;5;149m";
const SELECTED: &str = "\x1b[48;5;238m\x1b[38;5;230m";
const SELECTED_ACCENT: &str = "\x1b[48;5;238m\x1b[38;5;111m";
const MATCH_HL: &str = "\x1b[30m\x1b[48;5;228m";
const PROMPT_CURSOR: &str = "\x1b[47m \x1b[0m";

#[derive(Debug, Subcommand)]
enum Command {
    Files {
        #[arg(long)]
        base_path: Option<PathBuf>,
    },
    History {
        #[arg(long, env = "FFF_HISTORY_QUERY")]
        query: Option<String>,
        #[arg(long, env = "FFF_HISTORY_DIRECTION", default_value = "backward")]
        history_direction: String,
    },
}

#[derive(Debug, Parser)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    query: Option<String>,
    #[arg(default_value = ".")]
    path: PathBuf,
    #[arg(short = 'F', long)]
    fixed_strings: bool,
    #[arg(long, conflicts_with = "fixed_strings")]
    fuzzy: bool,
    #[arg(short = 'C', long)]
    context: Option<usize>,
    #[arg(short = 'B', long)]
    before_context: Option<usize>,
    #[arg(short = 'A', long)]
    after_context: Option<usize>,
    #[arg(short = 'm', long, default_value_t = 5000)]
    max_matches_per_file: usize,
    #[arg(long, default_value_t = 5000)]
    page_limit: usize,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    smart_case: bool,
}

enum PickerEngine {
    Files(FileSearchEngine),
    History(HistorySearchEngine),
}

struct App {
    mode: PickerMode,
    engine: PickerEngine,
    query: String,
    query_cursor: usize,
    selected: usize,
    scroll: usize,
    files: Option<FileSearchView>,
    history: Option<HistorySearchView>,
}

#[derive(Clone, Copy)]
struct BoxArea {
    row: u16,
    col: u16,
    width: u16,
    height: u16,
}

struct TerminalUi {
    output: Box<dyn Write>,
    input: Box<dyn Read>,
    anchor_row: u16,
    anchor_col: u16,
    last_box: Option<BoxArea>,
    last_size: Option<(u16, u16)>,
}

impl App {
    fn new_files(base_path: PathBuf) -> Result<Self> {
        let engine = FileSearchEngine::new(base_path)?;
        let mut app = Self {
            mode: PickerMode::Files,
            engine: PickerEngine::Files(engine),
            query: String::new(),
            query_cursor: 0,
            selected: 0,
            scroll: 0,
            files: None,
            history: None,
        };
        app.refresh()?;
        Ok(app)
    }

    fn new_history(query: String, direction: HistoryDirection) -> Result<Self> {
        let mut stdin_data = Vec::new();
        if !io::stdin().is_terminal() {
            io::stdin().read_to_end(&mut stdin_data)?;
        }
        let commands = load_history_commands(&stdin_data, direction);
        let engine = HistorySearchEngine::new(commands)?;
        let mut app = Self {
            mode: PickerMode::History,
            engine: PickerEngine::History(engine),
            query_cursor: query.chars().count(),
            query,
            selected: 0,
            scroll: 0,
            files: None,
            history: None,
        };
        app.refresh()?;
        Ok(app)
    }

    fn result_len(&self) -> usize {
        match self.mode {
            PickerMode::Files => self.files.as_ref().map_or(0, |view| view.matches.len()),
            PickerMode::History => self.history.as_ref().map_or(0, |view| view.matches.len()),
        }
    }

    fn refresh(&mut self) -> Result<()> {
        match &self.engine {
            PickerEngine::Files(engine) => {
                self.files = Some(engine.search(&self.query)?);
            }
            PickerEngine::History(engine) => {
                self.history = Some(engine.search(&self.query)?);
            }
        }
        self.selected = clamp_selected(self.selected, self.result_len());
        Ok(())
    }

    fn selected_output(&self) -> Option<String> {
        match self.mode {
            PickerMode::Files => self
                .files
                .as_ref()
                .and_then(|view| view.matches.get(self.selected))
                .map(|item| item.relative_path.clone()),
            PickerMode::History => self
                .history
                .as_ref()
                .and_then(|view| view.matches.get(self.selected))
                .map(|item| item.command.clone()),
        }
    }

    fn query_len_chars(&self) -> usize {
        self.query.chars().count()
    }

    fn query_byte_index(&self) -> usize {
        if self.query_cursor == 0 {
            return 0;
        }

        self.query
            .char_indices()
            .nth(self.query_cursor)
            .map(|(idx, _)| idx)
            .unwrap_or(self.query.len())
    }

    fn reset_after_query_edit(&mut self) -> Result<()> {
        self.selected = 0;
        self.scroll = 0;
        self.refresh()
    }

    fn insert_char(&mut self, ch: char) -> Result<()> {
        let byte_idx = self.query_byte_index();
        self.query.insert(byte_idx, ch);
        self.query_cursor += 1;
        self.reset_after_query_edit()
    }

    fn delete_backward(&mut self) -> Result<()> {
        if self.query_cursor == 0 {
            return Ok(());
        }

        let end = self.query_byte_index();
        self.query_cursor -= 1;
        let start = self.query_byte_index();
        self.query.replace_range(start..end, "");
        self.reset_after_query_edit()
    }

    fn delete_forward(&mut self) -> Result<()> {
        if self.query_cursor >= self.query_len_chars() {
            return Ok(());
        }

        let start = self.query_byte_index();
        self.query_cursor += 1;
        let end = self.query_byte_index();
        self.query_cursor -= 1;
        self.query.replace_range(start..end, "");
        self.reset_after_query_edit()
    }

    fn kill_to_end(&mut self) -> Result<()> {
        let start = self.query_byte_index();
        self.query.truncate(start);
        self.reset_after_query_edit()
    }

    fn kill_to_start(&mut self) -> Result<()> {
        let end = self.query_byte_index();
        self.query.replace_range(..end, "");
        self.query_cursor = 0;
        self.reset_after_query_edit()
    }

    fn move_cursor_left(&mut self) {
        self.query_cursor = self.query_cursor.saturating_sub(1);
    }

    fn move_cursor_right(&mut self) {
        self.query_cursor = (self.query_cursor + 1).min(self.query_len_chars());
    }

    fn move_cursor_home(&mut self) {
        self.query_cursor = 0;
    }

    fn move_cursor_end(&mut self) {
        self.query_cursor = self.query_len_chars();
    }

    fn move_cursor_word_left(&mut self) {
        let chars: Vec<char> = self.query.chars().collect();
        let mut cursor = self.query_cursor.min(chars.len());

        while cursor > 0 && chars[cursor - 1].is_whitespace() {
            cursor -= 1;
        }
        while cursor > 0 && !chars[cursor - 1].is_whitespace() {
            cursor -= 1;
        }

        self.query_cursor = cursor;
    }

    fn move_cursor_word_right(&mut self) {
        let chars: Vec<char> = self.query.chars().collect();
        let mut cursor = self.query_cursor.min(chars.len());

        while cursor < chars.len() && chars[cursor].is_whitespace() {
            cursor += 1;
        }
        while cursor < chars.len() && !chars[cursor].is_whitespace() {
            cursor += 1;
        }

        self.query_cursor = cursor;
    }

    fn kill_word_right(&mut self) -> Result<()> {
        let start_char = self.query_cursor;
        self.move_cursor_word_right();
        if self.query_cursor == start_char {
            return Ok(());
        }

        let end = self.query_byte_index();
        self.query_cursor = start_char;
        let start = self.query_byte_index();
        self.query.replace_range(start..end, "");
        self.reset_after_query_edit()
    }

    fn kill_word_left(&mut self) -> Result<()> {
        let end = self.query_byte_index();
        let end_char = self.query_cursor;
        self.move_cursor_word_left();
        if self.query_cursor == end_char {
            return Ok(());
        }

        let start = self.query_byte_index();
        self.query.replace_range(start..end, "");
        self.reset_after_query_edit()
    }
}

fn parse_history_direction(value: &str) -> HistoryDirection {
    if value.eq_ignore_ascii_case("forward") {
        HistoryDirection::Forward
    } else {
        HistoryDirection::Backward
    }
}

fn interactive_output() -> Result<Box<dyn Write>> {
    if io::stdout().is_terminal() {
        return Ok(Box::new(io::stdout()));
    }

    let tty = OpenOptions::new()
        .write(true)
        .open("/dev/tty")
        .context("failed to open /dev/tty for interactive output")?;
    Ok(Box::new(tty))
}

fn interactive_input() -> Result<Box<dyn Read>> {
    if io::stdin().is_terminal() {
        return Ok(Box::new(io::stdin()));
    }

    let tty = OpenOptions::new()
        .read(true)
        .open("/dev/tty")
        .context("failed to open /dev/tty for interactive input")?;
    Ok(Box::new(tty))
}

fn move_to(out: &mut dyn Write, row: u16, col: u16) -> io::Result<()> {
    write!(
        out,
        "\x1b[{};{}H",
        row.saturating_add(1),
        col.saturating_add(1)
    )
}

fn clear_rect(out: &mut dyn Write, area: BoxArea) -> io::Result<()> {
    for offset in 0..area.height {
        move_to(out, area.row + offset, area.col)?;
        write!(out, "\x1b[2K")?;
    }
    Ok(())
}

fn clear_below_prompt(out: &mut dyn Write, start_row: u16, rows: u16) -> io::Result<()> {
    for row in start_row..rows {
        move_to(out, row, 0)?;
        write!(out, "\x1b[2K")?;
    }
    Ok(())
}

fn draw_box(out: &mut dyn Write, area: BoxArea, title: &str) -> io::Result<()> {
    let inner_width = area.width.saturating_sub(2) as usize;
    let title_text = if title.is_empty() {
        String::new()
    } else {
        format!(" {title} ")
    };
    let left_fill = 1usize;
    let right_fill = inner_width.saturating_sub(title_text.chars().count() + left_fill);

    move_to(out, area.row, area.col)?;
    write!(
        out,
        "{BORDER}┌{}{BOLD}{}{RESET}{BORDER}{}┐{RESET}",
        "─".repeat(left_fill),
        title_text,
        "─".repeat(right_fill),
    )?;

    for offset in 1..area.height.saturating_sub(1) {
        move_to(out, area.row + offset, area.col)?;
        write!(
            out,
            "{BORDER}│{RESET}{}{BORDER}│{RESET}",
            " ".repeat(inner_width)
        )?;
    }

    move_to(out, area.row + area.height.saturating_sub(1), area.col)?;
    write!(out, "{BORDER}└{}┘{RESET}", "─".repeat(inner_width))?;
    Ok(())
}

fn draw_inner_line(
    out: &mut dyn Write,
    row: u16,
    col: u16,
    width: u16,
    text: &str,
) -> io::Result<()> {
    move_to(out, row, col)?;
    let visible = visible_width(text);
    if visible >= width as usize {
        write!(out, "{text}")?;
    } else {
        write!(out, "{text}{}", " ".repeat(width as usize - visible))?;
    }
    Ok(())
}

fn query_cursor_position(out: &mut dyn Write, input: &mut dyn Read) -> Result<(u16, u16)> {
    out.write_all(b"\x1b[6n")?;
    out.flush()?;

    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        input.read_exact(&mut byte)?;
        buf.push(byte[0]);
        if byte[0] == b'R' {
            break;
        }
    }

    let response = String::from_utf8_lossy(&buf);
    let Some(rest) = response.strip_prefix("\x1b[") else {
        return Ok((0, 0));
    };
    let Some(rest) = rest.strip_suffix('R') else {
        return Ok((0, 0));
    };
    let Some((row, col)) = rest.split_once(';') else {
        return Ok((0, 0));
    };

    let row = row.parse::<u16>().unwrap_or(1).saturating_sub(1);
    let col = col.parse::<u16>().unwrap_or(1).saturating_sub(1);
    Ok((row, col))
}

fn visible_width(text: &str) -> usize {
    let mut width = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.peek() == Some(&'[') {
                let _ = chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        width += 1;
    }
    width
}

fn apply_indices_highlight(
    text: &str,
    indices: &[usize],
    selected: bool,
    base_style: &str,
) -> String {
    if indices.is_empty() {
        return if selected {
            format!("{base_style}{text}")
        } else {
            format!("{base_style}{text}{RESET}")
        };
    }

    let mut out = String::from(base_style);
    for (idx, ch) in text.chars().enumerate() {
        if indices.contains(&idx) {
            if selected {
                out.push_str(SELECTED_ACCENT);
                out.push_str(BOLD);
                out.push(ch);
                out.push_str(base_style);
            } else {
                out.push_str(BOLD);
                out.push_str(CYAN);
                out.push(ch);
                out.push_str(RESET);
                out.push_str(base_style);
            }
        } else {
            out.push(ch);
        }
    }

    if !selected {
        out.push_str(RESET);
    }
    out
}

fn apply_match_ranges(
    text: &str,
    ranges: &[(usize, usize)],
    selected: bool,
    base_style: &str,
) -> String {
    let mut indices = Vec::new();
    for (start, end) in ranges {
        for idx in *start..*end {
            indices.push(idx);
        }
    }
    apply_indices_highlight(text, &indices, selected, base_style)
}

fn highlight_grep_ranges(text: &str, ranges: &[(usize, usize)], color: bool) -> String {
    if !color || ranges.is_empty() {
        return text.to_string();
    }

    let mut out = String::new();
    let mut byte_index = 0usize;
    for ch in text.chars() {
        let end = byte_index + ch.len_utf8();
        let matched = ranges
            .iter()
            .any(|(start, finish)| byte_index < *finish && end > *start);
        if matched {
            out.push_str(MATCH_HL);
            out.push(ch);
            out.push_str(RESET);
        } else {
            out.push(ch);
        }
        byte_index = end;
    }

    out
}

fn git_icon(kind: GitKind, selected: bool) -> String {
    let (color, ch) = match kind {
        GitKind::Modified => (YELLOW, "M"),
        GitKind::Added => (CYAN, "A"),
        GitKind::Deleted => (RED, "D"),
        GitKind::Renamed => (MAGENTA, "R"),
        GitKind::Clean => (GRAY, "·"),
    };

    if selected {
        format!("{color}{ch}")
    } else {
        format!("{color}{ch}{RESET}")
    }
}

fn badge_text(item: &FileMatch, selected: bool) -> String {
    let Some(badge) = &item.badge else {
        return String::new();
    };

    let color = if selected {
        SELECTED_ACCENT
    } else if badge.icon == "🔥" {
        RED
    } else {
        YELLOW
    };

    if selected {
        format!("{color}{}{score}", badge.icon, score = badge.score)
    } else {
        format!("{color}{}{score}{RESET}", badge.icon, score = badge.score)
    }
}

fn file_line(item: &FileMatch, query: &str, selected: bool, width: usize) -> String {
    let prefix = if selected {
        format!("{SELECTED_ACCENT}> ")
    } else {
        "  ".to_string()
    };
    let git = format!("{} ", git_icon(item.git, selected));
    let badge = badge_text(item, selected);
    let badge_width = if badge.is_empty() {
        0
    } else {
        visible_width(&badge) + 1
    };

    let slash = item
        .relative_path
        .rfind('/')
        .map(|idx| idx + 1)
        .unwrap_or(0);
    let dir = &item.relative_path[..slash];
    let base = &item.relative_path[slash..];
    let available = width.saturating_sub(visible_width(&prefix) + 2 + badge_width);
    let (dir, base) = truncate_path(dir, base, available);
    let display = format!("{dir}{base}");
    let matched = fuzzy_match_indices(&display, query);
    let dir_len = dir.chars().count();
    let dir_matches = matched
        .iter()
        .copied()
        .filter(|idx| *idx < dir_len)
        .collect::<Vec<_>>();
    let base_matches = matched
        .iter()
        .copied()
        .filter_map(|idx| idx.checked_sub(dir_len))
        .collect::<Vec<_>>();
    let rendered_dir = if dir.is_empty() {
        String::new()
    } else {
        apply_indices_highlight(
            &dir,
            &dir_matches,
            selected,
            if selected { "" } else { GRAY },
        )
    };
    let rendered_base = apply_indices_highlight(&base, &base_matches, selected, BOLD);

    let content = if badge.is_empty() {
        format!("{prefix}{git}{rendered_dir}{rendered_base}")
    } else {
        format!("{prefix}{git}{rendered_dir}{rendered_base} {badge}")
    };

    if selected {
        format!("{SELECTED}{content}{RESET}")
    } else {
        content
    }
}

fn history_line(item: &HistoryMatch, selected: bool, width: usize) -> String {
    let prefix = if selected {
        format!("{SELECTED_ACCENT}> ")
    } else {
        "  ".to_string()
    };
    let display = truncate(&item.display, width.saturating_sub(visible_width(&prefix)));
    let rendered = apply_match_ranges(&display, &item.match_ranges, selected, "");
    let content = format!("{prefix}{rendered}");
    if selected {
        format!("{SELECTED}{content}{RESET}")
    } else {
        content
    }
}

fn desired_height(mode: PickerMode, rows: u16) -> u16 {
    match mode {
        PickerMode::Files => max(12, ((rows as f32) * 0.4).floor() as u16),
        PickerMode::History => std::env::var("FFF_HISTORY_HEIGHT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(12),
    }
    .min(rows.saturating_sub(1))
}

fn prompt_line(query: &str, cursor: usize) -> String {
    let mut rendered = String::from(CYAN);
    rendered.push('🪿');
    rendered.push(' ');
    rendered.push_str(RESET);

    let mut chars = query.chars();
    for _ in 0..cursor {
        if let Some(ch) = chars.next() {
            rendered.push(ch);
        } else {
            break;
        }
    }

    if let Some(ch) = chars.next() {
        rendered.push_str("\x1b[30m\x1b[47m");
        rendered.push(ch);
        rendered.push_str(RESET);
    } else {
        rendered.push_str(PROMPT_CURSOR);
    }

    for ch in chars {
        rendered.push(ch);
    }

    rendered
}

fn ensure_space_below_prompt(
    out: &mut dyn Write,
    anchor_row: u16,
    pane_height: u16,
    rows: u16,
) -> Result<u16> {
    let shortage = anchor_row
        .saturating_add(1)
        .saturating_add(pane_height)
        .saturating_sub(rows);
    if shortage == 0 {
        return Ok(anchor_row);
    }

    write!(out, "\x1b[{}S", shortage)?;
    Ok(anchor_row.saturating_sub(shortage))
}

fn render(app: &mut App, ui: &mut TerminalUi) -> Result<()> {
    let (cols, rows) = terminal::size()?;
    let resized = ui.last_size.is_some_and(|last| last != (cols, rows));
    ui.last_size = Some((cols, rows));

    if resized {
        clear_below_prompt(&mut *ui.output, ui.anchor_row.saturating_add(1), rows)?;
        ui.last_box = None;
    }

    let desired = desired_height(app.mode, rows);
    ui.anchor_row = ensure_space_below_prompt(&mut *ui.output, ui.anchor_row, desired, rows)?;

    let box_row = ui.anchor_row.saturating_add(1);
    let available_rows = rows.saturating_sub(box_row).max(6);
    let box_height = desired.min(available_rows);
    let area = BoxArea {
        row: box_row,
        col: 0,
        width: cols,
        height: box_height,
    };
    let content_width = cols.saturating_sub(2);

    if let Some(last) = ui.last_box {
        clear_rect(&mut *ui.output, last)?;
    }
    ui.last_box = Some(area);

    ui.output.write_all(b"\x1b[?25l")?;
    draw_box(
        &mut *ui.output,
        area,
        match app.mode {
            PickerMode::Files => "FFFiles",
            PickerMode::History => "FFFHistory",
        },
    )?;

    match app.mode {
        PickerMode::Files => {
            let view = app.files.as_ref().context("missing file view")?;
            let prompt_row = area.row + 1;
            let header_row = area.row + 2;
            let list_row = area.row + 3;
            let visible_count = max(1, area.height.saturating_sub(4) as usize);
            app.scroll = ensure_selection_visible(app.selected, app.scroll, visible_count);

            draw_inner_line(
                &mut *ui.output,
                prompt_row,
                1,
                content_width,
                &prompt_line(&app.query, app.query_cursor),
            )?;

            let left_plain = format!(
                "{} ({} loaded)",
                selected_label(app.selected, view.total_matched),
                view.loaded
            );
            let right_plain = truncate(
                &view.root_display,
                content_width as usize - left_plain.chars().count() - 1,
            );
            let gap = max(
                1,
                content_width as usize - left_plain.chars().count() - right_plain.chars().count(),
            );
            let header = format!(
                "{BOLD}{left_plain}{RESET}{}{GRAY}{right_plain}{RESET}",
                " ".repeat(gap)
            );
            draw_inner_line(&mut *ui.output, header_row, 1, content_width, &header)?;

            for row_offset in 0..visible_count {
                let screen_row = list_row + row_offset as u16;
                let idx = app.scroll + row_offset;
                if let Some(item) = view.matches.get(idx) {
                    let line = file_line(
                        item,
                        &app.query,
                        idx == app.selected,
                        content_width as usize,
                    );
                    draw_inner_line(&mut *ui.output, screen_row, 1, content_width, &line)?;
                } else {
                    draw_inner_line(&mut *ui.output, screen_row, 1, content_width, "")?;
                }
            }
        }
        PickerMode::History => {
            let view = app.history.as_ref().context("missing history view")?;
            let header_row = area.row + 1;
            let prompt_row = area.row + area.height.saturating_sub(2);
            let list_bottom_row = prompt_row.saturating_sub(1);
            let visible_count = max(1, area.height.saturating_sub(4) as usize);
            app.scroll = app.selected.min(view.matches.len().saturating_sub(1));

            let right_plain = format!("{} shown", view.matches.len());
            let header = format!(
                "{BOLD}{}{RESET} {GRAY}{right_plain}{RESET}",
                selected_label(app.selected, view.total_matched),
            );
            draw_inner_line(&mut *ui.output, header_row, 1, content_width, &header)?;

            for row_offset in 0..visible_count {
                let screen_row = list_bottom_row.saturating_sub(row_offset as u16);
                let idx = app.scroll + row_offset;
                if let Some(item) = view.matches.get(idx) {
                    let line = history_line(item, idx == app.selected, content_width as usize);
                    draw_inner_line(&mut *ui.output, screen_row, 1, content_width, &line)?;
                } else {
                    draw_inner_line(&mut *ui.output, screen_row, 1, content_width, "")?;
                }
            }

            draw_inner_line(
                &mut *ui.output,
                prompt_row,
                1,
                content_width,
                &prompt_line(&app.query, app.query_cursor),
            )?;
        }
    }

    ui.output.flush()?;
    Ok(())
}

fn cleanup(ui: &mut TerminalUi) -> Result<()> {
    if let Some(area) = ui.last_box {
        clear_rect(&mut *ui.output, area)?;
    }
    move_to(&mut *ui.output, ui.anchor_row, ui.anchor_col)?;
    ui.output.write_all(b"\x1b[?25h\x1b[0m")?;
    ui.output.flush()?;
    Ok(())
}

fn handle_key(app: &mut App, key: KeyEvent) -> Result<Option<String>> {
    if key.kind != KeyEventKind::Press {
        return Ok(None);
    }

    let reverse_history = matches!(app.mode, PickerMode::History);

    match key {
        KeyEvent {
            code: KeyCode::Enter,
            ..
        } => return Ok(app.selected_output()),
        KeyEvent {
            code: KeyCode::Esc, ..
        } => bail!("cancelled"),
        KeyEvent {
            code: KeyCode::Char('c'),
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::CONTROL) => bail!("cancelled"),
        KeyEvent {
            code: KeyCode::Char('\u{0001}'),
            modifiers: KeyModifiers::NONE,
            ..
        } => app.move_cursor_home(),
        KeyEvent {
            code: KeyCode::Char('a'),
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::CONTROL) => app.move_cursor_home(),
        KeyEvent {
            code: KeyCode::Char('\u{0002}'),
            modifiers: KeyModifiers::NONE,
            ..
        }
        | KeyEvent {
            code: KeyCode::Char('b'),
            modifiers: KeyModifiers::CONTROL,
            ..
        }
        | KeyEvent {
            code: KeyCode::Left,
            modifiers: KeyModifiers::NONE,
            ..
        } => app.move_cursor_left(),
        KeyEvent {
            code: KeyCode::Char('\u{0004}'),
            modifiers: KeyModifiers::NONE,
            ..
        }
        | KeyEvent {
            code: KeyCode::Char('d'),
            modifiers: KeyModifiers::CONTROL,
            ..
        }
        | KeyEvent {
            code: KeyCode::Delete,
            modifiers: KeyModifiers::NONE,
            ..
        } => app.delete_forward()?,
        KeyEvent {
            code: KeyCode::Char('\u{0005}'),
            modifiers: KeyModifiers::NONE,
            ..
        } => app.move_cursor_end(),
        KeyEvent {
            code: KeyCode::Char('e'),
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::CONTROL) => app.move_cursor_end(),
        KeyEvent {
            code: KeyCode::Char('\u{0006}'),
            modifiers: KeyModifiers::NONE,
            ..
        }
        | KeyEvent {
            code: KeyCode::Char('f'),
            modifiers: KeyModifiers::CONTROL,
            ..
        }
        | KeyEvent {
            code: KeyCode::Right,
            modifiers: KeyModifiers::NONE,
            ..
        } => app.move_cursor_right(),
        KeyEvent {
            code: KeyCode::Char('\u{0008}'),
            modifiers: KeyModifiers::NONE,
            ..
        }
        | KeyEvent {
            code: KeyCode::Char('h'),
            modifiers: KeyModifiers::CONTROL,
            ..
        }
        | KeyEvent {
            code: KeyCode::Backspace,
            modifiers: KeyModifiers::NONE,
            ..
        } => app.delete_backward()?,
        KeyEvent {
            code: KeyCode::Char('\u{000b}'),
            modifiers: KeyModifiers::NONE,
            ..
        }
        | KeyEvent {
            code: KeyCode::Char('k'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => app.kill_to_end()?,
        KeyEvent {
            code: KeyCode::Char('\u{000e}'),
            modifiers: KeyModifiers::NONE,
            ..
        }
        | KeyEvent {
            code: KeyCode::Char('n'),
            modifiers: KeyModifiers::CONTROL,
            ..
        }
        | KeyEvent {
            code: KeyCode::Down,
            ..
        } => {
            let wrap = matches!(app.mode, PickerMode::Files);
            app.selected = if reverse_history {
                move_selection_up(app.selected, app.result_len(), wrap)
            } else {
                move_selection_down(app.selected, app.result_len(), wrap)
            };
        }
        KeyEvent {
            code: KeyCode::Char('\u{0010}'),
            modifiers: KeyModifiers::NONE,
            ..
        }
        | KeyEvent {
            code: KeyCode::Char('p'),
            modifiers: KeyModifiers::CONTROL,
            ..
        }
        | KeyEvent {
            code: KeyCode::Up, ..
        } => {
            let wrap = matches!(app.mode, PickerMode::Files);
            app.selected = if reverse_history {
                move_selection_down(app.selected, app.result_len(), wrap)
            } else {
                move_selection_up(app.selected, app.result_len(), wrap)
            };
        }
        KeyEvent {
            code: KeyCode::Char('\u{0015}'),
            modifiers: KeyModifiers::NONE,
            ..
        }
        | KeyEvent {
            code: KeyCode::Char('u'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => app.kill_to_start()?,
        KeyEvent {
            code: KeyCode::Char('\u{0017}'),
            modifiers: KeyModifiers::NONE,
            ..
        } => app.kill_word_left()?,
        KeyEvent {
            code: KeyCode::Char('w'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => app.kill_word_left()?,
        KeyEvent {
            code: KeyCode::Backspace,
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::ALT) => app.kill_word_left()?,
        KeyEvent {
            code: KeyCode::Char('h'),
            modifiers,
            ..
        } if modifiers == (KeyModifiers::CONTROL | KeyModifiers::ALT) => app.kill_word_left()?,
        KeyEvent {
            code: KeyCode::Delete,
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::ALT) => app.kill_word_right()?,
        KeyEvent {
            code: KeyCode::Char('b'),
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::ALT) => app.move_cursor_word_left(),
        KeyEvent {
            code: KeyCode::Left,
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::ALT) || modifiers.contains(KeyModifiers::CONTROL) => {
            app.move_cursor_word_left()
        }
        KeyEvent {
            code: KeyCode::Char('d'),
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::ALT) => app.kill_word_right()?,
        KeyEvent {
            code: KeyCode::Char('f'),
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::ALT) => app.move_cursor_word_right(),
        KeyEvent {
            code: KeyCode::Right,
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::ALT) || modifiers.contains(KeyModifiers::CONTROL) => {
            app.move_cursor_word_right()
        }
        KeyEvent {
            code: KeyCode::Home,
            ..
        } => app.move_cursor_home(),
        KeyEvent {
            code: KeyCode::End, ..
        } => app.move_cursor_end(),
        KeyEvent {
            code: KeyCode::Char(ch),
            modifiers,
            ..
        } if !modifiers.contains(KeyModifiers::CONTROL)
            && !modifiers.contains(KeyModifiers::ALT) =>
        {
            app.insert_char(ch)?;
        }
        _ => {}
    }

    Ok(None)
}

fn run(app: &mut App) -> Result<Option<String>> {
    enable_raw_mode()?;
    let mut ui = TerminalUi {
        output: interactive_output()?,
        input: interactive_input()?,
        anchor_row: 0,
        anchor_col: 0,
        last_box: None,
        last_size: None,
    };

    let (row, col) = query_cursor_position(&mut *ui.output, &mut *ui.input)?;
    ui.anchor_row = row;
    ui.anchor_col = col;

    let result = (|| -> Result<Option<String>> {
        render(app, &mut ui)?;

        loop {
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }

            match event::read()? {
                Event::Key(key) => {
                    if let Some(output) = handle_key(app, key)? {
                        return Ok(Some(output));
                    }
                    render(app, &mut ui)?;
                }
                Event::Resize(_, _) => {
                    render(app, &mut ui)?;
                }
                _ => {}
            }
        }
    })();

    let cleanup_result = cleanup(&mut ui);
    disable_raw_mode()?;

    match (result, cleanup_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (_, Err(error)) => Err(error),
    }
}

fn run_grep(cli: Cli) -> Result<()> {
    let before_context = cli.before_context.or(cli.context).unwrap_or(0);
    let after_context = cli.after_context.or(cli.context).unwrap_or(0);
    let result = grep_cli_search(&GrepCliOptions {
        base_path: cli.path,
        query: cli.query.context("missing grep query")?,
        mode: grep_cli_mode(cli.fixed_strings, cli.fuzzy),
        smart_case: cli.smart_case,
        before_context,
        after_context,
        max_file_size: 10 * 1024 * 1024,
        max_matches_per_file: cli.max_matches_per_file,
        page_limit: cli.page_limit,
    })?;
    let use_color = io::stdout().is_terminal();
    let mut current_path: Option<String> = None;

    for item in result.matches {
        if current_path.as_deref() != Some(item.path.as_str()) {
            if current_path.is_some() {
                println!();
            }
            if use_color {
                println!("{GREEN}{BOLD}{}{RESET}", item.path);
            } else {
                println!("{}", item.path);
            }
            current_path = Some(item.path.clone());
        }

        for (idx, line) in item.context_before.iter().enumerate() {
            let line_number = item
                .line_number
                .saturating_sub(item.context_before.len() as u64)
                + idx as u64;
            if use_color {
                println!("{YELLOW}{line_number}-{RESET}{GRAY}{line}{RESET}");
            } else {
                println!("{}", format_grep_context(&item.path, line_number, line));
            }
        }

        let content = highlight_grep_ranges(&item.line_content, &item.match_ranges, use_color);
        if use_color {
            println!("{YELLOW}{BOLD}{}{RESET}:{content}", item.line_number);
        } else {
            println!(
                "{}",
                format_grep_match(&item.path, item.line_number, item.col, &item.line_content)
            );
        }

        for (idx, line) in item.context_after.iter().enumerate() {
            let line_number = item.line_number + idx as u64 + 1;
            if use_color {
                println!("{YELLOW}{line_number}-{RESET}{GRAY}{line}{RESET}");
            } else {
                println!("{}", format_grep_context(&item.path, line_number, line));
            }
        }
    }

    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Files { base_path }) => {
            let mut app = App::new_files(base_path.unwrap_or(
                std::env::current_dir().context("failed to resolve current directory")?,
            ))?;
            match run(&mut app) {
                Ok(Some(output)) => {
                    print!("{output}");
                    Ok(())
                }
                Ok(None) => Ok(()),
                Err(error) if error.to_string() == "cancelled" => Ok(()),
                Err(error) => Err(error),
            }
        }
        Some(Command::History {
            query,
            history_direction,
        }) => {
            let mut app = App::new_history(
                query.unwrap_or_default(),
                parse_history_direction(&history_direction),
            )?;
            match run(&mut app) {
                Ok(Some(output)) => {
                    print!("{output}");
                    Ok(())
                }
                Ok(None) => Ok(()),
                Err(error) if error.to_string() == "cancelled" => Ok(()),
                Err(error) => Err(error),
            }
        }
        None => run_grep(cli),
    }
}
