//! lsw — read-only list of git worktrees: branch, dirty state, ahead/behind.
//!
//! Modes (KISS):
//! - inside a git repo → worktrees of that repo;
//! - inside a plain folder → scan subdirectories for repos (depth 1 by
//!   default, `-2`/`-3` deeper; nested repos labelled `dir/repo`; worktrees
//!   of an already-listed repo are not duplicated).
//! Flags: `-b` block cards, `-p` per-file dirty breakdown (with `-b`),
//! `--debug` shows per-repo errors (stale/broken repos are skipped silently).

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
    let mut debug = false;
    let mut depth = 1usize;
    let mut target = ".".to_owned();
    for arg in env::args().skip(1) {
        if arg == "--no-icons" {
            no_icons = true;
        } else if arg == "--debug" {
            debug = true;
        } else if arg == "--version" || arg == "-V" {
            println!("lsw {}", env!("CARGO_PKG_VERSION"));
            return;
        } else if let Some(flags) = arg.strip_prefix('-') {
            for f in flags.chars() {
                match f {
                    'b' => blocks = true,
                    'p' => print_files = true,
                    '1' | '2' | '3' => depth = f.to_digit(10).unwrap() as usize,
                    '4'..='9' => {
                        eprintln!("lsw: scan depth -{f} not supported (max -3)");
                        std::process::exit(1);
                    }
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
    if let Err(err) = run(Path::new(&target), blocks, print_files, no_icons, depth, debug) {
        eprintln!("lsw: {err}");
        std::process::exit(1);
    }
}

fn run(
    target: &Path,
    blocks: bool,
    print_files: bool,
    no_icons: bool,
    depth: usize,
    debug: bool,
) -> Result<(), LswError> {
    let dir = git::canonicalize(target)?;
    let (repos, skipped) = if git::has_git(&dir) {
        let label = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        (vec![Hit { path: dir.clone(), label }], Vec::new())
    } else {
        scan_repos(&dir, depth)?
    };

    let current_dir = env::current_dir().unwrap_or_else(|_| dir.clone());
    let home = home_dir();
    let scan_mode = repos.len() > 1 || (repos.len() == 1 && repos[0].path != dir);

    let mut rows = Vec::new();
    let mut errors = Vec::new();
    for Hit { path: repo, label: name } in &repos {
        let remote = git::remote_of(repo).map(|url| git::short_remote(&url));
        match collect_repo_rows(repo, &current_dir, home.as_deref(), blocks && print_files) {
            Ok(mut repo_rows) => {
                // Name + origin in BOTH modes: scan mode → group header,
                // single mode → card label (`./name` instead of bare `./`).
                for row in &mut repo_rows {
                    row.repo = Some(name.clone());
                    row.remote = remote.clone();
                }
                rows.append(&mut repo_rows);
            }
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    if debug {
        for e in &errors {
            eprintln!("lsw: {e}");
        }
        for note in &skipped {
            eprintln!("lsw: {note}");
        }
    }
    if repos.is_empty() {
        // Friendly empty state: what happened, where, and what to try next.
        let levels = if depth == 1 { "1 level".to_owned() } else { format!("{depth} levels") };
        eprintln!("no git repositories found in {} (scanned {levels})", dir.display());
        if !skipped.is_empty() {
            let n = skipped.len();
            eprintln!(
                "{n} {} could not be read — re-run with --debug for details",
                if n == 1 { "directory" } else { "directories" }
            );
        }
        return Ok(());
    }    let color = std::io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none();
    if rows.is_empty() {
        // Repositories were found, but none of them could be read — say so.
        let n = repos.len();
        eprintln!(
            "found {n} repositories but no worktrees could be read — re-run with --debug for details"
        );
        return Ok(());
    }
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

#[derive(Debug)]
/// One discovered repository: where it lives and how to label it (the path
/// relative to the scan root — `api` at depth 1, `team/webapp` deeper).
pub struct Hit {
    pub path: PathBuf,
    pub label: String,
}

/// Directories (up to `depth` levels below `root`) that contain a `.git`
/// entry, breadth-first with sorted names. Descends into plain dirs and
/// repos alike (nested repos are listed with their nesting path), but skips
/// hidden entries and heavy junk (`node_modules`, `target`).
///
/// Dedup ("only the top"): a linked worktree whose real repository root is
/// inside the scan root and already listed is skipped — its content is
/// already shown by that repo's `git worktree list`. Worktrees whose real
/// root is OUTSIDE the scan root stay as their own groups (sibling-worktree
/// layouts like `crm/{api,wtx1,wtx2}` must keep working).
///
/// Returns the hits plus human-readable notes about directories that could
/// not be read (permissions): unreadable dirs never abort the scan. An empty
/// result is `Ok` too — the caller turns it into a friendly notice.
fn scan_repos(root: &Path, depth: usize) -> Result<(Vec<Hit>, Vec<String>), LswError> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut frontier = vec![root.to_path_buf()];
    for level in 1..=depth {
        let mut descend = Vec::new();
        for dir in &frontier {
            let children = match fs::read_dir(dir) {
                Ok(c) => c,
                Err(e) if dir == root => {
                    return Err(LswError::Io { path: dir.clone(), source: e });
                }
                Err(e) => {
                    skipped.push(format!("{}: {e}", dir.display()));
                    continue;
                }
            };
            let mut paths: Vec<PathBuf> =
                children.flatten().map(|c| c.path()).filter(|p| p.is_dir()).collect();
            paths.sort();
            for p in paths {
                if git::has_git(&p) && !stale_worktree(&p) {
                    candidates.push(p.clone());
                }
                if level < depth && descend_ok(&p) {
                    descend.push(p);
                }
            }
        }
        frontier = descend;
    }

    let mut hits: Vec<Hit> = Vec::new();
    let mut emitted: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut listed_roots: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for path in candidates {
        let key = path.canonicalize().unwrap_or_else(|_| path.clone());
        if !emitted.insert(key.clone()) {
            continue;
        }
        if let Some(real) = real_root(&path) {
            if real.starts_with(root) {
                if real == key {
                    // The repository root itself, now listed: deeper worktree
                    // dirs resolving to it become duplicates.
                    listed_roots.insert(real);
                } else if listed_roots.contains(&real) {
                    continue; // worktree of an already-listed repo
                }
            }
        }
        let label =
            path.strip_prefix(root).unwrap_or(&path).to_string_lossy().into_owned();
        hits.push(Hit { path, label });
    }
    Ok((hits, skipped))
}

/// May the scan descend into this directory (deeper levels only)?
fn descend_ok(p: &Path) -> bool {
    match p.file_name().and_then(|n| n.to_str()) {
        Some(name) => {
            !name.starts_with('.') && name != "node_modules" && name != "target"
        }
        None => false,
    }
}

/// Real repository root behind a `.git` entry: the dir itself for a `.git`
/// directory, the main repo for a linked worktree's `.git` FILE
/// (`gitdir: <repo>/.git/worktrees/<name>`). Canonicalized; `None` when
/// unknowable — the dir then acts as a repository of its own.
fn real_root(dir: &Path) -> Option<PathBuf> {
    let dot = dir.join(".git");
    if fs::metadata(&dot).ok()?.is_dir() {
        return dir.canonicalize().ok();
    }
    let content = fs::read_to_string(&dot).ok()?;
    let target = content.strip_prefix("gitdir:")?.trim();
    let gitdir =
        if Path::new(target).is_absolute() { PathBuf::from(target) } else { dir.join(target) };
    let s = gitdir.to_string_lossy();
    let idx = s.rfind("/.git/worktrees/")?;
    let root = PathBuf::from(&s[..idx]);
    root.canonicalize().ok()
}

/// Linked worktree whose gitdir target is gone (the repo was moved or
/// deleted): every git command would fail here, so the scan silently skips
/// the directory instead of erroring per worktree.
fn stale_worktree(dir: &Path) -> bool {
    let dot = dir.join(".git");
    let Ok(meta) = fs::metadata(&dot) else { return false };
    if meta.is_dir() {
        return false;
    }
    let Ok(content) = fs::read_to_string(&dot) else { return false };
    let Some(target) = content.strip_prefix("gitdir:").map(str::trim) else { return false };
    let gitdir =
        if Path::new(target).is_absolute() { PathBuf::from(target) } else { dir.join(target) };
    !gitdir.exists()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = env::temp_dir().join(format!("lsw-scan-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn mkrepo(p: &Path) {
        fs::create_dir_all(p.join(".git")).unwrap();
    }

    fn mkworktree(p: &Path, real_root: &Path) {
        // Realistic worktree: the gitdir target must exist, else the scan
        // would classify it as stale.
        fs::create_dir_all(real_root.join(".git/worktrees/x")).unwrap();
        fs::create_dir_all(p).unwrap();
        fs::write(
            p.join(".git"),
            format!("gitdir: {}\n", real_root.join(".git/worktrees/x").display()),
        )
        .unwrap();
    }

    fn labels(root: &Path, depth: usize) -> Vec<String> {
        scan_repos(root, depth).unwrap().0.into_iter().map(|h| h.label).collect()
    }

    #[test]
    fn scan_empty_result_is_ok_not_error() {
        let root = tmpdir("empty");
        fs::create_dir_all(root.join("plain/nested")).unwrap();
        let (hits, skipped) = scan_repos(&root, 3).unwrap();
        assert!(hits.is_empty() && skipped.is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scan_unreadable_dir_is_skipped_not_fatal() {
        let root = tmpdir("perm");
        mkrepo(&root.join("real"));
        let locked = root.join("locked");
        fs::create_dir_all(&locked).unwrap();
        fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();
        // Running as root would defeat the permission model — then read_dir
        // succeeds and there is nothing to assert about skipping.
        if fs::read_dir(&locked).is_err() {
            let (hits, skipped) = scan_repos(&root, 3).unwrap();
            assert_eq!(hits.len(), 1, "repo still found");
            assert_eq!(skipped.len(), 1, "locked dir reported as skipped");
        }
        let _ = fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o755));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scan_depth1_finds_only_top_level_repos() {
        let root = tmpdir("d1");
        mkrepo(&root.join("api"));
        fs::create_dir_all(root.join("team")).unwrap();
        mkrepo(&root.join("team/webapp"));
        assert_eq!(labels(&root, 1), vec!["api"]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scan_depth2_labels_nested_path() {
        let root = tmpdir("d2");
        mkrepo(&root.join("team/webapp"));
        assert_eq!(labels(&root, 2), vec!["team/webapp"]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scan_dedups_worktree_of_listed_repo() {
        // api is listed; api/nested-wt is a linked worktree of api → skipped.
        let root = tmpdir("dup");
        mkrepo(&root.join("api"));
        mkworktree(&root.join("api/nested-wt"), &root.join("api"));
        assert_eq!(labels(&root, 2), vec!["api"]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scan_keeps_sibling_worktrees_of_outside_repo() {
        // crm layout: worktree siblings whose real root is outside the scan.
        let outside = tmpdir("outside");
        mkrepo(&outside.join("real"));
        let root = tmpdir("sib");
        mkworktree(&root.join("wta"), &outside.join("real"));
        mkworktree(&root.join("wtb"), &outside.join("real"));
        assert_eq!(labels(&root, 1), vec!["wta", "wtb"]);
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&outside).unwrap();
    }

    #[test]
    fn scan_finds_nested_repo_inside_repo_and_skips_junk() {
        let root = tmpdir("nest");
        mkrepo(&root.join("outer"));
        mkrepo(&root.join("outer/inner"));
        mkrepo(&root.join("outer/node_modules/junk"));
        mkrepo(&root.join("outer/.cache/hidden-junk"));
        assert_eq!(labels(&root, 3), vec!["outer", "outer/inner"]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scan_skips_stale_worktrees() {
        // .git file pointing to a vanished gitdir (repo moved/deleted):
        // silently skipped — no group, no error spam.
        let root = tmpdir("stale");
        mkrepo(&root.join("gone"));
        mkworktree(&root.join("wt"), &root.join("gone"));
        fs::remove_dir_all(root.join("gone")).unwrap();
        // Only a stale worktree left → empty result, not an error.
        let (hits, _skipped) = scan_repos(&root, 2).unwrap();
        assert!(hits.is_empty(), "stale worktree must be skipped");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn real_root_resolves_worktree_gitdir_file() {
        let root = tmpdir("rr");
        mkrepo(&root.join("real"));
        let wt = root.join("wt");
        mkworktree(&wt, &root.join("real"));
        assert_eq!(real_root(&wt), Some(root.join("real").canonicalize().unwrap()));
        assert_eq!(real_root(&root.join("real")), Some(root.join("real").canonicalize().unwrap()));
        fs::write(wt.join(".git"), "gitdir: ../real/.git/worktrees/x\n").unwrap();
        assert_eq!(real_root(&wt), Some(root.join("real").canonicalize().unwrap()));
        fs::write(wt.join(".git"), "gitdir: /somewhere/.git/modules/sub\n").unwrap();
        assert_eq!(real_root(&wt), None);
        fs::remove_dir_all(&root).unwrap();
    }
}
