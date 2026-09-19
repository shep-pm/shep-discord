# Handoff: first-publish prep, 2026-09-19

## Done

- `Wired<B>` parameterised over the board, four tests on `on_process_event`,
  each proven red against its own mutation. Merged to local `main`.
- `exclude = ["/docs/"]` in shep-discord (merged locally), shep-log-rotate
  (PR #25) and shep-deploy (PR #21). Both PRs are open and unmerged.
- Worktree `clever-ishizaka-b767bd` and its branch removed, verified merged
  and clean first.

## Left

- **Create `shep-pm/shep-discord` on GitHub and add the remote.** It does not
  exist. This repo has 122 commits and no remote at all.
- Merge the two open PRs.
- Remove this worktree from outside it:
  `git -C <repo> worktree remove .claude/worktrees/epic-jepsen-5da82e`
  then `git branch -d c/epic-jepsen-5da82e`.
- Decide on the changelog question below.

## What the code does not show

**shep itself needed nothing, and the reason is structural.** Its `docs/` is
at the workspace root, and every publishable crate has its package root at
`crates/<name>/`. `cargo package` only walks files under a package root, so
root-level `docs/` was never in any of the seven tarballs. Verified with
`cargo package --list -p <crate> | grep -c '^docs/'` across all seven, not
inferred. A flat-layout crate is the only shape that has this problem, which
is why the three dogs had it and the workspace did not.

**The changelog will come out thin on first release.** `filter_unconventional
= true` plus the docs/test/ci/chore/style skips in release-plz-changelog.toml
means 29 of 122 commits survive into a generated changelog. The other 93,
including most of the monitor work, get dropped silently. Nothing is broken;
it is a question of whether 0.1.0 gets a generated changelog or a written one.

**Other internal files still ship** in all three dogs: `CLAUDE.md`,
`.coderabbit.yaml`, `release-plz.toml`, `release-plz-changelog.toml` and
`.github/`. Deliberately left alone. Excluding them is one decision across
every dog, not a fourth line in this change.

**`shep-deploy` has no `readme =` key** in its `[package]`. Cargo auto-detects
`README.md` so it ships anyway, but it is the only dog of the three without
the key written out.
