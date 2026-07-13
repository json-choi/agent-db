# Collaboration workflow

## Work branches

Each contributor works in a branch under their GitHub login:

```text
work/<github-login>/<short-topic>
```

For example, `PENEKhun` uses `work/PENEKhun/query-history`. Open a pull request into `main` when the change is ready. `main` requires the macOS and Windows CI jobs, one approval, resolved conversations, and an up-to-date branch. Force pushes and deletion are blocked.

Files that control GitHub Actions or the application version are owned by `@json-choi` through `CODEOWNERS`, so changing them also requires the owner's review.

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

1. Update `package.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`, and `Cargo.lock` to the same version through a pull request.
2. Merge the pull request into `main` after CI and review pass.
3. Create and push `app-vX.Y.Z` from the merged commit.
4. Approve the pending `stable-release` environment deployment.

All tags other than `canary-*` are owner-only. The release workflow rejects tags whose version sources do not match or whose commit is not in `main`. It uploads all installers to a draft, then publishes the completed release so release immutability can protect its tag and assets.
