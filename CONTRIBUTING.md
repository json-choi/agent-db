# Collaboration workflow

AI assistants must read and follow both `AGENTS.md` and `CLAUDE.md` before changing this repository. `CONTRIBUTING.md` is the human-facing workflow; when collaboration or release rules change, keep all three files synchronized in the same change.

## Commit messages

Write commit messages according to [`docs/commit.md`](docs/commit.md).

## Choose the workflow

Before changing files, confirm both identities and inspect the worktree:

```sh
login="$(gh api user --jq .login)"
owner="$(gh repo view --json owner --jq .owner.login)"
git status --short --branch
```

Never discard another person's uncommitted work.

### Repository owner

When `login` and `owner` match, work directly on a clean, up-to-date `main`. Do not create a work branch or pull request.

1. Reuse an existing GitHub Issue for the user request or create one before committing.
2. Implement and run the relevant validation on `main`.
3. Write a Korean Conventional Commit with `Refs: #<number>` or `Closes: #<number>` in the footer.
4. Push normally with `git push origin main`.
5. Verify the required `build` and `windows-check` jobs after the push. If either fails, fix it under the same issue.

The owner administrator bypass is used only to omit the pull-request requirement. It does not permit force-pushing, deleting `main`, concealing failed validation, or bypassing release restrictions.

### Contributors

Each non-owner contributor works in a branch under their GitHub login:

```text
work/<github-login>/<short-topic>
```

For example, `PENEKhun` uses `work/PENEKhun/query-history`. Open a pull request into `main` when the change is ready. `main` requires the macOS and Windows CI jobs, one approval, resolved conversations, and an up-to-date branch. Force pushes and deletion are blocked.

Files that control GitHub Actions or the application version are owned by `@json-choi` through `CODEOWNERS`, so changing them also requires the owner's review.

The branch login segment is case-sensitive and must match the authenticated login exactly. Contributors must not push directly to `main`, use another contributor's namespace, or bypass required checks and reviews.

## Personal canary builds

Push your work branch, then dispatch the trusted workflow from `main`:

```sh
git push -u origin work/<github-login>/<short-topic>
gh workflow run canary.yml \
  --ref main \
  -f source_ref='work/<github-login>/<short-topic>'
```

The login in `source_ref` must exactly match the account that starts the workflow. A successful run publishes an immutable prerelease named `canary-<github-login>-<run>-<attempt>` through the contributor's own `canary-<github-login>` environment.

Canary installers are deliberately unsigned and do not include Tauri updater artifacts or `latest.json`. They are isolated from the stable updater and are for internal testing only.

## Stable releases

Only `@json-choi` publishes stable versions:

1. Create or reuse a release issue.
2. Update `package.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`, and `Cargo.lock` to the same version directly on an up-to-date `main`.
3. Commit with the issue footer, push normally, and verify the required CI jobs pass.
4. Create and push `app-vX.Y.Z` from that `main` commit.
5. Approve the pending `stable-release` environment deployment.

All tags other than `canary-*` are owner-only. The release workflow rejects tags whose version sources do not match or whose commit is not in `main`. It uploads all installers to a draft, then publishes the completed release so release immutability can protect its tag and assets.
