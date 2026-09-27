# Pull request code of conduct

How to open a PR against [WormForge-engine](https://github.com/ropahektic/WormForge-engine). Read this before you open one. Agents: also follow [contributing-with-an-agent.md](../contributing-with-an-agent.md) and the [README](../README.md) contributing section.

## Keep `main` current

1. **Fork**, then work on a branch off **current** `upstream/main` (or `origin/main` if that tracks this repo).
2. **Before you open the PR**, fetch and merge or rebase `main` into your branch so the PR starts clean.
3. **Resolve every conflict on your branch** before you ask for review. Do not open a PR that GitHub already marks as conflicted, and do not leave conflict markers (`<<<<<<<`, `=======`, `>>>>>>>`) in committed files.
4. If `main` moves while your PR is open, update your branch again. Long-lived forks that never catch up create painful review stacks — that is on the author to fix, not the maintainer.

```bash
git fetch upstream
git checkout your-feature
git merge upstream/main
# or: git rebase upstream/main
# fix conflicts, test, then:
git push
```

## One concern per PR

When practical, **one PR = one feature, one pack, or one focused engine verb**.

- Prefer a **Lua pack** (or one new `wa.*` verb) over a dump of unrelated engine changes.
- Do **not** mix a large pack port, an API rename, and an unrelated renderer tweak in the same PR.
- Do **not** “fix” the 71-slot panel by growing the weapon table in a drive-by PR.
- Do **not** vendor `Resources/`, wa-engine.js, collisionmanager.rs, wkaim.dll, or other GPL / game-replacement trees.
- Leave the root `README.md` alone unless the change is the point of the PR. List a new example in `mods/examples/README.md`.
- Do **not** commit agent working notes, scratch plans, or “docs while we figure it out” under `docs/` (or elsewhere in the tree). Those files are not curated; keep them local or in the PR description. Only add a `docs/*.md` when it is intentional maintainer-facing documentation (this file is one such case).

Small, reviewable PRs merge. Kitchen-sink PRs wait or get split.

## What CI expects (especially off Windows)

The game DLL is **32-bit Windows** (`i686-pc-windows-msvc`). You do not need a Windows box to contribute engine or pack work, but you **must** treat CI as the gate.

Workflow: [`.github/workflows/rust.yml`](https://github.com/ropahektic/WormForge-engine/blob/main/.github/workflows/rust.yml)

| Job | Runner | What it proves |
|---|---|---|
| **Host tests** | `ubuntu-latest` | Workspace tests that do not need the WA DLL (`cargo test --workspace`, excluding `wk-lua` / `engine` / `lua-host`, native Linux target). |
| **32-bit Windows DLL** | `windows-latest` | `cargo test -p engine -p lua-host --target i686-pc-windows-msvc`, then `cargo build --release -p wk-lua --target i686-pc-windows-msvc`. |

Locally on Linux/macOS you can still run the host job the same way the workflow does. For engine/`lua-host` behaviour and the real `wkLua.dll`, rely on the Windows CI job (or a Windows machine / VM if you have one).

**A PR is not ready while either job is red.** Fix it on your branch; do not ask the maintainer to debug your build first.

Pack-only PRs still go through the same workflow (the tree must keep compiling). Test the pack in WA **3.8.1** + WormKit when you can; if you cannot, say so in the PR and name what you did verify.

## Before you click “Create pull request”

- [ ] Branch is based on up-to-date `main`; no unresolved conflicts.
- [ ] Scope is one clear change (or you explained why it cannot be split).
- [ ] CI on your branch is green, or you just pushed and will not ping for review until it is.
- [ ] Engine changes wrap stock 3.8.1 and call the original — no trapping `FireWeapon` / `InitWeaponTable`, no stacked VFTs on stock classes.
- [ ] Lockstep-safe: no `math.random`, no wall clock, no local-only input as simulation; use `wa.sorted` when string-key order matters.
- [ ] Lua API renames bump `api_version` and say so in the PR description.
- [ ] Description states **what** changed, **why**, and **how you tested** (CI only / in-game match / which pack).
- [ ] No secrets, no huge unrelated binaries, no accidental `target/` or local WA paths.
- [ ] No uncurated agent scratch docs under `docs/` (or similar) — only curated docs belong in the repo.

## Review and merge

Be responsive to review comments. If the PR drifts from `main` again, update it. Squash or tidy commits if asked. Do not expect maintainers to rebase conflicted multi-feature branches for you.

Maintainers cut releases and `BUILD_ID` stamps — contributors should not bump those unless the PR is explicitly a release chore.

## Questions

Open an issue, or ask in the PR. For “can Lua already do this?” start from the [capabilities matrix](https://ropahektic.github.io/WormForge/capabilities.html) and the example packs under `mods/examples/`.
