# Releasing

The tag-only `Release` workflow publishes to crates.io with
[trusted publishing](https://crates.io/docs/trusted-publishing), then creates a GitHub release
containing the verified source `.crate` archive. No registry token secret belongs in GitHub.

The native code only compiles on macOS, and `cargo publish` verifies by building the packaged
crate for the host. The `gate` and `publish` jobs therefore run on `macos-latest`, not Linux.

## First version, by hand

Crates.io requires an existing crate before a trusted publisher can be configured, so the owner
publishes `0.1.0` manually from the protected-`main` commit that was admitted:

- Run `make all` and `cargo publish --dry-run --locked` on that exact commit, clean tree
- `cargo login` with a short-lived, publish-new-crate scoped token, then `cargo publish --locked`
- Revoke the token and remove the local Cargo login afterwards
- Do not create a tag for `0.1.0` that the workflow would then try to publish again. Create the GitHub release by hand from the exact published archive if wanted, see recovery below

## After the first publication

- On crates.io, add a GitHub trusted publisher: repository `nredd/icc-ptp-safe`, workflow
  filename `release.yml`, environment `crates-io`
- Enable **Require trusted publishing** so no token can publish. Keep a direct individual owner
  with a verified email and linked GitHub account; do not add an Actions bot as owner
- Create the `crates-io` GitHub environment before any tag: required reviewer, administrator
  bypass disabled, `v*` tag-only deployment rule, no secrets. Referencing a missing environment
  can create an unprotected one
- Optionally enable release immutability and a ruleset denying update/deletion of `refs/tags/v*`

Only `publish` receives `id-token: write`; the first-party
[`rust-lang/crates-io-auth-action`](https://github.com/rust-lang/crates-io-auth-action) exchanges
it for a short-lived token. All actions are pinned to full commit SHAs.

## Each later release

- Bump `version` in `Cargo.toml` and `Cargo.lock`, add a `CHANGELOG.md` section for it, and merge a reviewed, gated PR
- Create and push `v<version>` at that exact commit on `main`. Tags cannot be reused; mistakes need a new version
- `gate` checks the commit is an ancestor of `origin/main`, the tag matches the `Cargo.toml` version, runs `make all` and `cargo publish --dry-run --locked`, and uploads the `.crate`
- `publish` waits for environment approval, rebuilds the archive, requires it to be byte-identical to the gated one with `cmp`, authenticates via OIDC and runs `cargo publish --locked`
- `github-release` creates a draft with the archive, then publishes it. Release notes are `CHANGELOG.md`

## Failure and recovery

Do not rerun all jobs blindly. A registry publish can succeed before Cargo times out waiting for
the index, and a draft can exist after a failed release step. Neither registry versions nor
tags can be replaced.

- If Cargo definitely did not publish, fix the cause and retry only at the same commit
- If `publish` succeeded and `github-release` failed, retry only that job in the original run while the artifact is retained (30 days)
- If status is ambiguous, check the tag resolves to the admitted SHA, the crates.io version is as expected, and the downloaded registry `.crate` matches the gated archive byte for byte. A matching version string alone is NOT evidence. If bytes differ, publish a corrected new version

The `.crate` is the source archive only; there are no prebuilt binaries.
