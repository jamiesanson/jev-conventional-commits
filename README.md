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

## Status

Early development. See the open branches for progress.
