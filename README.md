# jev-conventional-commits

A fast git hook that classifies your staged diff and prefixes your commit message with
[Conventional Commits](https://www.conventionalcommits.org/) syntax, using
[TypeSafe's Jev](https://docs.typesafe.ai/api) model.

```console
$ git commit -m "handle empty config files"
$ git log -1 --format=%s
fix(config): handle empty config files
```

## How it works

Jev doesn't write text. It picks an answer from a set you define up front and says how
confident it is. That suits this tool:

- **You** write the description.
- **Jev** picks the `type` (`feat`, `fix`, `docs`, …) and whether the change is breaking (`!`).
- **Local rules** pick the `scope` from the paths you changed, and skip the model call
  when the answer is obvious (for example, a docs-only diff).

If the model isn't confident enough, or can't be reached, your message is left unchanged.

## Install

macOS and Linux:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/jamiesanson/jev-conventional-commits/releases/latest/download/jev-cc-installer.sh | sh
```

Windows:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/jamiesanson/jev-conventional-commits/releases/latest/download/jev-cc-installer.ps1 | iex"
```

Prebuilt archives for each platform are also on the
[releases page](https://github.com/jamiesanson/jev-conventional-commits/releases). To build from
source, run `cargo install --path .`.

Then:

```sh
jev-cc login     # once: paste your TypeSafe API key
jev-cc install   # in each repository
```

With `git commit -m`, the prefix goes in front of your message. With the editor, the first
line is pre-filled (e.g. `fix(config): `) for you to finish.

`install` won't overwrite existing `prepare-commit-msg` or `commit-msg` hooks. Add
`jev-cc <hook> "$@"` to them yourself.

Try it without committing:

```console
$ git add -p
$ jev-cc classify "handle empty config files"
fix(config): handle empty config files
  source: Jev, confidence: 87%, took 180ms
```

## Configuration

Settings are read from these places, each overriding the one before:

1. `~/.config/jev-cc/config.toml`, for your own defaults
2. `.jev-cc.toml` at the repository root, for settings shared with your team
3. environment variables

```toml
# .jev-cc.toml
min_confidence = 0.75
exclude = ["secrets/**", "**/*.pem"]
```

| Setting | Environment variable | Default | |
|---|---|---|---|
| `timeout_ms` | `JEV_CC_TIMEOUT_MS` | `1000` | Request timeout |
| `deadline_ms` | `JEV_CC_DEADLINE_MS` | `2000` | Time limit for the whole hook |
| `min_confidence` | `JEV_CC_MIN_CONFIDENCE` | `0.6` | Minimum confidence to apply a type |
| `breaking_threshold` | `JEV_CC_BREAKING_THRESHOLD` | `0.85` | Probability needed to add `!` |
| `disable` | `JEV_CC_DISABLE` | `false` | |
| `exclude` | | `[]` | Paths never sent to Jev |
| `base_url` | `JEV_CC_BASE_URL` | `https://api.typesafe.ai` | Global config only |

`exclude` patterns are
[git glob pathspecs](https://git-scm.com/docs/gitglossary#Documentation/gitglossary.txt-aiddefpathspecapathspec),
relative to the repository root. `*` doesn't match across directories, so use `**/*.pem` to
match at any depth. A project's patterns are added to your global ones.

`base_url` can't be set in `.jev-cc.toml`, so a repository you clone can't send your diffs
and API key somewhere else.

Run `jev-cc config` to see the settings in effect and where each one comes from.
`TYPESAFE_API_KEY` overrides the key saved by `jev-cc login`.

## Classification

1. **Skip** merges, squashes, amends, `fixup!`/`squash!` commits, and messages that already
   start with `type:` or `type(scope):`.
2. **Local rules.** If every changed file is documentation, a test or CI config, the type is
   `docs`, `test` or `ci`, and no request is made.
3. **Jev.** Otherwise, one request asks two questions about the changed file list, a trimmed
   patch (lockfiles and binaries omitted, about 24 KB at most) and your message:
   - `type`: a choice between `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`,
     `build`, `ci`, `chore` and `revert`
   - `breaking`: yes or no
4. **Scope** is the directory every changed file shares, skipping container directories
   such as `src/` and `packages/`. For example, changes only in `src/config/` get `(config)`.
   Changes at the root or across several directories get no scope.

## Privacy

When the local rules can't decide, jev-cc sends the following to TypeSafe
(`api.typesafe.ai`, or your `base_url`):

- the paths of your staged files, except those matched by `exclude`
- the patch for those files, trimmed to about 24 KB, with lockfile and binary contents left
  out
- your commit message, if you've written one

The repository name, remote, branch and author are not sent. Diffs that only touch docs,
tests or CI config aren't sent anywhere.

Any secrets in your staged changes are sent along with the patch. Keep sensitive paths out
with `exclude`, catch secrets elsewhere with a secret scanner in a `pre-commit` hook, and
don't install jev-cc in repositories whose code can't leave your machine.

## Development

```sh
cargo test
cargo build --release
./target/release/jev-cc install   # dogfood: classify this repo's own commits
```

### Releasing

Releases are built by [dist](https://opensource.axo.dev/cargo-dist/) in
`.github/workflows/release.yml`. Bump `version` in `Cargo.toml`, then push a matching tag:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

This builds macOS, Linux (glibc and static musl) and Windows binaries, generates the
installers, and publishes a GitHub Release. After changing `dist-workspace.toml`, run
`dist generate` to regenerate the workflow.

## Status

Early development.
