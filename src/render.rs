//! Pure tree/block rendering. No I/O — trivially testable.
//!
//! Tree layout (owner-approved, ui-ux-pro-max informed): repos are groups
//! separated by blank lines; worktrees hang on tree connectors
//! (`├─`/`└─`); one line per worktree when it fits the terminal width,
//! otherwise the branch gets its own line and `state · path` sits below.
//! The group header already names the directory (`./repo`), so the row
//! tail path is shown only for worktrees nested deeper (`· .wt/x`).
//! Branch names: type prefix up to the first `/` is dim, the rest bold.
//!
//! Block layout (`-b`): one full-width card per worktree with labeled
//! fields (branch / status / remote / last commit), labels dim, values
//! semantically colored, long values wrapped at the value column.
//!
//! `indent()` shifts the whole output right so it reads as belonging to
//! the running program. Colors are semantic tokens mapped to standard
//! ANSI SGR codes, so the terminal's own 16-color palette resolves
//! them — nothing is hardcoded.

use std::path::Path;

use crate::parse::{WorktreeEntry, WorktreeStatus};

// ── ANSI SGR tokens (terminal palette applies) ─────────────────────────
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const RED: &str = "\x1b[31m";

/// Visible width of a row prefix (`├─* ` / `    `).
const PREFIX_W: usize = 4;
/// Gap between branch and state on a single-line row.
const GAP: usize = 2;
/// Room reserved for `state · path` when capping the alignment column.
const MIN_TAIL: usize = 33;

/// Rendering knobs; terminal I/O stays in main.
pub struct RenderOpts {
    pub width: usize,
    pub color: bool,
    /// Folder-scan mode: group rows under bold repo-name headers.
    pub group_by_repo: bool,
    /// `-b`: one labeled card per worktree instead of the tree.
    pub blocks: bool,
    /// `-p` with `-b`: render detail lines (per-file breakdown data and the
    /// broken-symlink list) that the caller filled into the rows.
    pub detail: bool,
    /// Nerd Font glyphs (repo header, branch); `--no-icons` turns off.
    pub icons: bool,
}

/// Nerd Font glyphs (patched-font ranges; `--no-icons`/`NO_ICONS` fall back
/// to plain text). Branch = powerline, folder = FontAwesome.
const ICON_BRANCH: &str = "\u{e0a0}";
const ICON_FOLDER: &str = "\u{f07b}";
/// Visible width of `icon + space` when icons are on.
const ICON_W: usize = 2;

/// One display row (plain strings; coloring applied at render time).
#[derive(Clone)]
pub struct Row {
    /// Repository name — `Some` only in folder-scan mode.
    pub repo: Option<String>,
    /// Origin remote in compact form (`github.com/user/repo`); shown in
    /// group/card headers — the folder name alone isn't the repo identity.
    pub remote: Option<String>,
    pub current: bool,
    pub branch: String,
    /// Worktree path relative to the group root (`wt/x`, `../y`, `~/z`);
    /// `None` when the worktree IS the group root (header names it).
    pub subpath: Option<String>,
    pub state: Vec<StateSeg>,
    /// Broken symlinks in the worktree root (`-s`); 0 = unchecked or none.
    /// Invariant: `StateSeg::BrokenLinks(broken)` sits in `state` iff
    /// `broken > 0` and the entry is not bare.
    pub broken: u32,
    /// The broken symlinks themselves, sorted by name — `(name, target)`.
    /// Invariant: `broken == broken_list.len()`; the card list lines render
    /// in `-bps` (detail) iff this is non-empty.
    pub broken_list: Vec<(String, String)>,
    /// Last commit (block mode): age + subject, best-effort.
    pub last: Option<Last>,
    /// Dirty-file breakdown (block `-bp` mode).
    pub files: Vec<FileLine>,
}

/// Age-formatted last-commit info for block mode.
#[derive(Clone)]
pub struct Last {
    pub age: String,
    pub subject: String,
}

/// One dirty file (block `-bp` mode): status letters, path and
/// added/deleted line counts (`None` = binary or untracked).
#[derive(Clone)]
pub struct FileLine {
    pub letters: String,
    pub path: String,
    pub added: Option<u32>,
    pub deleted: Option<u32>,
}

/// Semantic state pieces; colored at render time via `seg_code`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateSeg {
    Bare,
    Clean,
    Dirty(u32),
    Ahead(u32),
    Behind(u32),
    NoUpstream,
    UpstreamGone,
    BrokenLinks(u32),
}

/// Builds a display row from entry + status (decisions precomputed by caller).
/// `root` is the group's own directory (repo in scan mode, cwd repo otherwise).
pub fn build_row(
    entry: &WorktreeEntry,
    status: &WorktreeStatus,
    current: bool,
    root: &Path,
    home: Option<&Path>,
    repo: Option<&str>,
    broken_list: Vec<(String, String)>,
) -> Row {
    // Bare entries have no root to scan: force count/list empty so a links
    // field can never render without its state seg (main guards !bare; this
    // makes it structural).
    let (broken, broken_list) =
        if entry.bare { (0, Vec::new()) } else { (broken_list.len() as u32, broken_list) };
    let mut state = state_segments(entry, status);
    if broken > 0 {
        state.push(StateSeg::BrokenLinks(broken));
    }
    Row {
        repo: repo.map(str::to_owned),
        remote: None,
        current,
        branch: entry.branch.clone().unwrap_or_else(|| "(detached)".to_owned()),
        subpath: subpath_of(&entry.path, root, home),
        state,
        broken,
        broken_list,
        last: None,
        files: Vec::new(),
    }
}

/// Tail path: `None` when the worktree IS the group root (`./` — header
/// already names it), else repo-relative without the `./` noise.
fn subpath_of(path: &Path, root: &Path, home: Option<&Path>) -> Option<String> {
    match relativize(path, root, home) {
        r if r == "./" => None,
        r => Some(r.trim_start_matches("./").to_owned()),
    }
}

/// Branch name coloring: type prefix up to the first `/` dim, rest bold.
fn branch_text(branch: &str, color: bool) -> String {
    match branch.split_once('/') {
        Some((prefix, rest)) => format!(
            "{}{}",
            paint(DIM, &format!("{prefix}/"), color),
            paint(BOLD, rest, color)
        ),
        None => paint(BOLD, branch, color),
    }
}

/// State as semantic segments (cleanliness first, then upstream info).
fn state_segments(entry: &WorktreeEntry, status: &WorktreeStatus) -> Vec<StateSeg> {
    if entry.bare {
        return vec![StateSeg::Bare];
    }
    let mut segs = vec![if status.dirty_files > 0 {
        StateSeg::Dirty(status.dirty_files)
    } else {
        StateSeg::Clean
    }];
    match &status.track {
        Some(t) if t.gone => segs.push(StateSeg::UpstreamGone),
        Some(t) => {
            if t.ahead > 0 {
                segs.push(StateSeg::Ahead(t.ahead));
            }
            if t.behind > 0 {
                segs.push(StateSeg::Behind(t.behind));
            }
        }
        None => segs.push(StateSeg::NoUpstream),
    }
    segs
}

fn seg_text(seg: &StateSeg) -> String {
    match seg {
        StateSeg::Bare => "bare".to_owned(),
        StateSeg::Clean => "clean".to_owned(),
        StateSeg::Dirty(n) => format!("{n} dirty"),
        StateSeg::Ahead(n) => format!("\u{2191}{n}"),
        StateSeg::Behind(n) => format!("\u{2193}{n}"),
        StateSeg::NoUpstream => "no upstream".to_owned(),
        StateSeg::UpstreamGone => "upstream gone".to_owned(),
        StateSeg::BrokenLinks(1) => "1 broken link".to_owned(),
        StateSeg::BrokenLinks(n) => format!("{n} broken links"),
    }
}

/// Semantic role → palette code (single mapping point, standard SGR only).
fn seg_code(seg: &StateSeg) -> &'static str {
    match seg {
        StateSeg::Bare => "",
        StateSeg::Clean => GREEN,
        StateSeg::Dirty(_) => YELLOW,
        StateSeg::Ahead(_) => CYAN,
        StateSeg::Behind(_) | StateSeg::UpstreamGone | StateSeg::BrokenLinks(_) => RED,
        StateSeg::NoUpstream => DIM,
    }
}

/// Path relative to scan root: `./`, `./x`; shallow `../y` (≤2 up);
/// deep-external falls back to `~`-abbreviated absolute.
pub fn relativize(path: &Path, root: &Path, home: Option<&Path>) -> String {
    if let Ok(rest) = path.strip_prefix(root) {
        return if rest.as_os_str().is_empty() {
            "./".to_owned()
        } else {
            format!("./{}", rest.display())
        };
    }
    if let Some(rel) = shallow_relative(path, root, 2) {
        return rel;
    }
    abbreviate_home(path, home)
}

/// Path relative to `root` via `..` when at most `max_up` levels up.
fn shallow_relative(path: &Path, root: &Path, max_up: usize) -> Option<String> {
    let p: Vec<_> = path.components().collect();
    let r: Vec<_> = root.components().collect();
    let common = p.iter().zip(&r).take_while(|(a, b)| a == b).count();
    let up = r.len() - common;
    if up == 0 || up > max_up {
        return None;
    }
    let mut s = "../".repeat(up);
    s.push_str(
        &p[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
    );
    Some(s)
}

/// `/home/u/x/y` → `~/x/y` when `home` matches.
fn abbreviate_home(path: &Path, home: Option<&Path>) -> String {
    let full = path.display().to_string();
    match home.and_then(|h| full.strip_prefix(&*h.display().to_string())) {
        Some(rest) => format!("~{rest}"),
        None => full,
    }
}

/// Unix age → compact human form (`5m`, `3h`, `2d`, `6w`, `4mo`, `1y`).
pub fn format_age(secs: u64) -> String {
    const MIN: u64 = 60;
    const HOUR: u64 = 60 * MIN;
    const DAY: u64 = 24 * HOUR;
    const WEEK: u64 = 7 * DAY;
    const MONTH: u64 = 30 * DAY;
    const YEAR: u64 = 365 * DAY;
    match secs {
        0..=59 => "<1m".to_owned(),
        s if s < HOUR => format!("{}m", s / MIN),
        s if s < DAY => format!("{}h", s / HOUR),
        s if s < WEEK => format!("{}d", s / DAY),
        s if s < MONTH => format!("{}w", s / WEEK),
        s if s < YEAR => format!("{}mo", s / MONTH),
        s => format!("{}y", s / YEAR),
    }
}

/// Prefixes every non-empty line with `n` spaces (whole-output shift,
/// tree and blocks alike); blank separators stay empty.
pub fn indent(s: &str, n: usize) -> String {
    let pad = " ".repeat(n);
    let mut out: String = s
        .lines()
        .map(|l| if l.is_empty() { String::new() } else { format!("{pad}{l}") })
        .collect::<Vec<_>>()
        .join("\n");
    if s.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Groups rows by repo (blank line between groups, bold headers) and
/// renders each group as a tree — or, in block mode, one card per row.
pub fn render_tree(rows: &[Row], opts: &RenderOpts) -> String {
    if opts.blocks {
        return render_blocks(rows, opts);
    }
    let mut out = String::new();
    let mut i = 0;
    while i < rows.len() {
        if !out.is_empty() {
            out.push('\n');
        }
        // Header key: the repo name (always present since block labels).
        // Without grouping the whole slice is one group — `end` must not
        // depend on key matching (the single-mode hang regression).
        let key = rows[i].repo.clone();
        let end = if opts.group_by_repo {
            i + rows[i..].iter().take_while(|r| r.repo.as_deref() == key.as_deref()).count()
        } else {
            rows.len()
        };
        if let Some(name) = &key {
            let icon = if opts.icons {
                format!("{} ", paint(CYAN, ICON_FOLDER, opts.color))
            } else {
                String::new()
            };
            out.push_str(&paint(&format!("{BOLD}{CYAN}"), &format!("{icon}./{name}"), opts.color));
            if let Some(remote) = rows[i].remote.as_deref() {
                out.push_str(&paint(DIM, &format!(" · {remote}"), opts.color));
            }
            out.push('\n');
        }
        out.push_str(&render_group(&rows[i..end], opts));
        i = end;
    }
    out
}

fn render_group(group: &[Row], opts: &RenderOpts) -> String {
    let fits: Vec<bool> = group
        .iter()
        .map(|r| fits_one_line(r, opts.width, opts.icons))
        .collect();
    let align_w = group
        .iter()
        .zip(&fits)
        .filter(|(_, f)| **f)
        .map(|(r, _)| branch_w(r, opts.icons))
        .max()
        .unwrap_or(0)
        .min(opts.width.saturating_sub(PREFIX_W + GAP + MIN_TAIL));
    group
        .iter()
        .enumerate()
        .map(|(i, r)| render_row(r, i + 1 == group.len(), align_w, opts))
        .collect()
}

/// Visible branch width including the icon slot when icons are on.
fn branch_w(row: &Row, icons: bool) -> usize {
    if icons { ICON_W + row.branch.chars().count() } else { row.branch.chars().count() }
}

fn render_row(row: &Row, last: bool, align_w: usize, opts: &RenderOpts) -> String {
    let conn = if last { "└─" } else { "├─" };
    let mark = if row.current {
        paint(&format!("{GREEN}{BOLD}"), "*", opts.color)
    } else {
        " ".to_owned()
    };
    let prefix = format!("{}{} ", paint(DIM, conn, opts.color), mark);
    let icon = if opts.icons {
        format!("{} ", paint(CYAN, ICON_BRANCH, opts.color))
    } else {
        String::new()
    };
    let branch = format!("{icon}{}", branch_text(&row.branch, opts.color));
    if fits_one_line(row, opts.width, opts.icons) {
        let pad = align_w.saturating_sub(branch_w(row, opts.icons)) + GAP;
        format!("{prefix}{branch}{}{}\n", " ".repeat(pad), tail_text(row, opts.color))
    } else {
        let cont = if last {
            " ".repeat(PREFIX_W)
        } else {
            format!("{}{}", paint(DIM, "│", opts.color), " ".repeat(PREFIX_W - 1))
        };
        format!("{prefix}{branch}\n{cont}{}\n", tail_text(row, opts.color))
    }
}

/// `state` tail; ` · path` appended only for non-root worktrees.
fn tail_text(row: &Row, color: bool) -> String {
    let state = row
        .state
        .iter()
        .map(|s| paint(seg_code(s), &seg_text(s), color))
        .collect::<Vec<_>>()
        .join(", ");
    match &row.subpath {
        None => state,
        Some(p) => format!("{state}{}{}", paint(DIM, " · ", color), paint(DIM, p, color)),
    }
}

/// One-line row: prefix + branch (+icon) + gap + state + separator + path ≤ width.
fn fits_one_line(row: &Row, width: usize, icons: bool) -> bool {
    let state_w: usize = row.state.iter().map(|s| seg_text(s).chars().count()).sum::<usize>()
        + 2 * row.state.len().saturating_sub(1);
    let path_w = row.subpath.as_ref().map_or(0, |p| 3 + p.chars().count());
    let icon_w = if icons { ICON_W } else { 0 };
    PREFIX_W + icon_w + row.branch.chars().count() + GAP + state_w + path_w <= width
}

/// Wraps `text` in ANSI codes when `color` is on.
fn paint(code: &str, text: &str, color: bool) -> String {
    if color {
        format!("{code}{text}{RESET}")
    } else {
        text.to_owned()
    }
}

// ── block mode (-b): one labeled full-width card per worktree ──────────

/// Label column: fixed field names (max `branch` = 6) + 2-space gap.
const LABEL_W: usize = 8;
/// Block width floor — guards degenerate narrow terminals.
const MIN_BLOCK_W: usize = 24;

fn render_blocks(rows: &[Row], opts: &RenderOpts) -> String {
    // Each card ends with a newline; the join adds the blank separator.
    rows.iter().map(|r| render_block(r, opts)).collect::<Vec<_>>().join("\n")
}

fn render_block(row: &Row, opts: &RenderOpts) -> String {
    // Closed box, one spare column on the right: the card never touches the
    // terminal's autowrap-prone last column and reads as a finished block.
    // Long broken-symlink targets widen the card instead of spilling over
    // the right border (list lines render in detail mode only, but the
    // width must hold whenever they do).
    let links_w = if opts.detail {
        row.broken_list
            .iter()
            .map(|(name, target)| {
                // │ + space + label column + name + " -> " + target + pad(≥1) + │
                2 + 1 + LABEL_W + name.chars().count() + 4 + target.chars().count() + 1
            })
            .max()
            .unwrap_or(0)
    } else {
        0
    };
    let w = opts.width.saturating_sub(1).max(MIN_BLOCK_W).max(links_w);
    let budget = w.saturating_sub(2 + LABEL_W + 1);
    let label = block_label(row);
    let marker = if row.current {
        paint(&format!("{GREEN}{BOLD}"), " *", opts.color)
    } else {
        String::new()
    };
    let remote_seg = row
        .remote
        .as_deref()
        .map(|r| paint(DIM, &format!(" · {r}"), opts.color))
        .unwrap_or_default();
    let used = 3 + label.chars().count()
        + if opts.icons { ICON_W } else { 0 }
        + if row.current { 2 } else { 0 }
        + row.remote.as_deref().map_or(0, |r| 3 + r.chars().count())
        + 1;
    let icon = if opts.icons {
        format!("{} ", paint(CYAN, ICON_FOLDER, opts.color))
    } else {
        String::new()
    };
    let fill = w.saturating_sub(used + 1).max(1);
    let mut out = format!(
        "{}{}{}{}{} {}{}\n",
        paint(DIM, "┌─ ", opts.color),
        icon,
        paint(&format!("{BOLD}{CYAN}"), &label, opts.color),
        marker,
        remote_seg,
        paint(DIM, &"─".repeat(fill), opts.color),
        paint(DIM, "┐", opts.color),
    );
    for (name, lines) in block_fields(row, budget, opts.color, opts.detail) {
        for (i, line) in lines.iter().enumerate() {
            let content = if i == 0 {
                format!(" {:<6}  {line}", name)
            } else {
                format!(" {}{line}", " ".repeat(LABEL_W))
            };
            let pad = " ".repeat(w.saturating_sub(2 + visible_len(&content)));
            out.push_str(&format!(
                "{}{content}{pad}{}\n",
                paint(DIM, "│", opts.color),
                paint(DIM, "│", opts.color)
            ));
        }
    }
    out.push_str(&paint(
        DIM,
        &format!("└{}┘", "─".repeat(w.saturating_sub(2))),
        opts.color,
    ));
    out.push('\n');
    out
}

/// Card header path: scan mode `./repo[/.wt/x]`; anchored run: the anchor
/// (`./repo`) — or the subpath alone when it escapes the anchor root
/// (`../sibling`, `~/wt`); `./repo/../x` would be noise.
fn block_label(row: &Row) -> String {
    match (&row.repo, &row.subpath) {
        (Some(r), None) => format!("./{r}"),
        (Some(_), Some(s)) if s.starts_with("..") || s.starts_with('~') => s.clone(),
        (Some(r), Some(s)) => format!("./{r}/{s}"),
        (None, None) => "./".to_owned(),
        (None, Some(s)) if s.starts_with("..") || s.starts_with('~') => s.clone(),
        (None, Some(s)) => format!("./{s}"),
    }
}

/// Labeled field lines (branch / status / remote / links / last). Branch and
/// last wrap at the value column; status/remote/links are short single-liners
/// (links gains one continuation line per broken symlink in detail mode).
fn block_fields(row: &Row, budget: usize, color: bool, detail: bool) -> Vec<(&'static str, Vec<String>)> {
    let mut fields = vec![(
        "branch",
        char_chunks(&row.branch, budget)
            .iter()
            .enumerate()
            .map(|(i, c)| if i == 0 { branch_text(c, color) } else { paint(BOLD, c, color) })
            .collect::<Vec<_>>(),
    )];
    if let Some(first) = row.state.first() {
        let mut lines = vec![paint(seg_code(first), &seg_text(first), color)];
        lines.extend(file_lines(&row.files, color));
        let more = row.files.len().saturating_sub(MAX_FILES);
        if more > 0 {
            lines.push(paint(DIM, &format!("… +{more} more"), color));
        }
        fields.push(("status", lines));
    }
    let remote: Vec<_> = row
        .state
        .iter()
        .skip(1)
        // Broken links get their own card field below — not remote state.
        .filter(|s| !matches!(s, StateSeg::BrokenLinks(_)))
        .collect();
    if !remote.is_empty() {
        let text = remote
            .iter()
            .map(|s| paint(seg_code(s), &seg_text(s), color))
            .collect::<Vec<_>>()
            .join(", ");
        fields.push(("remote", vec![text]));
    }
    if row.broken > 0 {
        let mut lines = vec![paint(RED, &format!("{} broken", row.broken), color)];
        if detail && !row.broken_list.is_empty() {
            lines.extend(broken_link_lines(&row.broken_list, color));
        }
        fields.push(("links", lines));
    }
    if let Some(last) = &row.last {
        let plain = format!("{} · {}", last.age, last.subject);
        fields.push((
            "last",
            char_chunks(&plain, budget).iter().map(|c| paint(DIM, c, color)).collect(),
        ));
    }
    fields
}

/// Splits `s` into chunks of at most `n` chars (ANSI-free input only).
fn char_chunks(s: &str, n: usize) -> Vec<String> {    if n == 0 {
        return vec![s.to_owned()];
    }
    s.chars()
        .collect::<Vec<_>>()
        .chunks(n)
        .map(|c| c.iter().collect::<String>())
        .collect()
}

/// Visible width of `s`: chars minus ANSI SGR sequences (`\x1b[…m`).
fn visible_len(s: &str) -> usize {
    let mut n = 0;
    let mut in_escape = false;
    for c in s.chars() {
        match (in_escape, c) {
            (false, '\x1b') => in_escape = true,
            (true, 'm') => in_escape = false,
            (true, _) => {}
            (false, _) => n += 1,
        }
    }
    n
}

/// Broken-symlink list lines under the `links` count (`-bps`):
/// `name -> target`, name plain, arrow and target dim.
fn broken_link_lines(links: &[(String, String)], color: bool) -> Vec<String> {
    links
        .iter()
        .map(|(name, target)| format!("{name}{}", paint(DIM, &format!(" -> {target}"), color)))
        .collect()
}

/// Per-file continuation lines under the `status` field (`-bp`):
/// `M modified  src/a.rs  +12 −3` — letters/label dim, counts colored.
fn file_lines(files: &[FileLine], color: bool) -> Vec<String> {
    let path_w = files.iter().map(|f| f.path.chars().count()).max().unwrap_or(0).min(28);
    files
        .iter()
        .take(MAX_FILES)
        .map(|f| {
            let letters = paint(DIM, &f.letters, color);
            let label = paint(DIM, change_label(&f.letters), color);
            let path = format!("{:<path_w$}", f.path, path_w = path_w);
            let counts = match (f.added, f.deleted) {
                (Some(a), Some(d)) => format!(
                    "{}{}",
                    paint(GREEN, &format!("+{a}"), color),
                    paint(RED, &format!(" −{d}"), color)
                ),
                _ => String::new(),
            };
            let line = format!("{letters} {label}  {path}  {counts}");
            if counts.is_empty() {
                line.trim_end().to_owned()
            } else {
                line
            }
        })
        .collect()
}

/// Max dirty files listed per card before the `… +N more` note.
const MAX_FILES: usize = 10;

/// Human word for porcelain status letters (first significant letter wins).
fn change_label(letters: &str) -> &'static str {
    let c = letters.chars().find(|&c| c != ' ').unwrap_or('M');
    match c {
        'A' => "added",
        'D' => "deleted",
        'R' => "renamed",
        'C' => "copied",
        'U' => "unmerged",
        '?' => "untracked",
        _ => "modified",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::TrackInfo;
    use std::path::PathBuf;

    fn entry(branch: Option<&str>, path: &str) -> WorktreeEntry {
        WorktreeEntry {
            path: PathBuf::from(path),
            branch: branch.map(str::to_owned),
            bare: false,
        }
    }

    fn status(dirty: u32, ahead: u32, behind: u32) -> WorktreeStatus {
        WorktreeStatus {
            dirty_files: dirty,
            track: Some(TrackInfo { ahead, behind, gone: false }),
        }
    }

    fn wide_opts() -> RenderOpts {
        RenderOpts { width: 200, color: false, group_by_repo: false, blocks: false, icons: false, detail: false }
    }

    // ── state segments ────────────────────────────────────────────────

    #[test]
    fn state_segments_variants() {
        let e = entry(Some("main"), "/r");
        assert_eq!(state_segments(&e, &status(0, 0, 0)), vec![StateSeg::Clean]);
        assert_eq!(
            state_segments(&e, &status(3, 2, 1)),
            vec![StateSeg::Dirty(3), StateSeg::Ahead(2), StateSeg::Behind(1)]
        );
        assert_eq!(
            state_segments(&e, &WorktreeStatus::default()),
            vec![StateSeg::Clean, StateSeg::NoUpstream]
        );
        let gone = WorktreeStatus {
            dirty_files: 1,
            track: Some(TrackInfo { ahead: 0, behind: 0, gone: true }),
        };
        assert_eq!(
            state_segments(&e, &gone),
            vec![StateSeg::Dirty(1), StateSeg::UpstreamGone]
        );
        let bare = WorktreeEntry { path: PathBuf::from("/r"), branch: None, bare: true };
        assert_eq!(state_segments(&bare, &WorktreeStatus::default()), vec![StateSeg::Bare]);
    }

    // ── relativize ────────────────────────────────────────────────────

    #[test]
    fn relativize_inside_root_and_root_itself() {
        assert_eq!(relativize(Path::new("/r/wt"), Path::new("/r"), None), "./wt");
        assert_eq!(relativize(Path::new("/r"), Path::new("/r"), None), "./");
    }

    #[test]
    fn relativize_shallow_up_then_home_fallback() {
        assert_eq!(relativize(Path::new("/a/b/other"), Path::new("/a/b/repo"), None), "../other");
        // 3 levels up exceeds the shallow budget → home-abbreviated absolute.
        assert_eq!(
            relativize(Path::new("/x/y/z"), Path::new("/a/b/c/d"), Some(Path::new("/x"))),
            "~/y/z"
        );
    }

    #[test]
    fn build_row_relative_path_and_detached() {
        let row = build_row(
            &entry(None, "/r/.wt/x"),
            &WorktreeStatus::default(),
            true,
            Path::new("/r"),
            None,
            None,
            Vec::new(),
        );
        assert_eq!(row.branch, "(detached)");
        assert_eq!(row.subpath.as_deref(), Some(".wt/x"));
        assert!(row.current);
        // The root worktree itself has no tail path — header names it.
        let root_row =
            build_row(&entry(Some("main"), "/r"), &WorktreeStatus::default(), false, Path::new("/r"), None, None, Vec::new());
        assert_eq!(root_row.subpath, None);
    }

    // ── branch coloring ───────────────────────────────────────────────

    #[test]
    fn branch_prefix_is_dim_rest_is_bold() {
        assert_eq!(branch_text("feature/x", false), "feature/x");
        assert_eq!(branch_text("main", false), "main");
        let colored = branch_text("feature/AREN-1/rest", true);
        assert!(colored.starts_with(&format!("{DIM}feature/{RESET}{BOLD}")));
        assert!(colored.ends_with(&format!("AREN-1/rest{RESET}")));
        assert_eq!(branch_text("main", true), format!("{BOLD}main{RESET}"));
    }

    // ── tree layout ───────────────────────────────────────────────────

    fn long_branch_row(repo: Option<&str>, current: bool) -> Row {
        Row {
            repo: repo.map(str::to_owned),
            remote: None,
            current,
            branch: "feature/ARENZAPRO-3925/3_supplier_manager_select_on_application_create"
                .to_owned(),
            subpath: Some("support_tasks/.wt/x".to_owned()),
            state: vec![StateSeg::Dirty(3), StateSeg::Ahead(3)],
            broken: 0,
            broken_list: Vec::new(),
            last: None,
            files: Vec::new(),
        }
    }

    #[test]
    fn tree_connectors_marker_and_single_lines() {
        let rows = vec![
            build_row(&entry(Some("develop"), "/r"), &status(0, 0, 1), true, Path::new("/r"), None, None, Vec::new()),
            long_branch_row(None, false),
            build_row(&entry(Some("staging"), "/r/.wt/s"), &status(1, 0, 11), false, Path::new("/r"), None, None, Vec::new()),
        ];
        let out = render_tree(&rows, &wide_opts());
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("\u{251c}\u{2500}* develop"));
        assert!(lines[1].starts_with("\u{251c}\u{2500}  feature/"));
        assert!(lines[2].starts_with("\u{2514}\u{2500}  staging"));
        assert!(lines[0].ends_with("clean, \u{2193}1"), "root worktree: no tail path");
        assert!(lines[2].ends_with("1 dirty, \u{2193}11 · .wt/s"));
    }

    #[test]
    fn tree_aligns_state_column_for_single_lines() {
        let rows = vec![
            build_row(&entry(Some("develop"), "/r"), &status(0, 0, 0), false, Path::new("/r"), None, None, Vec::new()),
            long_branch_row(None, false),
            build_row(&entry(Some("staging"), "/r/.wt/s"), &status(1, 0, 0), false, Path::new("/r"), None, None, Vec::new()),
        ];
        let out = render_tree(&rows, &wide_opts());
        let lines: Vec<&str> = out.lines().collect();
        let a = lines[0].find("clean").unwrap();
        let b = lines[2].find("1 dirty").unwrap();
        assert_eq!(a, b, "state columns must align");
    }

    #[test]
    fn tree_wraps_long_branch_into_two_lines() {
        let rows = vec![
            build_row(&entry(Some("develop"), "/r"), &status(0, 0, 0), false, Path::new("/r"), None, None, Vec::new()),
            long_branch_row(None, false),
            build_row(&entry(Some("staging"), "/r/.wt/s"), &status(1, 0, 0), false, Path::new("/r"), None, None, Vec::new()),
        ];
        let opts = RenderOpts { width: 80, color: false, group_by_repo: false, blocks: false, icons: false, detail: false };
        let out = render_tree(&rows, &opts);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 4, "long branch row takes two lines");
        assert!(lines[1].starts_with("\u{251c}\u{2500}  feature/ARENZAPRO"));
        assert!(lines[2].starts_with("\u{2502}   3 dirty"));
        assert!(lines[2].ends_with("· support_tasks/.wt/x"));
        // Last row wraps too (short branch, but width budget shared): continuation is blank.
        let last_opts = RenderOpts { width: 10, color: false, group_by_repo: false, blocks: false, icons: false, detail: false };
        let solo = vec![build_row(&entry(Some("m"), "/r"), &status(0, 0, 0), false, Path::new("/r"), None, None, Vec::new())];
        let solo_out = render_tree(&solo, &last_opts);
        let solo_lines: Vec<&str> = solo_out.lines().collect();
        assert_eq!(solo_lines.len(), 2);
        assert!(solo_lines[1].starts_with("    clean"));
    }

    #[test]
    fn tree_scan_mode_group_headers_and_blank_lines() {
        let rows = vec![
            build_row(&entry(Some("main"), "/r/api"), &status(0, 0, 0), false, Path::new("/r"), None, Some("api"), Vec::new()),
            build_row(&entry(Some("dev"), "/r/web"), &status(0, 0, 0), false, Path::new("/r"), None, Some("web"), Vec::new()),
        ];
        let opts = RenderOpts { width: 200, color: false, group_by_repo: true, blocks: false, icons: false, detail: false };
        let out = render_tree(&rows, &opts);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "./api");
        assert!(lines[1].starts_with("\u{2514}\u{2500}  main"));
        assert_eq!(lines[2], "", "blank line between groups");
        assert_eq!(lines[3], "./web");
    }

    #[test]
    fn tree_ungrouped_mode_renders_header_and_all_rows() {
        // Regression 1: ungrouped rendering must not scan for repo == None
        // (used to hang). Regression 2 (owner report): single-repo mode must
        // still show the `./repo` header — a bare `└─* main` looked like a
        // nameless root.
        let rows = vec![
            build_row(&entry(Some("main"), "/r/crm"), &status(0, 0, 0), true, Path::new("/r/crm"), None, Some("crm"), Vec::new()),
            build_row(&entry(Some("dev"), "/r/crm/.wt/w"), &status(1, 0, 0), false, Path::new("/r/crm"), None, None, Vec::new()),
        ];
        let out = render_tree(&rows, &wide_opts());
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "header + both rows: {out:?}");
        assert_eq!(lines[0], "./crm", "repo header present in ungrouped mode");
        assert!(lines[1].contains("main"));
        assert!(lines[2].contains("dev"));
    }

    #[test]
    fn origin_shown_in_tree_and_block_headers() {
        let mut row = build_row(&entry(Some("main"), "/r/api"), &status(0, 0, 0), false, Path::new("/r"), None, Some("api"), Vec::new());
        row.remote = Some("github.com/apilot/lsw".to_owned());
        let tree = render_tree(&[row.clone()], &wide_opts());
        assert!(tree.contains("./api · github.com/apilot/lsw"), "tree header: {tree:?}");
        let blocks = RenderOpts { width: 80, color: false, group_by_repo: true, blocks: true, icons: false, detail: false };
        let card = render_tree(&[row], &blocks);
        let first = card.lines().next().unwrap();
        assert!(first.contains("· github.com/apilot/lsw"), "block header: {first:?}");
        assert!(first.ends_with('┐'), "still closed with remote");
        assert_eq!(first.chars().count(), 79, "card width = width−1, fill shrinks for remote");
    }

    #[test]
    fn icons_render_in_headers_and_rows_and_turn_off() {        let rows = vec![
            build_row(&entry(Some("main"), "/r/api"), &status(0, 0, 0), false, Path::new("/r"), None, Some("api"), Vec::new()),
        ];
        let on = RenderOpts { width: 200, color: false, group_by_repo: true, blocks: false, icons: true, detail: false };
        let out = render_tree(&rows, &on);
        assert!(out.contains("\u{f07b}"), "folder icon on repo header");
        assert!(out.contains("\u{e0a0}"), "branch icon on row");
        let off_opts = RenderOpts { width: 200, color: false, group_by_repo: true, blocks: false, icons: false, detail: false };
        let plain = render_tree(&rows, &off_opts);
        assert!(!plain.contains('\u{f07b}') && !plain.contains('\u{e0a0}'), "icons off → ascii only");
    }

    #[test]
    fn block_header_carries_folder_icon() {
        let rows = vec![build_row(
            &entry(Some("main"), "/r/api"), &status(0, 0, 0), false, Path::new("/r"), None, Some("api"), Vec::new(),
        )];
        let on = RenderOpts { width: 60, color: false, group_by_repo: true, blocks: true, icons: true, detail: false };
        let out = render_tree(&rows, &on);
        let first = out.lines().next().unwrap();
        assert!(first.starts_with("┌─ \u{f07b} ./api"), "header: {first:?}");
        assert!(first.ends_with('┐'));
    }

    #[test]
    fn block_header_shows_anchored_sibling_subpath_alone() {
        // Anchored run: a sibling worktree outside the anchor root — the
        // header is the clean `../wt-a`, not the noisy `./proj/../wt-a`.
        let row = Row {
            repo: Some("proj".to_owned()),
            remote: None,
            current: true,
            branch: "a".to_owned(),
            subpath: Some("../wt-a".to_owned()),
            state: vec![StateSeg::Clean],
            broken: 0,
            broken_list: Vec::new(),
            last: None,
            files: Vec::new(),
        };
        let opts =
            RenderOpts { width: 60, color: false, group_by_repo: true, blocks: true, icons: false, detail: false };
        let first = render_tree(&[row], &opts).lines().next().unwrap().to_owned();
        assert!(first.contains("┌─ ../wt-a"), "header: {first:?}");
        assert!(!first.contains("/.."), "no unnormalized up-path: {first:?}");
    }

    #[test]
    fn block_header_joins_nested_subpath_under_repo() {
        // Scan guard: a worktree nested in the repo stays joined — `./repo/.wt/x`.
        let row = Row {
            repo: Some("proj".to_owned()),
            remote: None,
            current: false,
            branch: "fix".to_owned(),
            subpath: Some(".wt/fix-1".to_owned()),
            state: vec![StateSeg::Clean],
            broken: 0,
            broken_list: Vec::new(),
            last: None,
            files: Vec::new(),
        };
        let opts =
            RenderOpts { width: 60, color: false, group_by_repo: true, blocks: true, icons: false, detail: false };
        let first = render_tree(&[row], &opts).lines().next().unwrap().to_owned();
        assert!(first.contains("┌─ ./proj/.wt/fix-1"), "header: {first:?}");
    }

    #[test]
    fn icon_width_counts_toward_fit() {
        // Same row: fits at 24 without icons, wraps with icons (+2 chars).
        let row = build_row(&entry(Some("m"), "/r"), &status(0, 0, 0), false, Path::new("/r"), None, None, Vec::new());
        assert!(fits_one_line(&row, 13, false));
        assert!(!fits_one_line(&row, 13, true));
    }

    // ── colors ────────────────────────────────────────────────────────

    #[test]
    fn tree_colors_are_semantic_ansi() {
        let rows = vec![
            build_row(&entry(Some("develop"), "/r"), &status(0, 1, 2), true, Path::new("/r"), None, None, Vec::new()),
        ];
        let opts = RenderOpts { width: 200, color: true, group_by_repo: false, blocks: false, icons: false, detail: false };
        let out = render_tree(&rows, &opts);
        assert!(out.contains(GREEN), "marker/clean green");
        assert!(out.contains(CYAN), "ahead cyan");
        assert!(out.contains(RED), "behind red");
        assert!(out.contains(BOLD) && out.contains(DIM) && out.contains(RESET));
    }

    #[test]
    fn tree_no_color_is_plain() {
        let rows = vec![build_row(&entry(Some("m"), "/r"), &status(2, 0, 0), false, Path::new("/r"), None, None, Vec::new())];
        assert!(!render_tree(&rows, &wide_opts()).contains('\x1b'));
    }

    // ── iteration 5: indent / age / block cards / file breakdown ──────

    #[test]
    fn format_age_buckets() {
        assert_eq!(format_age(30), "<1m");
        assert_eq!(format_age(5 * 60), "5m");
        assert_eq!(format_age(3 * 3600), "3h");
        assert_eq!(format_age(2 * 86400), "2d");
        assert_eq!(format_age(3 * 604800), "3w");
        assert_eq!(format_age(2 * 2592000), "2mo");
        assert_eq!(format_age(400 * 86400), "1y");
    }

    #[test]
    fn indent_shifts_every_line_keeps_trailing_newline() {
        assert_eq!(indent("a\nb\n", 4), "    a\n    b\n");
        assert_eq!(indent("a\nb", 2), "  a\n  b");
        assert_eq!(indent("a\n\nb\n", 2), "  a\n\n  b\n", "blank lines stay empty");
    }

    fn dirty_row() -> Row {
        let mut row = build_row(
            &entry(Some("feature/AREN-1/do_stuff"), "/w/crm"),
            &WorktreeStatus { dirty_files: 3, track: None },
            false,
            Path::new("/w/crm"),
            None,
            Some("crm"),
            Vec::new(),
        );
        row.last = Some(Last { age: "2h".to_owned(), subject: "fix core".to_owned() });
        row.files = vec![
            FileLine {
                letters: "M".to_owned(),
                path: "src/main.rs".to_owned(),
                added: Some(12),
                deleted: Some(3),
            },
            FileLine {
                letters: "??".to_owned(),
                path: "notes.txt".to_owned(),
                added: None,
                deleted: None,
            },
        ];
        row
    }

    #[test]
    fn block_card_layout_fields_and_width() {
        let opts = RenderOpts { width: 60, color: false, group_by_repo: true, blocks: true, icons: false, detail: false };
        let out = render_tree(&[dirty_row()], &opts);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 8, "header + 4 fields (status has 3 lines) + footer");
        assert!(lines[0].starts_with("┌─ ./crm"), "header: {}", lines[0]);
        assert_eq!(lines[0].chars().count(), 59, "card = width − 4-indent budget − 1 inset");
        assert!(lines[0].ends_with('┐'), "top border closed");
        assert!(lines[1].starts_with("│ branch  feature/AREN-1/do_stuff"));
        assert!(lines[1].ends_with('│'), "field line closed");
        assert!(lines[2].starts_with("│ status  3 dirty"));
        assert!(lines[3].starts_with("│         M modified  src/main.rs  +12 −3"));
        assert!(lines[4].starts_with("│         ?? untracked  notes.txt"));
        assert!(lines[5].starts_with("│ remote  no upstream"));
        assert!(lines[6].starts_with("│ last    2h · fix core"));
        assert!(lines[7].starts_with('└') && lines[7].ends_with('┘'));
        assert_eq!(lines[7].chars().count(), 59);
    }

    #[test]
    fn block_cards_separated_by_blank_line() {
        let opts = RenderOpts { width: 60, color: false, group_by_repo: true, blocks: true, icons: false, detail: false };
        let mut second = dirty_row();
        second.repo = Some("web".to_owned());
        let out = render_tree(&[dirty_row(), second], &opts);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[8], "", "blank line between cards");
        assert!(lines[9].starts_with("┌─ ./web"));
    }

    #[test]
    fn block_wraps_long_branch_at_value_column() {
        let opts = RenderOpts { width: 30, color: false, group_by_repo: true, blocks: true, icons: false, detail: false };
        let out = render_tree(&[dirty_row()], &opts);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[1].starts_with("│ branch  feature/AREN-1/do_"), "wrapped at budget 18");
        assert!(lines[2].starts_with("│         stuff"), "continuation aligned at value col");
        assert_eq!(lines.len(), 9, "one extra line from the wrap");
    }

    #[test]
    fn block_colors_are_semantic() {
        let opts = RenderOpts { width: 60, color: true, group_by_repo: true, blocks: true, icons: false, detail: false };
        let out = render_tree(&[dirty_row()], &opts);
        assert!(out.contains(&format!("{BOLD}{CYAN}")), "header bold cyan");
        assert!(out.contains(YELLOW), "dirty yellow");
        assert!(out.contains(GREEN) && out.contains(RED), "+green −red counts");
        assert!(out.contains(DIM) && out.contains(BOLD) && out.contains(RESET));
    }

    #[test]
    fn block_files_capped_with_note() {
        let mut row = dirty_row();
        row.files = (0..13)
            .map(|i| FileLine {
                letters: "M".to_owned(),
                path: format!("f{i}.rs"),
                added: Some(1),
                deleted: Some(0),
            })
            .collect();
        let opts = RenderOpts { width: 80, color: false, group_by_repo: true, blocks: true, icons: false, detail: false };
        let out = render_tree(&[row], &opts);
        assert!(out.contains("… +3 more"), "13 files − 10 shown = 3 more");
    }

    #[test]
    fn change_labels_decode_porcelain_letters() {
        assert_eq!(change_label("M"), "modified");
        assert_eq!(change_label(" M"), "modified");
        assert_eq!(change_label("A"), "added");
        assert_eq!(change_label("D"), "deleted");
        assert_eq!(change_label("R"), "renamed");
        assert_eq!(change_label("??"), "untracked");
        assert_eq!(change_label("U"), "unmerged");
    }

    #[test]
    fn visible_len_skips_ansi() {
        assert_eq!(visible_len("plain"), 5);
        assert_eq!(visible_len("\x1b[1mabc\x1b[0m"), 3);
        assert_eq!(visible_len("a\x1b[31;1mb\nc"), 4, "each char counts, incl newline");
    }

    // ── broken symlinks (`-s`) ────────────────────────────────────────

    #[test]
    fn broken_links_seg_text_and_color() {
        assert_eq!(seg_text(&StateSeg::BrokenLinks(1)), "1 broken link");
        assert_eq!(seg_text(&StateSeg::BrokenLinks(2)), "2 broken links");
        assert_eq!(seg_code(&StateSeg::BrokenLinks(2)), RED);
    }

    #[test]
    fn build_row_appends_broken_seg_only_when_positive_and_not_bare() {
        let row = build_row(
            &entry(Some("main"), "/r"), &status(0, 0, 0), false, Path::new("/r"), None, None,
            vec![("a".to_owned(), "x".to_owned()), ("b".to_owned(), "y".to_owned())],
        );
        assert_eq!(row.broken, 2);
        assert_eq!(row.state.last(), Some(&StateSeg::BrokenLinks(2)), "seg after existing state segs");
        let zero = build_row(
            &entry(Some("main"), "/r"), &status(0, 0, 0), false, Path::new("/r"), None, None, Vec::new(),
        );
        assert_eq!(zero.broken, 0);
        assert!(!zero.state.iter().any(|s| matches!(s, StateSeg::BrokenLinks(_))));
        let bare = WorktreeEntry { path: PathBuf::from("/r"), branch: None, bare: true };
        let bare_row = build_row(&bare, &WorktreeStatus::default(), false, Path::new("/r"), None, None, vec![("x".to_owned(), "y".to_owned())]);
        assert_eq!(bare_row.state, vec![StateSeg::Bare], "bare: never a broken seg");
        assert_eq!(bare_row.broken, 0, "bare: count forced to 0");
        assert!(bare_row.broken_list.is_empty(), "bare: list forced empty");
    }

    #[test]
    fn tree_tail_shows_broken_links_token() {
        let rows = vec![build_row(
            &entry(Some("main"), "/r"), &status(0, 0, 0), false, Path::new("/r"), None, None,
            vec![("a".to_owned(), "x".to_owned()), ("b".to_owned(), "y".to_owned())],
        )];
        let out = render_tree(&rows, &wide_opts());
        assert!(out.contains("clean, 2 broken links"), "tree tail: {out:?}");
    }

    #[test]
    fn block_card_links_field_after_remote_not_merged_into_it() {
        let row = build_row(
            &entry(Some("main"), "/r"), &status(0, 1, 2), false, Path::new("/r"), None, None,
            vec![("a".to_owned(), "x".to_owned()), ("b".to_owned(), "y".to_owned())],
        );
        let plain = RenderOpts { width: 80, color: false, group_by_repo: false, blocks: true, icons: false, detail: false };
        let out = render_tree(std::slice::from_ref(&row), &plain);
        let lines: Vec<&str> = out.lines().collect();
        let remote = lines.iter().position(|l| l.starts_with("│ remote")).expect("remote field present");
        let links = lines.iter().position(|l| l.starts_with("│ links   2 broken")).expect("links field present");
        assert!(remote < links, "links card field comes after remote");
        assert!(lines[remote].contains("↑1, ↓2"), "remote keeps upstream text: {:?}", lines[remote]);
        assert!(!lines[remote].contains("broken"), "broken must not leak into remote: {:?}", lines[remote]);
        let colored = RenderOpts { width: 80, color: true, group_by_repo: false, blocks: true, icons: false, detail: false };
        let colored_out = render_tree(&[row], &colored);
        assert!(colored_out.contains(&format!("{RED}2 broken{RESET}")), "links value red: {colored_out:?}");
    }

    #[test]
    fn block_card_without_broken_has_no_links_field() {
        let row = build_row(
            &entry(Some("main"), "/r"), &status(0, 0, 0), false, Path::new("/r"), None, None, Vec::new(),
        );
        let opts = RenderOpts { width: 80, color: false, group_by_repo: false, blocks: true, icons: false, detail: false };
        let out = render_tree(&[row], &opts);
        assert!(!out.lines().any(|l| l.starts_with("│ links")), "no links field when broken = 0");
    }

    #[test]
    fn block_detail_mode_without_broken_has_no_links_field() {
        // `-bp` without `-s`: detail rendering active, but nothing scanned.
        let row = build_row(
            &entry(Some("main"), "/r"), &status(0, 0, 0), false, Path::new("/r"), None, None, Vec::new(),
        );
        let opts = RenderOpts { width: 80, color: false, group_by_repo: false, blocks: true, icons: false, detail: true };
        let out = render_tree(&[row], &opts);
        assert!(!out.lines().any(|l| l.starts_with("│ links")), "no links field without -s");
    }

    // ── broken symlink list (`-bps`) ──────────────────────────────────

    /// Pre-sorted like `git::broken_symlinks_of` returns it.
    fn broken_list_row() -> Row {
        build_row(
            &entry(Some("main"), "/r"), &status(0, 0, 0), false, Path::new("/r"), None, None,
            vec![
                (".env.local".to_owned(), "/mnt/secrets/app.env".to_owned()),
                ("vendor".to_owned(), "/opt/cuda-12.4".to_owned()),
            ],
        )
    }

    #[test]
    fn block_detail_lists_broken_links_under_count() {
        let opts = RenderOpts { width: 80, color: false, group_by_repo: false, blocks: true, icons: false, detail: true };
        let out = render_tree(&[broken_list_row()], &opts);
        let lines: Vec<&str> = out.lines().collect();
        let links = lines.iter().position(|l| l.starts_with("│ links   2 broken")).expect("count line");
        assert!(
            lines[links + 1].starts_with("│         .env.local -> /mnt/secrets/app.env"),
            "{:?}",
            lines[links + 1]
        );
        assert!(
            lines[links + 2].starts_with("│         vendor -> /opt/cuda-12.4"),
            "{:?}",
            lines[links + 2]
        );
        let value_col = lines[links].find("2 broken").unwrap();
        let entry_col = lines[links + 1].find(".env.local").unwrap();
        assert_eq!(value_col, entry_col, "entries align under the count value");
    }

    #[test]
    fn block_without_detail_keeps_count_only() {
        let opts = RenderOpts { width: 80, color: false, group_by_repo: false, blocks: true, icons: false, detail: false };
        let out = render_tree(&[broken_list_row()], &opts);
        assert!(out.contains("│ links   2 broken"), "count stays: {out:?}");
        assert!(!out.contains(".env.local") && !out.contains("-> "), "no list lines without -p");
    }

    #[test]
    fn broken_link_target_is_dim() {
        let colored = broken_link_lines(&[("v".to_owned(), "/opt/x".to_owned())], true);
        assert_eq!(colored[0], format!("v{DIM} -> /opt/x{RESET}"));
        let plain = broken_link_lines(&[("v".to_owned(), "/opt/x".to_owned())], false);
        assert_eq!(plain[0], "v -> /opt/x");
    }

    #[test]
    fn long_broken_target_widens_card() {
        let long = "/mnt/very/long/path/to/external/library/exceeding-normal-card-width";
        let row = build_row(
            &entry(Some("m"), "/r"), &status(0, 0, 0), false, Path::new("/r"), None, None,
            vec![("vendor".to_owned(), long.to_owned())],
        );
        let opts = RenderOpts { width: 60, color: false, group_by_repo: false, blocks: true, icons: false, detail: true };
        let out = render_tree(&[row], &opts);
        let lines: Vec<&str> = out.lines().collect();
        let widths: std::collections::HashSet<usize> =
            lines.iter().map(|l| l.chars().count()).collect();
        assert_eq!(widths.len(), 1, "all borders aligned: {widths:?}");
        let w = *widths.iter().next().unwrap();
        assert!(w > 59, "card widened beyond default width−1: {w}");
        assert!(lines.iter().any(|l| l.contains(long)), "target fully visible");
    }
}
