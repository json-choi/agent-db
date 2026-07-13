# Repository agent instructions

These instructions apply to Codex, Claude Code, and every other AI agent working anywhere in this repository.

## Required context

Before changing files, read `CLAUDE.md` for the project conventions and `CONTRIBUTING.md` for the human-facing collaboration workflow. Treat this file as the mandatory AI operating policy. If the collaboration or release policy changes, update all three files in the same change so they cannot drift.

## Identity and work branches

1. Determine the authenticated GitHub login before creating a branch or canary:

   ```sh
   gh api user --jq .login
   ```

2. Inspect `git status` before switching branches. Never discard or overwrite another person's uncommitted work.
3. Normal work must use `work/<exact-github-login>/<short-topic>`. The login segment is case-sensitive and must exactly match the account that will run the canary workflow.
4. Push only the contributor's own work branch. Do not push directly to `main` or another contributor's namespace.
5. The sole repository owner, `json-choi`, may use the `main` administrator bypass only when the user explicitly requests a direct administrative/bootstrap change. It is not the normal development path.

## Pull requests and main

- Open pull requests into `main`.
- `main` requires the `build` and `windows-check` jobs, an up-to-date branch, one approval, resolved conversations, linear history, and CODEOWNERS review for protected files.
- Stale approvals are dismissed, and the author of the latest push cannot provide the final approval.
- Never force-push or delete `main`, and never attempt to bypass a failed or pending protection rule.
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
2. Merge the version change into `main` and verify the required CI jobs pass.
3. Create `app-vX.Y.Z` on that merged commit and push it as `json-choi`. Never use a plain `vX.Y.Z` tag and never create a stable tag from an unmerged work branch.
4. The `stable-release` environment accepts only `app-v*` tags and requires approval from `json-choi`. Do not approve or bypass it unless the user explicitly asked to release that version.
5. The workflow checks the actor, all version sources, and `main` ancestry; builds into a draft; and publishes only after every platform and alias upload succeeds.

All tags other than `canary-*` are protected by the `owner-only-tags-except-canary` ruleset. Never try to work around this rule. Release immutability protects releases published after it was enabled on 2026-07-13; it does not retroactively protect older releases.

## GitHub permission caveat

This is a personal-account repository. GitHub gives collaborators general release creation/editing capability; it does not provide a repository setting that removes only that capability. Therefore, do not claim that collaborator Release UI/API access has been fully revoked. The supported official path is secured through owner-only non-canary tags, the protected stable environment, the trusted workflow, CODEOWNERS, and future immutable releases. Exact platform-level separation would require an organization with suitable granular roles or a fork-only contribution model.

## Validation and handoff

- Run checks proportional to the files changed. For workflow changes, run `actionlint`; for application changes, at minimum run the relevant TypeScript, site, and Rust checks documented in `CLAUDE.md`.
- Report the branch, commit, checks, and any pending GitHub approval or invitation accurately.
- Never expose, copy, print, or move signing keys and repository secrets.
