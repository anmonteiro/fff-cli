use std::cmp::min;
use std::fs::read_to_string;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use fff::file_picker::{FFFMode, FilePicker, FilePickerOptions, FuzzySearchOptions, scan_files};
use fff::frecency::FrecencyTracker;
use fff::grep::{GrepMode, GrepSearchOptions, grep_search, parse_grep_query};
use fff::query_tracker::QueryTracker;
use fff::shared::{SharedFrecency, SharedPicker, SharedQueryTracker};
use fff::{ContentCacheBudget, PaginationArgs, QueryParser};
use git2::Status;
use sha1::{Digest, Sha1};
use tempfile::TempDir;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerMode {
    Files,
    History,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryDirection {
    Backward,
    Forward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitKind {
    Clean,
    Modified,
    Added,
    Deleted,
    Renamed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Badge {
    pub icon: &'static str,
    pub score: i32,
}

#[derive(Debug, Clone)]
pub struct FileMatch {
    pub path: PathBuf,
    pub relative_path: String,
    pub file_name: String,
    pub git: GitKind,
    pub badge: Option<Badge>,
}

#[derive(Debug, Clone)]
pub struct FileSearchView {
    pub matches: Vec<FileMatch>,
    pub total_matched: usize,
    pub loaded: usize,
    pub root_display: String,
}

#[derive(Debug, Clone)]
pub struct HistoryMatch {
    pub command: String,
    pub display: String,
    pub match_ranges: Vec<(usize, usize)>,
}

#[derive(Debug, Clone)]
pub struct HistorySearchView {
    pub matches: Vec<HistoryMatch>,
    pub total_matched: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrepCliMode {
    PlainText,
    Regex,
    Fuzzy,
}

#[derive(Debug, Clone)]
pub struct GrepCliOptions {
    pub base_path: PathBuf,
    pub query: String,
    pub mode: GrepCliMode,
    pub smart_case: bool,
    pub before_context: usize,
    pub after_context: usize,
    pub max_file_size: u64,
    pub max_matches_per_file: usize,
    pub page_limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepCliMatch {
    pub path: String,
    pub line_number: u64,
    pub col: usize,
    pub line_content: String,
    pub match_ranges: Vec<(usize, usize)>,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct GrepCliResult {
    pub matches: Vec<GrepCliMatch>,
    pub total_files: usize,
    pub total_files_searched: usize,
    pub files_with_matches: usize,
}

pub fn runtime_dir() -> PathBuf {
    std::env::var_os("FFF_SHELL_WIDGET_RUNTIME_DIR")
        .or_else(|| std::env::var_os("FFF_CTRL_T_RUNTIME_DIR"))
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/share/fff-shell-widget")))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .map(|cache| cache.join("fff-shell-widget"))
        .unwrap_or_else(|| PathBuf::from(".cache/fff-shell-widget"))
}

pub fn dedupe_history_entries(entries: Vec<String>, direction: HistoryDirection) -> Vec<String> {
    let iter: Box<dyn Iterator<Item = String>> = match direction {
        HistoryDirection::Backward => Box::new(entries.into_iter().rev()),
        HistoryDirection::Forward => Box::new(entries.into_iter()),
    };

    let mut seen = std::collections::HashSet::new();
    let mut commands = Vec::new();
    for entry in iter {
        let trimmed = entry.trim().to_string();
        if trimmed.is_empty() || !seen.insert(trimmed.clone()) {
            continue;
        }
        commands.push(trimmed);
    }
    commands
}

pub fn parse_history_content(content: &str, direction: HistoryDirection) -> Vec<String> {
    let lines = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            line.strip_prefix(": ")
                .and_then(|rest| rest.split_once(';').map(|(_, command)| command))
                .unwrap_or(line)
                .to_string()
        })
        .collect::<Vec<_>>();

    dedupe_history_entries(lines, direction)
}

pub fn read_history_fallback(histfile: Option<&Path>, direction: HistoryDirection) -> Vec<String> {
    let fallback = dirs::home_dir()
        .map(|home| home.join(".zsh_history"))
        .unwrap_or_else(|| PathBuf::from(".zsh_history"));
    let path = histfile.unwrap_or(&fallback);

    match read_to_string(path) {
        Ok(content) => parse_history_content(&content, direction),
        Err(_) => Vec::new(),
    }
}

pub fn fuzzy_match_indices(text: &str, query: &str) -> Vec<usize> {
    if query.is_empty() {
        return Vec::new();
    }

    let lower_text = text.to_lowercase();
    let lower_query = query.to_lowercase();
    let mut indices = Vec::new();
    let mut query_chars = lower_query.chars();
    let mut current = query_chars.next();

    for (idx, ch) in lower_text.chars().enumerate() {
        if current.is_some_and(|q| q == ch) {
            indices.push(idx);
            current = query_chars.next();
            if current.is_none() {
                return indices;
            }
        }
    }

    Vec::new()
}

pub fn truncate_path(dir: &str, base: &str, width: usize) -> (String, String) {
    if dir.len() + base.len() <= width {
        return (dir.to_string(), base.to_string());
    }

    if base.len() >= width {
        let clipped = truncate(base, width);
        return (String::new(), clipped);
    }

    let clipped_dir = truncate(dir, width - base.len());
    (clipped_dir, base.to_string())
}

pub fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let char_count = text.chars().count();
    if char_count <= width {
        return text.to_string();
    }
    if width <= 3 {
        return text.chars().take(width).collect();
    }

    let clipped: String = text.chars().take(width - 3).collect();
    format!("{clipped}...")
}

pub fn sanitize_history_display(command: &str) -> String {
    command
        .replace("\r\n", " ↩ ")
        .replace('\n', " ↩ ")
        .trim()
        .to_string()
}

pub fn grep_cli_mode(fixed_strings: bool, fuzzy: bool) -> GrepCliMode {
    if fuzzy {
        GrepCliMode::Fuzzy
    } else if fixed_strings {
        GrepCliMode::PlainText
    } else {
        GrepCliMode::Regex
    }
}

pub fn format_grep_match(path: &str, line_number: u64, col: usize, line_content: &str) -> String {
    format!("{path}:{line_number}:{}:{line_content}", col + 1)
}

pub fn format_grep_context(path: &str, line_number: u64, line_content: &str) -> String {
    format!("{path}-{line_number}-{line_content}")
}

pub fn git_kind(status: Option<Status>) -> GitKind {
    match status {
        Some(s) if s.contains(Status::WT_DELETED) || s.contains(Status::INDEX_DELETED) => {
            GitKind::Deleted
        }
        Some(s) if s.contains(Status::WT_RENAMED) || s.contains(Status::INDEX_RENAMED) => {
            GitKind::Renamed
        }
        Some(s) if s.contains(Status::WT_NEW) || s.contains(Status::INDEX_NEW) => GitKind::Added,
        Some(s) if s.contains(Status::WT_MODIFIED) || s.contains(Status::INDEX_MODIFIED) => {
            GitKind::Modified
        }
        _ => GitKind::Clean,
    }
}

pub fn frecency_badge(total: i32, access: i32, modified: i32) -> Option<Badge> {
    if total <= 0 {
        return None;
    }

    let icon = if modified >= 6 {
        "🔥"
    } else if access >= 4 {
        "⭐"
    } else if total >= 3 {
        "✨"
    } else if total >= 1 {
        "•"
    } else {
        return None;
    };

    Some(Badge { icon, score: total })
}

pub fn ensure_selection_visible(selected: usize, scroll: usize, visible_count: usize) -> usize {
    if selected < scroll {
        selected
    } else if selected >= scroll + visible_count {
        selected.saturating_sub(visible_count.saturating_sub(1))
    } else {
        scroll
    }
}

pub struct FileSearchEngine {
    base_path: PathBuf,
    root_display: String,
    picker: SharedPicker,
    query_tracker: SharedQueryTracker,
}

impl FileSearchEngine {
    pub fn new(base_path: impl AsRef<Path>) -> Result<Self> {
        let base_path = base_path.as_ref().to_path_buf();
        let base_path_str = base_path.display().to_string();
        let cache_dir = cache_dir();
        let runtime = runtime_dir();
        let data_dir = runtime.join("data");
        std::fs::create_dir_all(&cache_dir)?;
        std::fs::create_dir_all(&data_dir)?;

        let mut hasher = Sha1::new();
        hasher.update(base_path_str.as_bytes());
        let key = format!("{:x}", hasher.finalize());
        let key = &key[..12];

        let picker = SharedPicker::default();
        let frecency = SharedFrecency::default();
        let query_tracker = SharedQueryTracker::default();

        let frecency_db_path = cache_dir.join(format!("{key}-frecency.mdb"));
        if let Some(parent) = frecency_db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tracker = FrecencyTracker::new(&frecency_db_path, true).with_context(|| {
            format!(
                "failed to init frecency db at {}",
                frecency_db_path.display()
            )
        })?;
        frecency.init(tracker)?;

        let history_db_path = data_dir.join(format!("{key}-history.mdb"));
        if let Some(parent) = history_db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let query_db = QueryTracker::new(&history_db_path, true).with_context(|| {
            format!(
                "failed to init query tracker db at {}",
                history_db_path.display()
            )
        })?;
        query_tracker.init(query_db)?;

        FilePicker::new_with_shared_state(
            picker.clone(),
            frecency,
            FilePickerOptions {
                base_path: base_path_str.clone(),
                warmup_mmap_cache: false,
                mode: FFFMode::Neovim,
                cache_budget: None,
                watch: true,
            },
        )?;

        let _ = picker.wait_for_scan(Duration::from_millis(500));

        Ok(Self {
            base_path,
            root_display: base_path_str,
            picker,
            query_tracker,
        })
    }

    pub fn root_display(&self) -> &str {
        &self.root_display
    }

    pub fn search(&self, query: &str) -> Result<FileSearchView> {
        let picker_guard = self.picker.read()?;
        let picker = picker_guard
            .as_ref()
            .context("file picker is not initialized")?;
        let tracker_guard = self.query_tracker.read()?;
        let query_tracker = tracker_guard.as_ref();
        let parser = QueryParser::default();
        let parsed = parser.parse(query);
        let result = FilePicker::fuzzy_search(
            picker.get_files(),
            &parsed,
            query_tracker,
            FuzzySearchOptions {
                max_threads: 0,
                current_file: None,
                project_path: Some(&self.base_path),
                combo_boost_score_multiplier: 100,
                min_combo_count: if query.is_empty() { 1 } else { 0 },
                pagination: PaginationArgs {
                    offset: 0,
                    limit: 200,
                },
            },
        );

        let matches = result
            .items
            .into_iter()
            .map(|item| FileMatch {
                path: item.path.clone(),
                relative_path: item.relative_path.clone(),
                file_name: item.file_name.clone(),
                git: git_kind(item.git_status),
                badge: frecency_badge(
                    item.total_frecency_score,
                    item.access_frecency_score,
                    item.modification_frecency_score,
                ),
            })
            .collect::<Vec<_>>();

        Ok(FileSearchView {
            loaded: matches.len(),
            matches,
            total_matched: result.total_matched,
            root_display: self.root_display.clone(),
        })
    }
}

pub struct HistorySearchEngine {
    _temp_dir: TempDir,
    picker: FilePicker,
    commands: Vec<String>,
    display_lines: Vec<String>,
}

impl HistorySearchEngine {
    pub fn new(commands: Vec<String>) -> Result<Self> {
        let temp_dir = tempfile::Builder::new()
            .prefix("fff-history-")
            .tempdir_in(cache_dir())
            .context("failed to create history work directory")?;
        let history_file = temp_dir.path().join("history.txt");
        let display_lines = commands
            .iter()
            .map(|command| sanitize_history_display(command))
            .collect::<Vec<_>>();
        std::fs::write(&history_file, display_lines.join("\n") + "\n")?;

        let mut picker = FilePicker::new(FilePickerOptions {
            base_path: temp_dir.path().display().to_string(),
            warmup_mmap_cache: false,
            mode: FFFMode::Neovim,
            cache_budget: None,
            watch: false,
        })?;
        picker.collect_files()?;

        Ok(Self {
            _temp_dir: temp_dir,
            picker,
            commands,
            display_lines,
        })
    }

    pub fn search(&self, query: &str) -> Result<HistorySearchView> {
        if query.is_empty() {
            let matches = self
                .commands
                .iter()
                .zip(self.display_lines.iter())
                .map(|(command, display)| HistoryMatch {
                    command: command.clone(),
                    display: display.clone(),
                    match_ranges: Vec::new(),
                })
                .collect::<Vec<_>>();

            return Ok(HistorySearchView {
                total_matched: matches.len(),
                matches,
            });
        }

        let parser = QueryParser::default();
        let parsed = parser.parse(query);
        let result = self.picker.grep(
            &parsed,
            &GrepSearchOptions {
                max_file_size: 10 * 1024 * 1024,
                max_matches_per_file: 5000,
                smart_case: true,
                file_offset: 0,
                page_limit: 5000,
                mode: GrepMode::Fuzzy,
                time_budget_ms: 0,
                before_context: 0,
                after_context: 0,
                classify_definitions: false,
            },
        );

        let total_matched = result.matches.len();
        let matches = result
            .matches
            .into_iter()
            .map(|item| {
                let idx = item.line_number.saturating_sub(1) as usize;
                let command = self.commands.get(idx).cloned().unwrap_or_default();
                let display = self.display_lines.get(idx).cloned().unwrap_or_default();
                let match_ranges = item
                    .match_byte_offsets
                    .into_iter()
                    .map(|(start, end)| (start as usize, end as usize))
                    .collect::<Vec<_>>();

                HistoryMatch {
                    command,
                    display,
                    match_ranges,
                }
            })
            .collect::<Vec<_>>();

        Ok(HistorySearchView {
            total_matched,
            matches,
        })
    }
}

pub fn grep_cli_search(options: &GrepCliOptions) -> Result<GrepCliResult> {
    let canonical_path = std::fs::canonicalize(&options.base_path).with_context(|| {
        format!(
            "failed to resolve search path {}",
            options.base_path.display()
        )
    })?;
    let metadata = std::fs::metadata(&canonical_path)
        .with_context(|| format!("failed to stat search path {}", canonical_path.display()))?;
    let (base_path, target_file) = if metadata.is_file() {
        let parent = canonical_path
            .parent()
            .context("search file has no parent directory")?
            .to_path_buf();
        (parent, Some(canonical_path))
    } else {
        (canonical_path, None)
    };

    let mut files = scan_files(&base_path);
    if let Some(target_file) = target_file.as_ref() {
        files.retain(|file| file.path == *target_file);
    }
    let parsed = parse_grep_query(&options.query);
    let mode = match options.mode {
        GrepCliMode::PlainText => GrepMode::PlainText,
        GrepCliMode::Regex => GrepMode::Regex,
        GrepCliMode::Fuzzy => GrepMode::Fuzzy,
    };
    let budget = ContentCacheBudget::default();
    let result = grep_search(
        &files,
        &parsed,
        &GrepSearchOptions {
            max_file_size: options.max_file_size,
            max_matches_per_file: options.max_matches_per_file,
            smart_case: options.smart_case,
            file_offset: 0,
            page_limit: options.page_limit,
            mode,
            time_budget_ms: 0,
            before_context: options.before_context,
            after_context: options.after_context,
            classify_definitions: false,
        },
        &budget,
        None,
        None,
        None,
    );

    let matches = result
        .matches
        .into_iter()
        .map(|item| {
            let path = result.files[item.file_index].relative_path.clone();
            GrepCliMatch {
                path,
                line_number: item.line_number,
                col: item.col,
                line_content: item.line_content,
                match_ranges: item
                    .match_byte_offsets
                    .into_iter()
                    .map(|(start, end)| (start as usize, end as usize))
                    .collect(),
                context_before: item.context_before,
                context_after: item.context_after,
            }
        })
        .collect::<Vec<_>>();

    Ok(GrepCliResult {
        matches,
        total_files: result.total_files,
        total_files_searched: result.total_files_searched,
        files_with_matches: result.files_with_matches,
    })
}

pub fn load_history_commands(stdin_data: &[u8], direction: HistoryDirection) -> Vec<String> {
    if !stdin_data.is_empty() {
        return stdin_data
            .split(|byte| *byte == 0)
            .filter_map(|chunk| {
                if chunk.is_empty() {
                    None
                } else {
                    String::from_utf8(chunk.to_vec()).ok()
                }
            })
            .collect();
    }

    let histfile = std::env::var_os("HISTFILE").map(PathBuf::from);
    read_history_fallback(histfile.as_deref(), direction)
}

pub fn selected_label(selected_index: usize, total: usize) -> String {
    if total == 0 {
        "0/0".to_string()
    } else {
        format!("{}/{}", selected_index + 1, total)
    }
}

pub fn clamp_selected(selected: usize, len: usize) -> usize {
    if len == 0 { 0 } else { min(selected, len - 1) }
}

pub fn move_selection_up(selected: usize, len: usize, wrap: bool) -> usize {
    if len == 0 {
        return 0;
    }
    if wrap {
        if selected == 0 { len - 1 } else { selected - 1 }
    } else {
        selected.saturating_sub(1)
    }
}

pub fn move_selection_down(selected: usize, len: usize, wrap: bool) -> usize {
    if len == 0 {
        return 0;
    }
    if wrap {
        (selected + 1) % len
    } else {
        min(selected + 1, len - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_extended_history_and_dedupes_backward() {
        let input = ": 1:0;git status\n: 2:0;pwd\n: 3:0;git status\n";
        let commands = parse_history_content(input, HistoryDirection::Backward);
        assert_eq!(commands, vec!["git status", "pwd"]);
    }

    #[test]
    fn parses_extended_history_and_dedupes_forward() {
        let input = ": 1:0;git status\n: 2:0;pwd\n: 3:0;git status\n";
        let commands = parse_history_content(input, HistoryDirection::Forward);
        assert_eq!(commands, vec!["git status", "pwd"]);
    }

    #[test]
    fn fuzzy_indices_follow_subsequence_order() {
        let indices = fuzzy_match_indices("ci/filter.nix", "cfilt");
        assert_eq!(indices, vec![0, 3, 4, 5, 6]);
    }

    #[test]
    fn fuzzy_indices_fail_when_query_is_not_subsequence() {
        assert!(fuzzy_match_indices("abc", "az").is_empty());
    }

    #[test]
    fn frecency_badge_thresholds_match_prototype() {
        assert_eq!(frecency_badge(0, 0, 0), None);
        assert_eq!(frecency_badge(1, 0, 0).unwrap().icon, "•");
        assert_eq!(frecency_badge(3, 0, 0).unwrap().icon, "✨");
        assert_eq!(frecency_badge(3, 4, 0).unwrap().icon, "⭐");
        assert_eq!(frecency_badge(3, 4, 6).unwrap().icon, "🔥");
    }

    #[test]
    fn truncates_long_base_without_dir() {
        let (dir, base) = truncate_path("foo/", "very-long-file-name.rs", 8);
        assert_eq!(dir, "");
        assert_eq!(base, "very-...");
    }

    #[test]
    fn sanitizes_multiline_history_for_display() {
        assert_eq!(
            sanitize_history_display("printf 'a\\n'\necho done"),
            "printf 'a\\n' ↩ echo done"
        );
    }

    #[test]
    fn selection_visibility_tracks_window() {
        assert_eq!(ensure_selection_visible(2, 0, 5), 0);
        assert_eq!(ensure_selection_visible(6, 0, 5), 2);
        assert_eq!(ensure_selection_visible(1, 3, 5), 1);
    }

    #[test]
    fn load_history_commands_prefers_stdin() {
        let data = b"git status\0pwd\0";
        let commands = load_history_commands(data, HistoryDirection::Backward);
        assert_eq!(commands, vec!["git status", "pwd"]);
    }

    #[test]
    fn grep_mode_prefers_fuzzy_over_fixed_strings() {
        assert_eq!(grep_cli_mode(false, false), GrepCliMode::Regex);
        assert_eq!(grep_cli_mode(true, false), GrepCliMode::PlainText);
        assert_eq!(grep_cli_mode(true, true), GrepCliMode::Fuzzy);
    }

    #[test]
    fn formats_grep_output_like_rg() {
        assert_eq!(
            format_grep_match("src/main.rs", 42, 4, "let value = 1;"),
            "src/main.rs:42:5:let value = 1;"
        );
        assert_eq!(
            format_grep_context("src/main.rs", 41, "fn main() {"),
            "src/main.rs-41-fn main() {"
        );
    }
}
