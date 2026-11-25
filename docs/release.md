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
   without `publish`: `gh workflow run release.yml --ref <branch>`.
2. **Version.** Set `version` in the root `Cargo.toml` and the `smbs`
   dependency version in `crates/smbs-cli/Cargo.toml`. Run `cargo check` to
   update `Cargo.lock`, which CI and releases use with `--locked`. Commit all
   three files and merge to `main`.
3. **CI on main.** Wait for the `CI` workflow's push run on that commit to
   pass. It covers Linux, macOS, and Windows. Runs on `main` are never
   cancelled. If one failed for an unrelated reason, re-run it.
4. **Tag.** Confirm the tag is unused, then tag the merged commit:
   `git tag vX.Y.Z <sha> && git push origin vX.Y.Z`.
5. **Publish.** Dispatch the workflow **on the tag**, so the workflow, the
   scripts, and the sources all come from the tagged commit:
   `gh workflow run release.yml --ref vX.Y.Z -f publish=true`. Before
   building, it checks that:
   - the run was started (or re-run) by the maintainer (`triggering_actor`);
   - it runs on a `vX.Y.Z` tag that matches the Cargo version;
   - the tagged commit is on `main` and has a successful CI push run.

   It then builds and installs on every target. Before publishing, it checks
   again that the tag still points at the built commit and that no release
   or draft exists for it yet. It creates a draft, downloads the assets to
   compare hashes, and publishes. The release becomes "latest" only if it is
   the highest version, so a backport never downgrades the one-line installer.
6. **Check.** Install with the one-line command from the README on a real
   machine and run `smbs version`.

## Recovery

- Never move or reuse a published tag, and never replace an asset. If a
  release is wrong, ship `X.Y.(Z+1)`.
- If the workflow fails before `publish`, nothing is public. A rerun with the
  same tag builds the tagged commit, using that commit's workflow and
  scripts, so it can only absorb a transient failure. For a fix to source,
  scripts, or the workflow, either ship `X.Y.(Z+1)`, or move the unpublished
  tag to the fixed commit on `main` once that commit's CI push run passes,
  then dispatch again.
- If it fails inside `publish`, a draft release may exist. Delete the draft
  (`gh release delete vX.Y.Z`; the tag stays) and rerun the whole workflow.

## Platform notes

- Linux binaries are built on Ubuntu 22.04. The highest glibc symbol they use
  is `GLIBC_2.34` (observed for 0.2.0), and the build job logs it for every
  release.
- The Windows binary links the C runtime statically (`.cargo/config.toml`),
  so it needs no Visual C++ redistributable. Verification fails if it imports
  `VCRUNTIME140.dll`, and also installs under Windows PowerShell 5.1.
- `rust-toolchain.toml` pins the compiler for CI and releases. Bump it in its
  own PR, so new lints or codegen changes are reviewed rather than inherited.
- Windows and macOS binaries are not code-signed. macOS may quarantine a
  browser-downloaded archive; the curl installer is not affected.
