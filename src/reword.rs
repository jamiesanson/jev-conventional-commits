//! `jev-cc reword`: prefixes commits the hook couldn't, for example because the commit
//! was made offline.
//!
//! Every commit is classified up front, in parallel. Git then does the rewriting: an
//! interactive rebase whose todo list jev-cc writes, with an `exec` after each commit to
//! amend in its prefix. Amending through `git commit` keeps signing, author and date as
//! the user's git config has them.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::classify::Classification;
use crate::{message, parallel};

/// Where the sequence editor finds the planned prefixes.
const PLAN_VAR: &str = "JEV_CC_REWORD_PLAN";

/// Where the default branch might be, most preferred first.
const REMOTE_DEFAULTS: &[&str] = &["origin/HEAD", "origin/main", "origin/master"];
const LOCAL_DEFAULTS: &[&str] = &["main", "master"];

pub struct Options {
    /// Commits after this one, up to HEAD, are considered. Defaults to where the branch
    /// left the default branch.
    pub since: Option<String>,
    pub dry_run: bool,
}

impl Options {
    pub fn parse(args: &[&str]) -> Result<Options, String> {
        let mut options = Options {
            since: None,
            dry_run: false,
        };
        let mut args = args.iter();
        while let Some(&arg) = args.next() {
            match arg {
                "--dry-run" | "-n" => options.dry_run = true,
                "--since" => {
                    let rev = args.next().ok_or("--since needs a commit")?;
                    options.since = Some(rev.to_string());
                }
                _ if arg.starts_with('-') => return Err(format!("unknown option {arg}")),
                _ => return Err(format!("unexpected argument {arg}")),
            }
        }
        Ok(options)
    }
}

struct Commit {
    sha: String,
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
    let (base, described) = match &options.since {
        Some(rev) => (
            resolve(rev).ok_or(format!("unknown revision {rev}"))?,
            rev.clone(),
        ),
        None => branch_start()?,
    };
    let head = git(&["rev-parse", "HEAD"])?.trim().to_string();
    let candidates: Vec<Commit> = commits_between(&base, &head)?
        .into_iter()
        .filter(|c| !message::should_skip(c.subject()))
        .collect();
    if candidates.is_empty() {
        println!("nothing to reword since {described}");
        return Ok(());
    }
    let count = match candidates.len() {
        1 => "1 unprefixed commit".to_string(),
        n => format!("{n} unprefixed commits"),
    };
    println!("{count} since {described} ({}):", &base[..7]);

    let results = parallel::map(&candidates, |commit| {
        classify(&commit.sha, commit.message.trim())
    });
    let mut plan = Vec::new();
    for (commit, result) in candidates.iter().zip(&results) {
        match result {
            Ok(c) => {
                println!("{} {}{}", commit.short(), c.prefix(), commit.subject());
                plan.push((commit, c.prefix()));
            }
            Err(e) => println!("{} left as is ({e})", commit.short()),
        }
    }
    if plan.is_empty() {
        return Err("no commits reworded".into());
    }
    if options.dry_run {
        return Ok(());
    }

    let mut published = None;
    for (commit, _) in &plan {
        published = published_on(&commit.sha)?;
        if published.is_some() {
            break;
        }
    }

    let plan_path = git(&["rev-parse", "--git-path", "jev-cc-reword-plan"])?;
    let plan_path = plan_path.trim();
    let contents: String = plan
        .iter()
        .map(|(commit, prefix)| format!("{} {prefix}\n", commit.sha))
        .collect();
    std::fs::write(plan_path, contents).map_err(|e| e.to_string())?;
    let rebased = rebase(&base, plan_path);
    let _ = std::fs::remove_file(plan_path);
    rebased?;

    println!(
        "reworded {} of {} commits; undo with `git reset --soft {}`",
        plan.len(),
        candidates.len(),
        &head[..7]
    );
    if let Some(remote) = published {
        println!("some were already on {remote}; push with `git push --force-with-lease`");
    }
    Ok(())
}

/// Runs the rebase with jev-cc as its sequence editor. The hook is disabled throughout,
/// since the prefixes are already decided.
fn rebase(base: &str, plan_path: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let editor = format!("{} __reword-todo", shell_quote(&exe.to_string_lossy()));
    let output = Command::new("git")
        .args([
            "rebase",
            "--interactive",
            "--autostash",
            "--rebase-merges",
            base,
        ])
        .env("GIT_SEQUENCE_EDITOR", editor)
        .env(PLAN_VAR, plan_path)
        .env("JEV_CC_DISABLE", "1")
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    // Progress is noise, but anything else (a conflict, an autostash that didn't apply
    // cleanly) needs to be seen. Interactive rebases ignore --quiet.
    let stderr = String::from_utf8_lossy(&output.stderr);
    for line in stderr.split(['\r', '\n']) {
        let noise = [
            "Rebasing (",
            "Executing: ",
            "Successfully rebased",
            "Created autostash",
            "Applied autostash",
        ];
        if !line.trim().is_empty() && !noise.iter().any(|n| line.starts_with(n)) {
            eprintln!("{line}");
        }
    }
    if !output.status.success() {
        return Err("rebase stopped; finish it or run `git rebase --abort`".into());
    }
    Ok(())
}

/// The sequence editor: adds an `exec` amending in the planned prefix after each
/// planned commit's `pick`.
pub fn edit_todo(todo: &Path) -> Result<(), String> {
    let plan_path = std::env::var(PLAN_VAR).map_err(|_| format!("{PLAN_VAR} isn't set"))?;
    let plan = std::fs::read_to_string(plan_path).map_err(|e| e.to_string())?;
    let plan: Vec<(&str, &str)> = plan
        .lines()
        .filter_map(|line| line.split_once(' '))
        .collect();
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = shell_quote(&exe.to_string_lossy());

    let original = std::fs::read_to_string(todo).map_err(|e| e.to_string())?;
    std::fs::write(todo, add_amends(&original, &plan, &exe)).map_err(|e| e.to_string())
}

fn add_amends(todo: &str, plan: &[(&str, &str)], exe: &str) -> String {
    let mut out = String::new();
    for line in todo.lines() {
        out.push_str(line);
        out.push('\n');
        let mut words = line.split_whitespace();
        let (Some("pick" | "p"), Some(abbrev)) = (words.next(), words.next()) else {
            continue;
        };
        if let Some((_, prefix)) = plan.iter().find(|(sha, _)| sha.starts_with(abbrev)) {
            out.push_str(&format!(
                "exec {exe} __reword-amend {}\n",
                shell_quote(prefix)
            ));
        }
    }
    out
}

/// Run by the rebase after each planned commit: puts `prefix` on HEAD's message.
pub fn amend(prefix: &str) -> Result<(), String> {
    let current = git(&["log", "-1", "--format=%B"])?;
    if message::should_skip(current.lines().next().unwrap_or_default()) {
        return Ok(());
    }
    let reworded = message::apply_prefix(current.trim_end(), prefix);
    let mut child = Command::new("git")
        .args([
            "commit",
            "--amend",
            "--quiet",
            "--no-verify",
            "--allow-empty",
            "--cleanup=verbatim",
            "--file=-",
        ])
        .env("JEV_CC_DISABLE", "1")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to run git: {e}"))?;
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(format!("{}\n", reworded.trim_end()).as_bytes())
        .map_err(|e| e.to_string())?;
    if !child.wait().map_err(|e| e.to_string())?.success() {
        return Err("git commit --amend failed".into());
    }
    Ok(())
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Where the current branch left the default branch, and that branch's name. Both the
/// local and remote default branch count, whichever the branch left most recently, so
/// unpushed commits on local `main` aren't mistaken for the branch's own. On the default
/// branch itself only the remote counts, so this is the last push.
fn branch_start() -> Result<(String, String), String> {
    let remote = REMOTE_DEFAULTS
        .iter()
        .find(|b| resolve(b).is_some())
        .map(|b| git(&["rev-parse", "--abbrev-ref", b]).map(|name| name.trim().to_string()))
        .transpose()?;
    let local_name = remote
        .as_deref()
        .and_then(|r| r.split_once('/'))
        .map(|(_, name)| name)
        .filter(|name| resolve(name).is_some())
        .or_else(|| {
            LOCAL_DEFAULTS
                .iter()
                .copied()
                .find(|b| resolve(b).is_some())
        });
    let current = git(&["symbolic-ref", "--quiet", "--short", "HEAD"])
        .map(|b| b.trim().to_string())
        .ok();
    let local = local_name
        .filter(|name| current.as_deref() != Some(*name))
        .map(str::to_string);

    let mut best: Option<(String, String)> = None;
    for name in remote.into_iter().chain(local) {
        let base = git(&["merge-base", "HEAD", &name])?.trim().to_string();
        let newer = match &best {
            None => true,
            Some((current, _)) => is_ancestor(current, &base),
        };
        if newer {
            best = Some((base, name));
        }
    }
    best.ok_or_else(|| {
        "can't find where this branch started; pass --since with the commit to start after"
            .to_string()
    })
}

fn is_ancestor(ancestor: &str, of: &str) -> bool {
    git(&["merge-base", "--is-ancestor", ancestor, of]).is_ok()
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

/// Oldest first.
fn commits_between(base: &str, head: &str) -> Result<Vec<Commit>, String> {
    let range = format!("{base}..{head}");
    let log = git(&[
        "log",
        "--reverse",
        "--topo-order",
        "--format=%H%x00%B%x1e",
        &range,
        "--",
    ])?;
    Ok(log
        .split('\x1e')
        .filter_map(|record| {
            let (sha, message) = record.trim_start_matches('\n').split_once('\0')?;
            Some(Commit {
                sha: sha.to_string(),
                message: message.to_string(),
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
        let o = Options::parse(&["-n", "--since", "v1.0"]).unwrap();
        assert!(o.dry_run);
        assert_eq!(o.since.as_deref(), Some("v1.0"));
        assert!(Options::parse(&["main"]).is_err());
        assert!(Options::parse(&["--since"]).is_err());
        assert!(Options::parse(&["--force"]).is_err());
    }

    #[test]
    fn adds_amends_after_planned_picks() {
        let todo = "label onto\n\
                    reset onto\n\
                    pick 1111111 add a flag\n\
                    pick 2222222 docs: readme\n\
                    # pick 3333333 commented out\n";
        let plan = [("1111111abcdef", "feat(cli): ")];
        assert_eq!(
            add_amends(todo, &plan, "'/bin/jev-cc'"),
            "label onto\n\
             reset onto\n\
             pick 1111111 add a flag\n\
             exec '/bin/jev-cc' __reword-amend 'feat(cli): '\n\
             pick 2222222 docs: readme\n\
             # pick 3333333 commented out\n"
        );
    }

    #[test]
    fn quotes_for_the_shell() {
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
