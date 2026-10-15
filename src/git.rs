//! The only module that talks to the `git` binary (I/O boundary).

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::parse::{
    parse_numstat, parse_status_files, parse_status_output, parse_worktree_list, FileChange,
    WorktreeEntry, WorktreeStatus,
};

#[derive(Debug)]
pub enum LswError {
    Io { path: PathBuf, source: io::Error },
    NoRepos(PathBuf),
    Git(String),
}

impl std::fmt::Display for LswError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LswError::Io { path, source } => write!(f, "{}: {}", path.display(), source),
            LswError::NoRepos(p) => {
                write!(f, "no git repositories found in {}", p.display())
            }
            LswError::Git(msg) => write!(f, "git {msg}"),
        }
    }
}

/// Runs `git -C <dir> <args...>`, returns stdout; non-zero exit → `LswError::Git`.
fn run(dir: &Path, args: &[&str]) -> Result<String, LswError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|source| LswError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(LswError::Git(format!("{}: {}", args.join(" "), stderr.trim())));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Returns the canonical (absolute, symlinks resolved) form of `dir`.
pub fn canonicalize(dir: &Path) -> Result<PathBuf, LswError> {
    dir.canonicalize()
        .map_err(|source| LswError::Io { path: dir.to_path_buf(), source })
}

/// Cheap repository check: `.git` entry exists (dir in repos, file in worktrees).
pub fn has_git(dir: &Path) -> bool {
    dir.join(".git").exists()
}

/// Lists all worktrees of the repository containing `dir`.
pub fn list_worktrees(dir: &Path) -> Result<Vec<WorktreeEntry>, LswError> {
    let out = run(dir, &["worktree", "list", "--porcelain"])?;
    Ok(parse_worktree_list(&out))
}

/// Dirty count + ahead/behind for one worktree in a single git call.
pub fn status_of(worktree: &Path) -> Result<WorktreeStatus, LswError> {
    let out = run(worktree, &["status", "--porcelain", "-b"])?;
    Ok(parse_status_output(&out))
}

/// Last commit of a worktree as `(unix_ts, subject)`; best-effort —
/// `None` on any failure (empty repo, detached plumbing, …).
pub fn last_commit(worktree: &Path) -> Option<(u64, String)> {
    let out = run(worktree, &["log", "-1", "--format=%ct%x09%s"]).ok()?;
    let line = out.lines().next()?;
    let (ts, subject) = line.split_once('\t')?;
    Some((ts.trim().parse().ok()?, subject.to_owned()))
}

/// Per-file changes of a worktree (`git status --porcelain`).
pub fn status_files_of(worktree: &Path) -> Result<Vec<FileChange>, LswError> {
    let out = run(worktree, &["status", "--porcelain"])?;
    Ok(parse_status_files(&out))
}

/// Added/deleted line counts per file vs HEAD; best-effort — empty on
/// failure (e.g. repo without commits). Binary files carry `(None, None)`.
pub fn numstat_of(worktree: &Path) -> Vec<(String, Option<u32>, Option<u32>)> {
    run(worktree, &["diff", "--numstat", "HEAD"])
        .map(|out| parse_numstat(&out))
        .unwrap_or_default()
}
