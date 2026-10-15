//! Pure parsers for git porcelain output. No I/O — trivially testable.

use std::path::PathBuf;

/// One record of `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    /// Branch name when on a branch, `None` when detached.
    pub branch: Option<String>,
    pub bare: bool,
}

/// Upstream tracking info from the `## ...` header of `git status -b --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackInfo {
    pub ahead: u32,
    pub behind: u32,
    /// Upstream branch no longer exists (`[gone]`).
    pub gone: bool,
}

/// Result of `git status -b --porcelain` for one worktree.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorktreeStatus {
    /// Changed/untracked files (porcelain lines after the header).
    pub dirty_files: u32,
    /// `None` = no upstream (or detached HEAD).
    pub track: Option<TrackInfo>,
}

/// One file line of `git status --porcelain` (`XY path`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub x: char,
    pub y: char,
    pub path: String,
}

/// Parses `git worktree list --porcelain` output.
pub fn parse_worktree_list(output: &str) -> Vec<WorktreeEntry> {
    let mut entries = Vec::new();
    let mut current: Option<WorktreeEntry> = None;
    for line in output.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(WorktreeEntry {
                path: PathBuf::from(path),
                branch: None,
                bare: false,
            });
        } else if let Some(name) = line.strip_prefix("branch refs/heads/") {
            if let Some(entry) = current.as_mut() {
                entry.branch = Some(name.to_owned());
            }
        } else if line == "bare" {
            if let Some(entry) = current.as_mut() {
                entry.bare = true;
            }
        }
        // "HEAD <sha>", "detached" and blank separators carry no state we need.
    }
    if let Some(entry) = current.take() {
        entries.push(entry);
    }
    entries
}

/// Parses `git status --porcelain -b`: dirty file count + upstream tracking.
pub fn parse_status_output(output: &str) -> WorktreeStatus {
    let mut lines = output.lines();
    let header = lines.next().unwrap_or_default();
    let dirty_files = lines.filter(|l| !l.trim().is_empty()).count() as u32;
    WorktreeStatus {
        dirty_files,
        track: parse_track(header),
    }
}

/// Parses `git status --porcelain` (no header) into per-file changes.
pub fn parse_status_files(output: &str) -> Vec<FileChange> {
    output
        .lines()
        .filter(|l| !l.starts_with("## ") && l.len() >= 4)
        .map(|l| FileChange {
            x: l.as_bytes()[0] as char,
            y: l.as_bytes()[1] as char,
            path: l[3..].to_owned(),
        })
        .collect()
}

/// Parses `git diff --numstat HEAD`: `added<TAB>deleted<TAB>path`;
/// `-` (binary) → `None`. Rename paths `a => b` normalize to `a -> b`
/// to match `git status` rename notation.
pub fn parse_numstat(output: &str) -> Vec<(String, Option<u32>, Option<u32>)> {
    output
        .lines()
        .filter_map(|l| {
            let mut it = l.splitn(3, '\t');
            let added = it.next()?.parse().ok();
            let deleted = it.next()?.parse().ok();
            let path = it.next()?;
            // Rename notation `{a => b}` / `dir/{a => b}/c` → `a -> b` style,
            // matching `git status` (braces in real paths are vanishingly rare).
            let path = path.replace(['{', '}'], "").replace(" => ", " -> ");
            Some((path, added, deleted))
        })
        .collect()
}

/// Extracts `[ahead N, behind M]` (any subset) or `[gone]` from a status header.
fn parse_track(header: &str) -> Option<TrackInfo> {
    // Upstream is present only when the header has `branch...upstream`;
    // `## main` / `## HEAD (no branch)` mean no upstream at all.
    if !header.contains("...") {
        return None;
    }
    let mut info = TrackInfo {
        ahead: 0,
        behind: 0,
        gone: false,
    };
    if let Some(start) = header.find('[') {
        if let Some(end) = header[start..].find(']') {
            let body = &header[start + 1..start + end];
            info.gone = body.contains("gone");
            for part in body.split(',') {
                let part = part.trim();
                if let Some(n) = part.strip_prefix("ahead ") {
                    info.ahead = n.parse().unwrap_or(0);
                }
                if let Some(n) = part.strip_prefix("behind ") {
                    info.behind = n.parse().unwrap_or(0);
                }
            }
        }
    }
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_in_sync_upstream_has_no_bracket() {
        // `## main...origin/main` (in sync, no [..] markers) still means
        // upstream EXISTS — not "no upstream".
        let s = parse_status_output("## main...origin/main\n");
        let t = s.track.expect("upstream must be detected from ...");
        assert_eq!((t.ahead, t.behind, t.gone), (0, 0, false));
    }

    #[test]
    fn track_header_without_upstream_is_none() {
        assert!(parse_status_output("## main\n").track.is_none());
        assert!(parse_status_output("## HEAD (no branch)\n").track.is_none());
    }

    #[test]
    fn worktree_list_two_branches() {
        let out = "\
worktree /repo/main
HEAD abc123
branch refs/heads/main

worktree /repo/.wt/feature
HEAD def456
branch refs/heads/feature/auth
";
        assert_eq!(
            parse_worktree_list(out),
            vec![
                WorktreeEntry { path: PathBuf::from("/repo/main"), branch: Some("main".into()), bare: false },
                WorktreeEntry { path: PathBuf::from("/repo/.wt/feature"), branch: Some("feature/auth".into()), bare: false },
            ]
        );
    }

    #[test]
    fn worktree_list_detached_has_no_branch() {
        let out = "\
worktree /repo/.wt/tmp
HEAD abc123
detached

worktree /repo/main
HEAD 000000
branch refs/heads/main
";
        let parsed = parse_worktree_list(out);
        assert_eq!(parsed[0].branch, None);
        assert_eq!(parsed[1].branch, Some("main".into()));
    }

    #[test]
    fn worktree_list_bare_main() {
        let out = "\
worktree /repo
bare

worktree /repo/.wt/x
HEAD abc123
branch refs/heads/x
";
        let parsed = parse_worktree_list(out);
        assert!(parsed[0].bare);
        assert!(!parsed[1].bare);
    }

    #[test]
    fn worktree_list_empty_output() {
        assert!(parse_worktree_list("").is_empty());
    }

    #[test]
    fn status_clean_no_upstream() {
        assert_eq!(
            parse_status_output("## main\n"),
            WorktreeStatus { dirty_files: 0, track: None }
        );
    }

    #[test]
    fn status_dirty_ahead_behind() {
        let out = "## feat...origin/feat [ahead 1, behind 2]\n M a.txt\n?? b.txt\n";
        assert_eq!(
            parse_status_output(out),
            WorktreeStatus {
                dirty_files: 2,
                track: Some(TrackInfo { ahead: 1, behind: 2, gone: false }),
            }
        );
    }

    #[test]
    fn status_ahead_only() {
        let out = "## feat...origin/feat [ahead 3]\n";
        assert_eq!(
            parse_status_output(out),
            WorktreeStatus {
                dirty_files: 0,
                track: Some(TrackInfo { ahead: 3, behind: 0, gone: false }),
            }
        );
    }

    #[test]
    fn status_upstream_gone() {
        let out = "## feat...origin/feat [gone]\n M a.txt\n";
        assert_eq!(
            parse_status_output(out),
            WorktreeStatus {
                dirty_files: 1,
                track: Some(TrackInfo { ahead: 0, behind: 0, gone: true }),
            }
        );
    }

    #[test]
    fn status_detached_header() {
        assert_eq!(
            parse_status_output("## HEAD (no branch)\n"),
            WorktreeStatus { dirty_files: 0, track: None }
        );
    }

    #[test]
    fn status_empty_output() {
        assert_eq!(parse_status_output(""), WorktreeStatus::default());
    }

    #[test]
    fn status_files_mixed_changes() {
        let out = "## main\n M src/a.rs\n?? b.txt\nR  old.rs -> new.rs\n";
        assert_eq!(
            parse_status_files(out),
            vec![
                FileChange { x: ' ', y: 'M', path: "src/a.rs".into() },
                FileChange { x: '?', y: '?', path: "b.txt".into() },
                FileChange { x: 'R', y: ' ', path: "old.rs -> new.rs".into() },
            ]
        );
    }

    #[test]
    fn status_files_skips_header_and_garbage() {
        assert!(parse_status_files("## main\n").is_empty());
        assert!(parse_status_files("## main\nxy\n").is_empty());
    }

    #[test]
    fn numstat_counts_binary_and_rename() {
        let out = "12\t3\tsrc/a.rs\n-\t-\timg.png\n5\t7\t{old.rs => new.rs}\n";
        assert_eq!(
            parse_numstat(out),
            vec![
                ("src/a.rs".to_owned(), Some(12), Some(3)),
                ("img.png".to_owned(), None, None),
                ("old.rs -> new.rs".to_owned(), Some(5), Some(7)),
            ]
        );
    }

    #[test]
    fn numstat_empty_output() {
        assert!(parse_numstat("").is_empty());
    }
}
