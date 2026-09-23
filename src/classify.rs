//! Decides the conventional commit type, scope and breaking flag for a staged diff.
//!
//! Local rules answer obvious cases for free. Everything else is a single Jev call.

use std::collections::BTreeMap;

use crate::diff::{self, ChangedFile, Outgoing};
use crate::jev::{self, Answer, NoulCriteria, Question};

/// Conventional commit types and the criteria Jev uses to tell them apart.
pub const TYPES: &[(&str, &str)] = &[
    ("feat", "Adds new user-facing behaviour or capability"),
    ("fix", "Corrects a bug or wrong behaviour"),
    ("docs", "Changes documentation only"),
    (
        "style",
        "Formatting or whitespace only; no change in behaviour",
    ),
    ("refactor", "Restructures code without changing behaviour"),
    ("perf", "Improves performance without changing behaviour"),
    ("test", "Adds or changes tests only"),
    (
        "build",
        "Changes the build system, packaging or dependencies",
    ),
    ("ci", "Changes CI configuration or pipelines"),
    ("chore", "Maintenance that doesn't fit another type"),
    ("revert", "Reverts an earlier commit"),
];

const TYPE_INSTRUCTIONS: &str =
    "Which Conventional Commits type best describes the primary purpose of this staged change?";
const BREAKING_INSTRUCTIONS: &str = "Does this change break backwards compatibility for users of \
     the code, such as removing or renaming a public API, changing a signature, or changing \
     default behaviour?";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Local,
    Jev,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Classification {
    pub kind: &'static str,
    pub scope: Option<String>,
    pub breaking: bool,
    pub confidence: f64,
    pub source: Source,
}

impl Classification {
    /// The `type(scope)!: ` prefix.
    pub fn prefix(&self) -> String {
        let scope = self
            .scope
            .as_ref()
            .map(|s| format!("({s})"))
            .unwrap_or_default();
        let bang = if self.breaking { "!" } else { "" };
        format!("{}{scope}{bang}: ", self.kind)
    }
}

pub struct Settings {
    /// Probability above which a change is marked breaking.
    pub breaking_threshold: f64,
}

pub fn classify(
    client: Option<&jev::Client>,
    files: &[ChangedFile],
    exclude: &[String],
    message: &str,
    settings: &Settings,
) -> Result<Classification, String> {
    let scope = scope_for(files);

    if let Some(kind) = local_type(files) {
        return Ok(Classification {
            kind,
            scope,
            breaking: false,
            confidence: 1.0,
            source: Source::Local,
        });
    }

    let client = client.ok_or_else(|| jev::Error::MissingApiKey.to_string())?;
    let outgoing = diff::outgoing(files, exclude)?;
    if outgoing.files.is_empty() {
        return Err("every changed file is excluded".into());
    }
    let response = client
        .system_one(&request(&outgoing, message))
        .map_err(|e| e.to_string())?;

    let (kind, confidence) = match response.answers.get("type") {
        Some(Answer::Choice { choice, confidence }) => TYPES
            .iter()
            .find(|(t, _)| t == choice)
            .map(|(t, _)| (*t, *confidence))
            .ok_or_else(|| format!("unexpected type from Jev: {choice}"))?,
        _ => return Err("response had no type answer".into()),
    };
    let breaking = matches!(
        response.answers.get("breaking"),
        Some(Answer::Noul { noul }) if *noul >= settings.breaking_threshold
    );

    Ok(Classification {
        kind,
        scope,
        breaking,
        confidence,
        source: Source::Jev,
    })
}

fn request<'a>(outgoing: &Outgoing, message: &str) -> jev::Request<'a> {
    let files: Vec<String> = outgoing
        .files
        .iter()
        .map(|f| format!("{} {}", f.status, f.path))
        .collect();
    let mut state = serde_json::json!({ "files": files, "patch": outgoing.patch });
    if !message.is_empty() {
        state["message"] = serde_json::Value::from(message);
    }

    let mut questions = BTreeMap::new();
    questions.insert(
        "type",
        Question::Choice {
            instructions: TYPE_INSTRUCTIONS,
            criteria: TYPES.iter().copied().collect(),
        },
    );
    questions.insert(
        "breaking",
        Question::Noul {
            instructions: BREAKING_INSTRUCTIONS,
            criteria: NoulCriteria {
                yes: "Existing callers or users must change something",
                no: "Existing callers and users are unaffected",
            },
        },
    );

    jev::Request {
        model: jev::DEFAULT_MODEL,
        state,
        questions,
    }
}

/// Answers without the model when every changed file points the same way.
fn local_type(files: &[ChangedFile]) -> Option<&'static str> {
    if files.is_empty() {
        return None;
    }
    let all = |f: fn(&str) -> bool| files.iter().all(|c| f(&c.path));
    if all(is_docs) {
        Some("docs")
    } else if all(is_test) {
        Some("test")
    } else if all(is_ci) {
        Some("ci")
    } else {
        None
    }
}

fn is_docs(path: &str) -> bool {
    let name = file_name(path).to_ascii_lowercase();
    path.starts_with("docs/")
        || [".md", ".mdx", ".rst", ".adoc"]
            .iter()
            .any(|ext| name.ends_with(ext))
        || name.starts_with("license")
}

fn is_test(path: &str) -> bool {
    let name = file_name(path);
    path.split('/')
        .any(|dir| matches!(dir, "tests" | "test" | "__tests__" | "spec"))
        || name.starts_with("test_")
        || name.contains("_test.")
        || name.contains(".test.")
        || name.contains(".spec.")
}

fn is_ci(path: &str) -> bool {
    path.starts_with(".github/workflows/")
        || path.starts_with(".circleci/")
        || path.starts_with(".buildkite/")
        || path == ".gitlab-ci.yml"
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The directory every changed file shares, skipping container directories like `src/`.
/// Files at the repository root, or spread across directories, get no scope.
fn scope_for(files: &[ChangedFile]) -> Option<String> {
    const CONTAINERS: &[&str] = &[
        "src", "lib", "crates", "packages", "apps", "pkg", "internal",
    ];

    let mut scopes = files.iter().map(|f| {
        let mut parts = f.path.split('/').collect::<Vec<_>>();
        parts.pop(); // file name
        parts.into_iter().find(|p| !CONTAINERS.contains(p))
    });
    let first = scopes.next()??;
    scopes.all(|s| s == Some(first)).then(|| first.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(paths: &[&str]) -> Vec<ChangedFile> {
        paths
            .iter()
            .map(|p| ChangedFile {
                status: 'M',
                path: p.to_string(),
            })
            .collect()
    }

    #[test]
    fn docs_only_is_local() {
        assert_eq!(
            local_type(&files(&["README.md", "docs/setup.txt"])),
            Some("docs")
        );
    }

    #[test]
    fn tests_only_is_local() {
        assert_eq!(
            local_type(&files(&[
                "tests/cli.rs",
                "src/app.test.ts",
                "pkg/x_test.go"
            ])),
            Some("test")
        );
    }

    #[test]
    fn mixed_change_needs_model() {
        assert_eq!(local_type(&files(&["README.md", "src/main.rs"])), None);
    }

    #[test]
    fn scope_from_shared_directory() {
        assert_eq!(
            scope_for(&files(&["src/config/load.rs", "src/config/mod.rs"])),
            Some("config".into())
        );
        assert_eq!(
            scope_for(&files(&["packages/api/src/index.ts"])),
            Some("api".into())
        );
    }

    #[test]
    fn no_scope_for_root_or_spread_changes() {
        assert_eq!(scope_for(&files(&["src/main.rs"])), None);
        assert_eq!(scope_for(&files(&["src/a/x.rs", "src/b/y.rs"])), None);
        assert_eq!(scope_for(&files(&["Cargo.toml", "src/a/x.rs"])), None);
    }

    #[test]
    fn prefix_formatting() {
        let c = Classification {
            kind: "feat",
            scope: Some("api".into()),
            breaking: true,
            confidence: 0.9,
            source: Source::Jev,
        };
        assert_eq!(c.prefix(), "feat(api)!: ");
    }
}
