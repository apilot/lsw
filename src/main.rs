//! lsw — read-only list of git worktrees: branch, dirty state, ahead/behind.
//!
//! Modes (KISS):
//! - inside a git repo → worktrees of that repo;
//! - inside a plain folder → scan its subdirectories (depth 1) for repos.
//! Flags: `-b` block cards, `-p` per-file dirty breakdown (with `-b`).

mod git;
mod parse;
mod render;

use std::env;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use git::LswError;
use parse::WorktreeStatus;

fn main() {
    let mut blocks = false;
    let mut print_files = false;
    let mut no_icons = false;
    let mut target = ".".to_owned();
    for arg in env::args().skip(1) {
        if arg == "--no-icons" {
            no_icons = true;
        } else if arg == "--version" || arg == "-V" {
            println!("lsw {}", env!("CARGO_PKG_VERSION"));
            return;
        } else if let Some(flags) = arg.strip_prefix('-') {
            for f in flags.chars() {
                match f {
                    'b' => blocks = true,
                    'p' => print_files = true,
                    _ => {
                        eprintln!("lsw: unknown flag: -{f}");
                        std::process::exit(1);
                    }
                }
            }
        } else {
            target = arg;
        }
    }
    if let Err(err) = run(Path::new(&target), blocks, print_files, no_icons) {
        eprintln!("lsw: {err}");
        std::process::exit(1);
    }
}

fn run(target: &Path, blocks: bool, print_files: bool, no_icons: bool) -> Result<(), LswError> {
    let dir = git::canonicalize(target)?;
    let repos = if git::has_git(&dir) {
        vec![dir.clone()]
    } else {
        scan_repos(&dir)?
    };

    let current_dir = env::current_dir().unwrap_or_else(|_| dir.clone());
    let home = home_dir();
    let scan_mode = repos.len() > 1 || (repos.len() == 1 && repos[0] != dir);

    let mut rows = Vec::new();
    let mut errors = Vec::new();
    for repo in &repos {
        let name = repo
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match collect_repo_rows(repo, &current_dir, home.as_deref(), blocks && print_files) {
            Ok(mut repo_rows) => {
                // Name in BOTH modes: scan mode → group header, single mode →
                // block-card label (`./name` instead of bare `./`).
                for row in &mut repo_rows {
                    row.repo = Some(name.clone());
                }
                rows.append(&mut repo_rows);
            }
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    for e in &errors {
        eprintln!("lsw: {e}");
    }
    let color = std::io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none();
    let icons = !no_icons && env::var_os("NO_ICONS").is_none();
    // Width budget = terminal minus the 4-space indent shift, so no line wraps.
    let opts =
        render::RenderOpts { width: terminal_width().saturating_sub(4), color, group_by_repo: scan_mode, blocks, icons };
    let out = render::render_tree(&rows, &opts);
    // Whole-output shift: the listing reads as belonging to the program.
    print!("{}", render::indent(&out, 4));
    Ok(())
}

/// Terminal width: `COLUMNS` → `stty size` → 100. No external crates.
fn terminal_width() -> usize {
    if let Some(w) = env::var("COLUMNS").ok().and_then(|v| v.parse().ok()) {
        return w;
    }
    if let Ok(out) = std::process::Command::new("stty")
        .arg("size")
        .stdin(std::process::Stdio::inherit())
        .output()
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            if let Some(cols) = s.split_whitespace().nth(1) {
                if let Ok(w) = cols.parse() {
                    return w;
                }
            }
        }
    }
    100
}

/// Direct subdirectories of `dir` that are git repositories, sorted by name.
fn scan_repos(dir: &Path) -> Result<Vec<PathBuf>, LswError> {
    let children =
        fs::read_dir(dir).map_err(|source| LswError::Io { path: dir.to_path_buf(), source })?;
    let mut repos: Vec<PathBuf> = children
        .flatten()
        .map(|c| c.path())
        .filter(|p| p.is_dir() && git::has_git(p))
        .collect();
    repos.sort();
    if repos.is_empty() {
        return Err(LswError::NoRepos(dir.to_path_buf()));
    }
    Ok(repos)
}

/// All worktree rows of one repository, filtered to the repository's own
/// subtree: the root worktree itself plus worktrees nested inside it
/// (`wt/...`). Sibling worktrees of the shared repo are the other scan
/// groups' business — the owner wants only "what lives here".
/// `detail` (`-bp`) fills `last` and the per-file dirty breakdown.
fn collect_repo_rows(
    repo: &Path,
    current_dir: &Path,
    home: Option<&Path>,
    detail: bool,
) -> Result<Vec<render::Row>, LswError> {
    let entries = git::list_worktrees(repo)?;
    let mut rows = Vec::with_capacity(entries.len());
    for entry in &entries {
        if entry.path != repo && !entry.path.starts_with(repo) {
            continue;
        }
        let status = if entry.bare {
            WorktreeStatus::default()
        } else {
            git::status_of(&entry.path)?
        };
        let current = entry
            .path
            .canonicalize()
            .map(|p| p == current_dir)
            .unwrap_or(false);
        let mut row = render::build_row(entry, &status, current, repo, home, None);
        if detail && !entry.bare {
            row.last = git::last_commit(&entry.path).map(|(ts, subject)| {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(ts);
                render::Last { age: render::format_age(now.saturating_sub(ts)), subject }
            });
            if status.dirty_files > 0 {
                let numstat: std::collections::HashMap<_, _> = git::numstat_of(&entry.path)
                    .into_iter()
                    .map(|(p, a, d)| (p, (a, d)))
                    .collect();
                row.files = git::status_files_of(&entry.path)?
                    .into_iter()
                    .map(|c| {
                        let letters: String = [c.x, c.y].iter().collect::<String>();
                        let letters = letters.trim_end().to_owned();
                        let (added, deleted) = numstat.get(&c.path).copied().unwrap_or((None, None));
                        render::FileLine { letters, path: c.path, added, deleted }
                    })
                    .collect();
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

/// `$HOME` (unix) or `%USERPROFILE%` (windows).
fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}
