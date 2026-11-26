# Plan: self-upgrade and update checks

Status: approved 2026-11-26. PR 1 merged (#9); PR 2 in review; then v0.3.0.

Add `smbs upgrade`, `smbs version --check`, and cached update notices,
following darty's design (`../darty/crates/darty-cli/src/upgrade.rs`,
`release_check.rs`) and mytech's
[standalone distribution](../../mytech/practices/standalone-cli-distribution.md)
and [version checking](../../mytech/practices/cli-version-checking.md)
guidance. Two PRs, then release `v0.3.0`.

## PR 1: receipts, release manifest, `upgrade`, `version --check`

**Release side**

- `assemble.sh` also writes `release-manifest.json`:
  `{schemaVersion, version, tag, targets: {<key>: {asset, sha256}}}`,
  generated from `release-targets.json`, and covered by `SHA256SUMS`.
- Both installers write a receipt next to the binary,
  `.smbs-receipt.json`:
  `{schemaVersion: 1, version, target, executable, sha256}`. `executable`
  is the resolved absolute path, and `sha256` is the hash of the installed
  binary. The binary is swapped in first, then the receipt.
- The verify job checks the receipt on every target.

**`smbs upgrade [--check]`** (CLI only; the SDK is untouched)

1. **Ownership.** Upgrade only manages a binary that the installer or a
   previous upgrade put there, so it never replaces a binary from
   `cargo build`, a package manager, or a copied file. The receipt must
   exist and match the running binary: same version and target, same
   canonical path, same SHA-256. Otherwise it fails with
   `unmanaged_installation`, before any network request.
2. **Latest release.** `GET https://github.com/cpaikr/smbs/releases/latest/download/release-manifest.json`.
   This is not the REST API, so there is no rate limit or token, and drafts
   and prereleases are excluded. Limits are a 15 s timeout and a 64 KiB cap.
   Versions are compared as `MAJOR.MINOR.PATCH`. A release that lacks this
   platform's asset is reported, not skipped.
3. **`--check`** stops here and reports `runningVersion`, `latestVersion`,
   `updateAvailable`, and `releaseUrl`.
4. **Download and verify.** Download the archive (300 s timeout, 64 MiB cap)
   and check its SHA-256 against the manifest. Extract it; the archive must
   contain exactly `smbs[.exe]` and `LICENSE.md`. Run the staged binary's
   `--version`, which must print the expected version within 10 s.
5. **Replace and recover.**
   1. Take an exclusive `.smbs-upgrade.lock`.
   2. Write the staged binary and receipt into the same directory.
   3. Move the running binary aside to `.smbs-old-<pid>`. On Windows this is
      a rename, which Windows allows for a running `.exe` on the same volume.
      On Unix it is a hard link followed by an atomic rename.
   4. Move the new binary in, then the new receipt.
   5. If any step fails, restore the old binary and receipt. If the restore
      also fails, report `upgrade_recovery_required` with the exact manual
      steps.
   6. Leftover `.smbs-old-*` and `.smbs-new-*` files are swept on the next
      run. On Windows the old image stays locked until the process exits.
6. **Output.** One JSON envelope, as in other commands. Error codes:
   `unmanaged_installation`, `upgrade_source_unavailable` (retryable),
   `upgrade_release_invalid`, `upgrade_verification_failed`,
   `upgrade_failed`, and `upgrade_recovery_required`. Progress lines such as
   `Downloading smbs 0.3.1 (1.5 MB)...` go to stderr only when stderr is a
   terminal; this is the documented exception to the empty-stderr rule.

**`smbs version --check`** reports `comparison` (`newer`, `equal`, `ahead`,
`no_stable_release`, or `unavailable`), `latestVersion`, `releaseUrl`, and
`freshness`. It always exits 0; problems are reported, not raised. Plain
`smbs version` stays offline.

**Transport.** The CLI's GitHub client is HTTPS-only, so `ureq` gains
`rustls`. smbs.biz stays on plain HTTP. Archive reading uses `flate2` and
`tar` for `.tar.gz`, and the existing `zip` crate for Windows.

**Tests** (offline). The `fixture-origin` feature also accepts
`SMBS_FIXTURE_RELEASE_ORIGIN` and `SMBS_FIXTURE_RUNNING_VERSION`. A loopback
server serves a manifest and archives built from the test binary itself.
Cases:

- check, then upgrade;
- a checksum mismatch;
- a candidate whose `--version` is wrong;
- no or mismatched receipt (no request is made);
- a release missing this platform;
- an interrupted replacement that is rolled back;
- leftover cleanup.

## PR 2: cached update notices

- After a successful rate command, add `advisories: [{code, message}]` to the
  JSON envelope, but only when a newer release exists or the evidence is
  stale or failing. Nothing appears when the binary is up to date.
- **Cache.** The cache is `release-check-v1.json` in the per-user cache
  directory. `SMBS_CACHE_DIR` overrides the location and must be absolute.
  - Entries live 24 h.
  - After a failure, the next attempt waits 1 h.
  - One refresh may take at most 1.5 s.
  - Each attempt is recorded before the fetch, so a crash or an unwritable
    cache cannot cause a request on every run.
- **Opt-out.** `SMBS_NO_UPDATE_CHECK=1` turns the notices off. It does not
  affect an explicit `upgrade` or `version --check`.
- No checks run on help, `--version`, offline commands, invalid input, or
  failed commands. A notice never changes a command's exit status.

## Release

Bump to `0.3.0`, release, then on this machine:

1. reinstall once with the one-line installer;
2. check that `smbs upgrade --check` reports `equal`;
3. check that `smbs upgrade` on an unmanaged copy refuses.

**Existing 0.2.0 installs have no receipt.** They must rerun the installer
once, and every later version can use `smbs upgrade`. A real upgrade between
two published versions is first exercised when `v0.3.1` ships; until then the
fixture tests cover the mechanics.

## Out of scope

- Upgrading to a specific older version (`--version X`).
- Signatures or provenance attestations.
- Prerelease channels.
- Adopting installs that have no receipt.
