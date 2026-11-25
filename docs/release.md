# Release runbook

How `smbs` binaries are published. [`release.yml`](../.github/workflows/release.yml)
does the work; [`release-targets.json`](../release-targets.json) is the single
list of targets, runners, and archive formats.

## What a release contains

For version `X.Y.Z`, tag `vX.Y.Z`:

- `smbs-X.Y.Z-<target>.tar.gz` (or `.zip` on Windows), each holding the
  binary and `LICENSE.md`;
- `install.sh` and `install.ps1`, generated from
  [`scripts/install/`](../scripts/install/) with each archive's SHA-256 embedded;
- `SHA256SUMS` covering all of the above.

Every target is built on its own native runner. Before anything is
published, the generated installer runs on that runner, and the installed
binary runs `--version`, `--help`, `currencies`, and `version`.

## Steps

1. **Candidate.** Pull requests that touch release files already run the
   release workflow without publishing. For other changes, run it by hand
   with an empty tag: `gh workflow run release.yml`.
2. **Version.** Set `version` in the root `Cargo.toml` (and the `smbs`
   dependency version in `crates/smbs-cli/Cargo.toml`), then merge to `main`.
3. **CI on main.** Wait for the `CI` workflow's push run on that commit to
   pass. It covers Linux, macOS, and Windows.
4. **Tag.** Confirm the tag is unused, then tag the merged commit:
   `git tag vX.Y.Z <sha> && git push origin vX.Y.Z`.
5. **Publish.** Run `gh workflow run release.yml -f tag=vX.Y.Z`. The workflow
   checks these gates before building:
   - the maintainer started the run;
   - the tag matches the Cargo version;
   - the tagged commit is on `main` and has a successful CI push run;
   - no release exists for the tag yet.

   It then builds, installs on every target, creates a draft release,
   downloads the assets to compare hashes, and publishes the draft as latest.
6. **Check.** Install with the one-line command from the README on a real
   machine and run `smbs version`.

## Recovery

- Never move or reuse a published tag, and never replace an asset. If a
  release is wrong, ship `X.Y.(Z+1)`.
- If the workflow fails before `publish`, nothing is public: fix the problem
  and rerun with the same tag.
- If it fails inside `publish`, a draft release may exist. Delete the draft
  (`gh release delete vX.Y.Z`; the tag stays) and rerun.

## Platform notes

- Linux binaries are built on Ubuntu 22.04 and need glibc 2.35 or newer. The
  build job logs the highest glibc symbol version the binary uses.
- Windows and macOS binaries are not code-signed. macOS may quarantine a
  browser-downloaded archive; the curl installer is not affected.
