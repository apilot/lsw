//! The only module that talks to the `git` binary (I/O boundary).

use std::fs;
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
    Git(String),
}

impl std::fmt::Display for LswError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LswError::Io { path, source } => write!(f, "{}: {}", path.display(), source),
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

/// `origin` URL of the repository, best-effort (`None` when unset).
pub fn remote_of(repo: &Path) -> Option<String> {
    run(repo, &["remote", "get-url", "origin"])
        .ok()
        .map(|url| url.trim().to_owned())
        .filter(|url| !url.is_empty())
}

/// Broken symlinks among the TOP-LEVEL entries of a worktree root (`-s`),
/// as `(name, target)` pairs sorted by name (read_dir order is arbitrary;
/// deterministic output for tests and listing alike). A broken symlink is
/// an entry whose target cannot be resolved. One `read_dir`, no recursion;
/// hidden entries included, `.git` skipped by name (never a useful signal,
/// often the heaviest stat). The entry type comes straight from the dirent
/// (`d_type`) — `fs::metadata` runs only for actual symlinks. Best-effort —
/// an unreadable directory yields an empty list, never fails the listing.
pub fn broken_symlinks_of(worktree: &Path) -> Vec<(String, String)> {
    let Ok(entries) = fs::read_dir(worktree) else { return Vec::new() };
    let mut broken = Vec::new();
    for entry in entries.flatten() {
        if entry.file_name() == ".git" {
            continue;
        }
        // DirEntry::file_type does not follow symlinks (d_type; falls back
        // to one symlink_metadata call where the fs lacks d_type).
        let is_symlink = entry.file_type().map(|t| t.is_symlink()).unwrap_or(false);
        if is_symlink && fs::metadata(entry.path()).is_err() {
            // TOCTOU: the link may vanish between metadata and read_link —
            // skip rather than render a dangling arrow (`name -> `).
            let Ok(target) = fs::read_link(entry.path()) else { continue };
            broken.push((
                entry.file_name().to_string_lossy().into_owned(),
                target.to_string_lossy().into_owned(),
            ));
        }
    }
    broken.sort_by(|a, b| a.0.cmp(&b.0));
    broken
}

/// Compact display form of a remote URL: `git@github.com:apilot/lsw.git`,
/// `https://github.com/apilot/lsw.git` and `ssh://git@github.com/a/b` all
/// become `github.com/apilot/lsw`. Anything else passes through trimmed
/// (local paths stay readable as-is).
pub fn short_remote(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("ssh://"))
    {
        return rest.strip_prefix("git@").unwrap_or(rest).to_owned();
    }
    if let Some(rest) = url.strip_prefix("git@") {
        return rest.replacen(':', "/", 1);
    }
    url.to_owned()
}

#[cfg(test)]
mod tests {
    use super::{broken_symlinks_of, short_remote};
    use std::fs;
    use std::path::{Path, PathBuf};

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lsw-git-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn short_remote_normalizes_common_url_forms() {
        assert_eq!(short_remote("git@github.com:apilot/lsw.git"), "github.com/apilot/lsw");
        assert_eq!(short_remote("https://github.com/apilot/lsw.git"), "github.com/apilot/lsw");
        assert_eq!(short_remote("http://gitlab.corp/team/proj/"), "gitlab.corp/team/proj");
        assert_eq!(short_remote("ssh://git@host.tld/x/y.git"), "host.tld/x/y");
        assert_eq!(short_remote("/srv/git/lsw"), "/srv/git/lsw");
    }

    #[cfg(unix)]
    #[test]
    fn broken_symlink_counted() {
        let root = tmpdir("broken");
        std::os::unix::fs::symlink("gone-target", root.join("bad")).unwrap();
        assert_eq!(broken_symlinks_of(&root), vec![("bad".to_owned(), "gone-target".to_owned())]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn valid_symlink_not_counted() {
        let root = tmpdir("valid");
        fs::write(root.join("real.txt"), "x").unwrap();
        std::os::unix::fs::symlink("real.txt", root.join("good")).unwrap();
        assert!(broken_symlinks_of(&root).is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn regular_file_not_counted() {
        let root = tmpdir("regular");
        fs::write(root.join("file.txt"), "x").unwrap();
        assert!(broken_symlinks_of(&root).is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn hidden_broken_symlink_counted() {
        let root = tmpdir("hidden");
        std::os::unix::fs::symlink("gone", root.join(".env")).unwrap();
        assert_eq!(broken_symlinks_of(&root), vec![(".env".to_owned(), "gone".to_owned())]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn git_name_skipped() {
        // `.git` is skipped by name — even a broken symlink named `.git`.
        let root = tmpdir("gitname");
        std::os::unix::fs::symlink("nowhere", root.join(".git")).unwrap();
        assert!(broken_symlinks_of(&root).is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn two_broken_symlinks_counted_and_sorted() {
        // Created b-before-a on purpose: output must be sorted by name
        // (read_dir order is arbitrary).
        let root = tmpdir("two-broken");
        std::os::unix::fs::symlink("gone-b", root.join("b")).unwrap();
        std::os::unix::fs::symlink("gone-a", root.join("a")).unwrap();
        assert_eq!(
            broken_symlinks_of(&root),
            vec![("a".to_owned(), "gone-a".to_owned()), ("b".to_owned(), "gone-b".to_owned())]
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn missing_dir_counts_zero() {
        // Best-effort: unreadable/missing root never fails the listing.
        assert!(broken_symlinks_of(Path::new("/nonexistent/lsw-test")).is_empty());
    }
}
