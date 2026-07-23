# Repository agent instructions

These instructions apply to Codex, Claude Code, and every other AI agent working anywhere in this repository.

## Required context

Before changing files, read `CLAUDE.md` for the project conventions and `CONTRIBUTING.md` for the human-facing collaboration workflow. Treat this file as the mandatory AI operating policy. If the collaboration or release policy changes, update all three files in the same change so they cannot drift.

## Commit messages

All commit messages must follow [`docs/commit.md`](docs/commit.md).

## Identity and workflow selection

1. Determine both the authenticated GitHub login and the remote repository owner before starting work:

   ```sh
   login="$(gh api user --jq .login)"
   owner="$(gh repo view --json owner --jq .owner.login)"
   ```

2. Inspect `git status` before switching branches or pulling. Never discard or overwrite another person's uncommitted work.
3. When `login == owner`, use the owner workflow:
   - Work directly on a clean, up-to-date `main`; do not create a work branch or pull request.
   - Before committing user-requested work, create a GitHub Issue or reuse the existing issue for that request.
   - Link every such commit to the issue with `Refs: #<number>` or `Closes: #<number>`, run the relevant validation, and push normally to `origin/main`.
   - The owner may use the administrator bypass only to omit the pull-request requirement. Never force-push, delete `main`, conceal failed validation, or bypass release restrictions.
   - Observe the required `main` CI jobs after pushing. If a job fails, fix it under the same issue before treating the work as complete.
4. When `login != owner`, use `work/<exact-github-login>/<short-topic>`. The login segment is case-sensitive and must exactly match the account that will run the canary workflow.
5. Contributors push only their own work branch and open a pull request into `main`. They must not push directly to `main`, use another contributor's namespace, or bypass protection rules.

## Pull requests and main

- Contributor pull requests target `main`.
- Contributor changes require the `build` and `windows-check` jobs, an up-to-date branch, one approval, resolved conversations, linear history, and CODEOWNERS review for protected files.
- Stale approvals are dismissed, and the author of the latest push cannot provide the final approval.
- The repository owner works through an issue-linked direct `main` commit instead of a pull request, validates before pushing, and verifies the same CI jobs after pushing.
- Never force-push or delete `main`, and never attempt to hide or ignore a failed protection or CI result.
- GitHub Actions, version files, and these policy documents are owned by `@json-choi`. Do not weaken their CODEOWNERS coverage.

## Contributor canary releases

Non-owner contributors may publish only their own isolated canary. After pushing the work branch, dispatch the trusted workflow definition from `main`:

```sh
branch="$(git branch --show-current)"
git push -u origin "$branch"
gh workflow run canary.yml --ref main -f source_ref="$branch"
```

The workflow accepts the branch only when it starts with `work/${GITHUB_ACTOR}/`. It resolves that branch to an immutable commit SHA and publishes `canary-<login>-<run>-<attempt>` through `canary-<login>`.

Canaries are unsigned internal-test prereleases. They must never receive `TAURI_SIGNING_PRIVATE_KEY`, updater signatures, updater archives, stable direct-download aliases, or `latest.json`. Do not create canary tags or GitHub Releases manually; let `.github/workflows/canary.yml` create them. Do not present a canary as a stable/public build.

## Stable releases

Only `json-choi`, after an explicit user request, may publish a stable version.

1. Keep the same version in `package.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`, and the `dopedb` package entry in `Cargo.lock`.
2. Track the version change in an issue, commit it directly to an up-to-date `main` with the issue footer, and push normally.
3. Verify the required CI jobs pass on that `main` commit.
4. Create `app-vX.Y.Z` on that commit and push it as `json-choi`. Never use a plain `vX.Y.Z` tag and never create a stable tag from a contributor work branch.
5. The `stable-release` environment accepts only `app-v*` tags and requires approval from `json-choi`. Do not approve or bypass it unless the user explicitly asked to release that version.
6. The workflow checks the actor, all version sources, and `main` ancestry; builds into a draft; and publishes only after every platform and alias upload succeeds.

All tags other than `canary-*` are protected by the `owner-only-tags-except-canary` ruleset. Never try to work around this rule. Release immutability protects releases published after it was enabled on 2026-07-13; it does not retroactively protect older releases.

## GitHub permission caveat

This is a personal-account repository. GitHub gives collaborators general release creation/editing capability; it does not provide a repository setting that removes only that capability. Therefore, do not claim that collaborator Release UI/API access has been fully revoked. The supported official path is secured through owner-only non-canary tags, the protected stable environment, the trusted workflow, CODEOWNERS, and future immutable releases. Exact platform-level separation would require an organization with suitable granular roles or a fork-only contribution model.

## Validation and handoff

- Run checks proportional to the files changed. For workflow changes, run `actionlint`; for application changes, at minimum run the relevant TypeScript, site, and Rust checks documented in `CLAUDE.md`.
- Report the branch, commit, checks, and any pending GitHub approval or invitation accurately.
- Never expose, copy, print, or move signing keys and repository secrets.
