//! `jev-cc reword`: prefixes commits the hook couldn't, for example because the commit
//! was made offline.
//!
//! Commits are rebuilt with `git commit-tree` on their original trees, so there's nothing
//! to conflict and the index and working tree are untouched. Only the branch moves.

use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};

use crate::classify::Classification;
use crate::{message, parallel};

pub struct Options {
    /// Commits after this one, up to HEAD, are considered. Defaults to the upstream.
    pub base: Option<String>,
    pub dry_run: bool,
}

impl Options {
    pub fn parse(args: &[&str]) -> Result<Options, String> {
        let mut options = Options {
            base: None,
            dry_run: false,
        };
        for &arg in args {
            match arg {
                "--dry-run" | "-n" => options.dry_run = true,
                _ if arg.starts_with('-') => return Err(format!("unknown option {arg}")),
                _ if options.base.is_none() => options.base = Some(arg.to_string()),
                _ => return Err(format!("unexpected argument {arg}")),
            }
        }
        Ok(options)
    }
}

struct Commit {
    sha: String,
    parents: Vec<String>,
    author_name: String,
    author_email: String,
    /// `--date=raw`, which `GIT_AUTHOR_DATE` accepts as is.
    author_date: String,
    message: String,
}

impl Commit {
    fn subject(&self) -> &str {
        self.message.lines().next().unwrap_or_default()
    }

    fn short(&self) -> &str {
        &self.sha[..7]
    }
}

/// `classify` answers for one commit: its sha and its message. It should reject answers
/// below the confidence threshold, as the hook does.
pub fn run<F>(options: &Options, classify: F) -> Result<(), String>
where
    F: Fn(&str, &str) -> Result<Classification, String> + Sync,
{
    let base = match &options.base {
        Some(base) => resolve(base).ok_or(format!("unknown revision {base}"))?,
        None => resolve("@{upstream}").ok_or(
            "this branch has no upstream; pass the commit to start after, \
             e.g. `jev-cc reword main`",
        )?,
    };
    let head = git(&["rev-parse", "HEAD"])?.trim().to_string();
    let commits = commits_between(&base, &head)?;
    let candidates: Vec<&Commit> = commits
        .iter()
        .filter(|c| !message::should_skip(c.subject()))
        .collect();
    if candidates.is_empty() {
        println!("nothing to reword");
        return Ok(());
    }
    for commit in &candidates {
        if let Some(remote) = published_on(&commit.sha)? {
            return Err(format!(
                "{} is already on {remote}; rewording it would rewrite published history",
                commit.short()
            ));
        }
    }

    let results = parallel::map(&candidates, |commit| {
        classify(&commit.sha, commit.message.trim())
    });
    let mut prefixes: HashMap<&str, String> = HashMap::new();
    for (commit, result) in candidates.iter().zip(&results) {
        match result {
            Ok(c) => {
                println!("{} {}{}", commit.short(), c.prefix(), commit.subject());
                prefixes.insert(commit.sha.as_str(), c.prefix());
            }
            Err(e) => println!("{} left as is ({e})", commit.short()),
        }
    }
    if prefixes.is_empty() {
        return Err("no commits reworded".into());
    }
    if options.dry_run {
        return Ok(());
    }

    let new_head = rewrite(&commits, &prefixes)?;
    move_head(&head, &new_head)?;
    println!(
        "reworded {} of {} commits; undo with `git reset --soft {}`",
        prefixes.len(),
        candidates.len(),
        &head[..7]
    );
    Ok(())
}

fn resolve(rev: &str) -> Option<String> {
    git(&[
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("{rev}^{{commit}}"),
    ])
    .ok()
    .map(|s| s.trim().to_string())
}

/// Oldest first, parents before children.
fn commits_between(base: &str, head: &str) -> Result<Vec<Commit>, String> {
    let range = format!("{base}..{head}");
    let log = git(&[
        "log",
        "--reverse",
        "--topo-order",
        "--date=raw",
        "--format=%H%x00%P%x00%an%x00%ae%x00%ad%x00%B%x1e",
        &range,
        "--",
    ])?;
    Ok(log
        .split('\x1e')
        .filter_map(|record| {
            let mut fields = record.trim_start_matches('\n').splitn(6, '\0');
            let sha = fields.next().filter(|s| !s.is_empty())?;
            Some(Commit {
                sha: sha.to_string(),
                parents: fields
                    .next()?
                    .split_whitespace()
                    .map(str::to_string)
                    .collect(),
                author_name: fields.next()?.to_string(),
                author_email: fields.next()?.to_string(),
                author_date: fields.next()?.to_string(),
                message: fields.next()?.to_string(),
            })
        })
        .collect())
}

/// A remote-tracking branch that already contains `sha`, if any.
fn published_on(sha: &str) -> Result<Option<String>, String> {
    let refs = git(&[
        "for-each-ref",
        "--contains",
        sha,
        "--format=%(refname:short)",
        "refs/remotes",
    ])?;
    Ok(refs.lines().next().map(str::to_string))
}

/// Recreates every commit from the first reworded one onwards, and returns the new tip.
fn rewrite(commits: &[Commit], prefixes: &HashMap<&str, String>) -> Result<String, String> {
    let mut rewritten: HashMap<&str, String> = HashMap::new();
    let mut tip = String::new();
    for commit in commits {
        let parents: Vec<&str> = commit
            .parents
            .iter()
            .map(|p| rewritten.get(p.as_str()).map_or(p.as_str(), String::as_str))
            .collect();
        let prefix = prefixes.get(commit.sha.as_str());
        let unchanged = prefix.is_none() && parents.iter().eq(commit.parents.iter());
        tip = if unchanged {
            commit.sha.clone()
        } else {
            let message = match prefix {
                Some(prefix) => message::apply_prefix(&commit.message, prefix),
                None => commit.message.clone(),
            };
            commit_tree(commit, &parents, &message)?
        };
        rewritten.insert(&commit.sha, tip.clone());
    }
    Ok(tip)
}

/// A copy of `commit` with new parents and message. Author and date are kept; the
/// committer is you, now, as with a rebase.
fn commit_tree(commit: &Commit, parents: &[&str], message: &str) -> Result<String, String> {
    let tree = format!("{}^{{tree}}", commit.sha);
    let mut command = Command::new("git");
    command.args(["commit-tree", &tree, "-F", "-"]);
    for parent in parents {
        command.args(["-p", parent]);
    }
    let mut child = command
        .env("GIT_AUTHOR_NAME", &commit.author_name)
        .env("GIT_AUTHOR_EMAIL", &commit.author_email)
        .env("GIT_AUTHOR_DATE", &commit.author_date)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to run git: {e}"))?;
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(message.as_bytes())
        .map_err(|e| e.to_string())?;
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Moves the current branch (or a detached HEAD) to `new`, only if it's still at `old`.
fn move_head(old: &str, new: &str) -> Result<(), String> {
    let reason = "jev-cc reword";
    match git(&["symbolic-ref", "-q", "HEAD"]) {
        Ok(branch) => git(&["update-ref", "-m", reason, branch.trim(), new, old]),
        Err(_) => git(&["update-ref", "--no-deref", "-m", reason, "HEAD", new, old]),
    }
    .map(|_| ())
}

fn git(args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_options() {
        let o = Options::parse(&["-n", "main"]).unwrap();
        assert!(o.dry_run);
        assert_eq!(o.base.as_deref(), Some("main"));
        assert!(Options::parse(&["a", "b"]).is_err());
        assert!(Options::parse(&["--force"]).is_err());
    }
}
