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

If the model isn't confident enough, times out, or can't be reached, your message is left
unchanged. The hook never blocks a commit.

## Design goals

- **Fast.** A single static Rust binary with no async runtime, and one HTTPS request per
  commit. The model call is the only noticeable cost.
- **Safe to fail.** Every error path leaves the message as it was and exits 0.
- **Out of the way.** Skips merges, squashes, amends, and messages that are already
  conventional.

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

Then, in each repository:

```sh
export TYPESAFE_API_KEY=...   # from TypeSafe
jev-cc install
```

`install` adds two hooks to the repository:

- `prepare-commit-msg` classifies the staged diff and adds the prefix. With `git commit -m`
  the prefix goes in front of your message. With the editor, the first line is pre-filled
  (e.g. `fix(config): `) for you to finish.
- `commit-msg` clears a message that is only the pre-filled prefix, so quitting the editor
  without writing anything still aborts the commit.

If a hook already exists, `install` won't overwrite it. Add `jev-cc <hook> "$@"` to it
yourself.

Try it without committing:

```console
$ git add -p
$ jev-cc classify "handle empty config files"
fix(config): handle empty config files
  source: Jev, confidence: 87%, took 180ms
```

## Configuration

| Variable | Default | |
|---|---|---|
| `TYPESAFE_API_KEY` | | Required for anything the local rules can't decide |
| `JEV_CC_TIMEOUT_MS` | `1000` | The message is left unchanged on timeout |
| `JEV_CC_MIN_CONFIDENCE` | `0.6` | Below this, the message is left unchanged |
| `JEV_CC_BREAKING_THRESHOLD` | `0.85` | Probability needed to add `!` |
| `JEV_CC_BASE_URL` | `https://api.typesafe.ai` | |
| `JEV_CC_DISABLE` | | Set to `1` to skip the hook |

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

## Development

```sh
cargo test
cargo build --release
./target/release/jev-cc install   # dogfood: classify this repo's own commits
```

Hooks installed from a binary on `PATH` call `jev-cc` by name, so upgrades keep working.
Hooks installed from a local build, as above, call it by absolute path.

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
