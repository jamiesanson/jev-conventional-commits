mod classify;
mod diff;
mod jev;
mod message;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use classify::{Classification, Settings};

const USAGE: &str = "\
jev-cc: prefix commit messages with Conventional Commits syntax using TypeSafe's Jev

USAGE:
    jev-cc install                       Install the git hooks into the current repository
    jev-cc classify [MESSAGE]            Classify the staged diff and print the prefix
    jev-cc prepare-commit-msg FILE [SOURCE] [SHA]   (git hook)
    jev-cc commit-msg FILE                          (git hook)

ENVIRONMENT:
    TYPESAFE_API_KEY             API key for Jev (required for anything local rules can't decide)
    JEV_CC_BASE_URL              API base URL (default: https://api.typesafe.ai)
    JEV_CC_TIMEOUT_MS            Request timeout; the message is left as-is on timeout (default: 1000)
    JEV_CC_MIN_CONFIDENCE        Minimum confidence to apply a type (default: 0.6)
    JEV_CC_BREAKING_THRESHOLD    Probability needed to mark a change breaking (default: 0.85)
    JEV_CC_DISABLE               Set to 1 to skip the hook entirely
";

const HOOK_MARKER: &str = "# installed by jev-cc";
const BIN_NAME: &str = if cfg!(windows) {
    "jev-cc.exe"
} else {
    "jev-cc"
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    match args.as_slice() {
        ["prepare-commit-msg", file, rest @ ..] => {
            hook_result(prepare_commit_msg(Path::new(file), rest.first().copied()))
        }
        ["commit-msg", file] => hook_result(commit_msg(Path::new(file))),
        ["classify", message @ ..] => match run_classify(&message.join(" ")) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("jev-cc: {e}");
                ExitCode::FAILURE
            }
        },
        ["install"] => match install() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("jev-cc: {e}");
                ExitCode::FAILURE
            }
        },
        ["--version" | "-V"] => {
            println!("jev-cc {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// Hooks never block a commit: failures are reported and the message is left alone.
fn hook_result(result: Result<(), String>) -> ExitCode {
    if let Err(e) = result {
        eprintln!("jev-cc: message left unchanged ({e})");
    }
    ExitCode::SUCCESS
}

fn prepare_commit_msg(file: &Path, source: Option<&str>) -> Result<(), String> {
    if env_flag("JEV_CC_DISABLE") {
        return Ok(());
    }
    // merge, squash and commit (-c/-C/--amend) reuse an existing message.
    if matches!(source, Some("merge" | "squash" | "commit")) {
        return Ok(());
    }

    let content = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    let body = message::body(&content);
    let subject = body.lines().next().unwrap_or_default();
    if message::should_skip(subject) {
        return Ok(());
    }

    let diff = diff::staged()?;
    if diff.files.is_empty() {
        return Ok(());
    }
    let classification = classify_with_env(&diff, &body)?;
    let min_confidence = env_f64("JEV_CC_MIN_CONFIDENCE", 0.6);
    if classification.confidence < min_confidence {
        return Err(format!(
            "best guess {} at {:.0}% confidence, below {:.0}%",
            classification.kind,
            classification.confidence * 100.0,
            min_confidence * 100.0
        ));
    }

    write_atomic(
        file,
        &message::apply_prefix(&content, &classification.prefix()),
    )
}

/// Clears a message that is only the prefix we suggested, so quitting the editor
/// without writing anything still aborts the commit.
fn commit_msg(file: &Path) -> Result<(), String> {
    let content = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    if message::is_bare_prefix(&content) {
        write_atomic(file, "")?;
    }
    Ok(())
}

/// Writes via a temporary file and a rename, so an interrupted write can't leave git a
/// truncated commit message.
fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".jev-cc.tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, contents)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            e.to_string()
        })
}

fn run_classify(message: &str) -> Result<(), String> {
    let started = Instant::now();
    let diff = diff::staged()?;
    if diff.files.is_empty() {
        return Err("nothing staged".into());
    }
    let c = classify_with_env(&diff, message)?;
    println!("{}{message}", c.prefix());
    eprintln!(
        "  source: {:?}, confidence: {:.0}%, took {}ms",
        c.source,
        c.confidence * 100.0,
        started.elapsed().as_millis()
    );
    Ok(())
}

fn classify_with_env(diff: &diff::StagedDiff, message: &str) -> Result<Classification, String> {
    let client = std::env::var("TYPESAFE_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())
        .map(|key| {
            let base =
                std::env::var("JEV_CC_BASE_URL").unwrap_or_else(|_| jev::DEFAULT_BASE_URL.into());
            let timeout = Duration::from_millis(env_f64("JEV_CC_TIMEOUT_MS", 1000.0) as u64);
            jev::Client::new(key, &base, timeout)
        });
    let settings = Settings {
        breaking_threshold: env_f64("JEV_CC_BREAKING_THRESHOLD", 0.85),
    };
    classify::classify(client.as_ref(), diff, message, &settings)
}

fn install() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let hooks_dir = git_hooks_dir()?;
    std::fs::create_dir_all(&hooks_dir).map_err(|e| e.to_string())?;

    for hook in ["prepare-commit-msg", "commit-msg"] {
        let path = hooks_dir.join(hook);
        if let Ok(existing) = std::fs::read_to_string(&path)
            && !existing.contains(HOOK_MARKER)
        {
            return Err(format!(
                "{} already exists and wasn't installed by jev-cc; add `jev-cc {hook} \"$@\"` to it instead",
                path.display()
            ));
        }
        std::fs::write(&path, hook_script(hook, &exe)).map_err(|e| e.to_string())?;
        make_executable(&path)?;
        println!("installed {}", path.display());
    }
    Ok(())
}

/// Hooks run the first candidate that exists and always exit 0, so a missing or broken
/// binary (wrong architecture, truncated download) never blocks a commit. It isn't
/// `exec`ed: a failed `exec` would end the hook with a non-zero status.
///
/// The first candidate is an absolute path, so hooks work from git GUIs that don't
/// inherit the shell's PATH. When the binary running `install` is the one on PATH, that
/// PATH entry is used (e.g. `/opt/homebrew/bin/jev-cc`) rather than `current_exe()`,
/// which can resolve into a versioned directory that disappears on upgrade. The bare
/// name is the last resort for when that path moves.
fn hook_script(hook: &str, exe: &Path) -> String {
    let stable = match find_on_path(BIN_NAME) {
        Some(found) if same_file(&found, exe) => found,
        _ => exe.to_path_buf(),
    };
    let candidates = format!("\"{}\" {BIN_NAME}", stable.display());
    format!(
        "#!/bin/sh\n\
         {HOOK_MARKER}\n\
         for bin in {candidates}; do\n\
         \x20   if command -v \"$bin\" >/dev/null 2>&1; then\n\
         \x20       \"$bin\" {hook} \"$@\"\n\
         \x20       exit 0\n\
         \x20   fi\n\
         done\n\
         exit 0\n"
    )
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn git_hooks_dir() -> Result<PathBuf, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-path", "hooks"])
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    if !output.status.success() {
        return Err("not inside a git repository".into());
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
