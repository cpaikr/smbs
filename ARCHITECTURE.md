# Architecture

`smbs` has one Rust implementation of three read-only smbs.biz operations
(monthly average, month-end, and daily rates),
shared by the SDK and the CLI. [README.md](README.md) owns commands and
configuration; [docs/source-contract.md](docs/source-contract.md) owns observed
wire behavior.

## Crates

```text
crates/smbs-cli (binary `smbs`)  ->  crates/smbs (SDK)  ->  http://www.smbs.biz
  argv, JSON envelope, exit status     validation, pacing, transport, parsing
```

- [`crates/smbs`](crates/smbs/src/lib.rs): `Client`, `SeriesRequest` and
  `DailyRequest`, `PacingConfig`, the `Currency` table, and typed
  `SmbsError`s. [`chart.rs`](crates/smbs/src/chart.rs) holds the one chart
  parser all three operations share. Blocking I/O
  only; there is no async runtime and no language binding.
- [`crates/smbs-cli`](crates/smbs-cli/src/main.rs): a clap parser and the
  versioned process contract (`metadata.contractVersion`). It adds no source
  parsing of its own. [`table.rs`](crates/smbs-cli/src/table.rs) turns
  fetched series into a wide table, and
  [`output.rs`](crates/smbs-cli/src/output.rs) renders it as CSV or XLSX and
  publishes the file. Writing files is a CLI concern, so the SDK never
  touches the filesystem except for the pacing lock.

## Request flow

1. The CLI parses arguments. Every input is validated before pacing state or
   the network is touched: currency against the allowlist, `YYYY-MM` months
   or `YYYY-MM-DD` dates, `from <= to`, `from` no earlier than 1990, daily
   ranges of at most 99 years, and the pacing interval.
2. For each distinct currency in order, `Client::fetch` or
   `Client::fetch_daily`:
   1. acquires the pacing permit, waiting the interval under the lock;
   2. sends `GET <path>?arr_value=<CUR>_<from>_<end>` with no redirects
      (5 s connect, 30 s total, and a body cap of 1 MiB monthly or 4 MiB
      daily). `<end>` is `to`, except for month-end, where it is the month
      after `to` because that endpoint excludes its end month;
   3. requires `200` and an XML media type (`text/xml` or `application/xml`),
      decodes EUC-KR strictly, and releases the permit;
   4. parses the chart. It requires only `<set>` and `<styles>` children,
      the caption for the basis, `YYYY.MM` (or daily `YY.MM.DD`) labels,
      positive plain decimals, strictly ascending periods, and the requested
      range.
3. The command stops at the first failure. Its envelope carries the error
   code. Errors from contacting or parsing the source also carry the public
   `sourceUrl`; validation and pacing-state errors do not.

## Invariants

- **Fixed origin.** Requests go only to `http://www.smbs.biz`, except a
  loopback origin compiled in with the `fixture-origin` feature for tests.
- **Fail closed on source drift.** Anything the parser does not recognize is
  `source_changed` or `source_parse_failure`, never partial data. That
  includes chart elements other than `<set>` and `<styles>`, so rows moved
  into a new container cannot read as an empty series. Only a chart with no
  elements of its own besides `<styles>` is a successful empty series.
- **Allowlisted currencies.** The source returns an empty chart for unknown
  codes, so only the dated list in
  [`currency.rs`](crates/smbs/src/currency.rs) is accepted. `SeriesRequest::new`
  re-resolves every currency against that list, so SDK callers cannot send
  other codes or report a different name or unit.
- **Cross-process pacing.** [`pacing.rs`](crates/smbs/src/pacing.rs) locks
  `request-pacing-v1.lock` in the state directory and holds the lock from
  before the wait until the response is read. The file records the last
  interval, so the longest interval among processes applies to the next
  request. The file is never replaced. An unreadable state is an
  `internal_error`, and requests are never sent unpaced. Waiters are not
  served in order, so waiting more than 120 s for the lock gives up with a
  retryable `internal_error`. Pacing covers only processes that share a state
  directory. The default directory is per user, so other OS users, other
  hosts, and processes with a different `SMBS_STATE_DIR` are not
  coordinated. The model follows the mytech request-pacing practice and
  darty. With mixed intervals, each request waits the larger of its own
  interval and the previous request's interval. A longer interval held by a
  process still waiting for the lock does not slow other processes until it
  gets the lock. Library callers configure pacing with `PacingConfig::new`;
  `PacingConfig::from_env` adds the environment variables.
- **Output files are validated first and published atomically.** The
  extension, parent directory, and existing-entry checks (`symlink_metadata`,
  so a directory, a dangling link, or an unreadable entry is rejected) run
  with the other validation, before any request. The table is written to a
  `tempfile` in the target directory, flushed, then moved into place. Without
  `--overwrite`, the move refuses to replace anything: `MoveFileExW` without
  replace on Windows, and `renameat2(RENAME_NOREPLACE)` or link-then-unlink
  on Unix. So a file another program creates after validation is never
  replaced. A failure after the fetch is the CLI's own `output_failed` code
  (exit 1), and the envelope does not carry the series. The directory is not
  fsynced after the move, so a file written just before a power loss may be
  lost.
- **Errors say whether a retry can help.** A transport failure, timeout, 408,
  or any other 5xx is `source_unavailable`. A 429, or a 503 with `Retry-After`, is
  `rate_limited`, and a numeric `Retry-After` appears in `recoveryHint`. Both
  are retryable. A redirect or other 4xx is `source_changed` and is not
  retryable. Nothing retries automatically.

## Upgrades

[`upgrade.rs`](crates/smbs-cli/src/upgrade.rs) implements `smbs upgrade`, and
[`release.rs`](crates/smbs-cli/src/release.rs) reads release metadata. Both
are CLI-only; the SDK never contacts GitHub.

- **Ownership.** A binary is managed only if `.smbs-receipt.json` beside it
  records the running version, platform key, canonical path, and SHA-256.
  Anything else is `unmanaged_installation`, decided before any network
  request.
- **Metadata.** `releases/latest/download/release-manifest.json` is fetched
  over HTTPS. The OS certificate store is trusted, so TLS-inspecting proxies
  work. No API token is needed, and the endpoint skips drafts and prereleases.
  Versions compare as numeric `MAJOR.MINOR.PATCH`. A missing platform entry is
  reported, never replaced by an older release. A release without a manifest
  (from before 0.3.0) is told apart from "nothing published" by its
  `SHA256SUMS`.
- **Verification.** The archive's SHA-256 must match the manifest. The
  archive must hold exactly the executable and `LICENSE.md`, and the staged
  binary's `--version` must print the expected version within 10 s.
- **Locking and repair.** `--check` is read-only. A real upgrade takes the
  directory's exclusive `.smbs-upgrade.lock` before downloading, and inspects
  the installation again under it. Under the lock, a
  `.smbs-receipt-new-*.json` left by an upgrade interrupted after the binary
  swap is adopted if it fully describes the running binary. Refusals of
  unmanaged binaries still create no files.
- **Redirected writes.** If the staged binary resolves outside the install
  directory, the upgrade stops with `upgrade_unsupported`. This happens when
  a packaged Windows app's shell redirects writes to private storage.
- **Replacement.** Under the lock:
  1. Stage the new binary and receipt in the same directory.
  2. Set the old binary aside. On Windows this is a rename, which Windows
     allows for a running `.exe`. On Unix it is a hard link, or a copy where
     links are unsupported.
  3. Rename the new binary in, then the new receipt.
  4. If a step fails, restore the old binary. If that restore also fails,
     report `upgrade_recovery_required`. The staged receipt is kept so that
     the next `smbs upgrade` can finish the job.

  Leftover `.smbs-new-*`, `.smbs-old-*`, and `.smbs-receipt-new-*` files are
  swept by the next real `smbs upgrade` (under the lock) and by the
  installers. On Windows the renames are not forced to disk (write-through
  would need `unsafe` FFI, which the workspace forbids).
  [`tests/upgrade.rs`](crates/smbs-cli/tests/upgrade.rs) injects failures
  through `SMBS_FIXTURE_FAIL_AT`, available in fixture builds only, to cover
  rollback and repair.

- **Update notices.** [`release_check.rs`](crates/smbs-cli/src/release_check.rs)
  caches the latest-release evidence per user (`release-check-v1.json`) and
  compares it with the running version on every use.
  - **When it refreshes:** an incidental check, after a successful rate
    command, refreshes at most once per 24 h with a 1.5 s network budget, and
    waits 1 h after a failure. The attempt is recorded before the fetch, so a
    crash or an unwritable cache cannot cause a request on every run.
  - **When it reports:** only a newer release, or no successful check for a
    week, becomes an `advisories` entry. `version --check` always refreshes,
    and falls back to stale cached evidence with its age.
  - **When it never runs:** help, `--version`, offline commands, invalid or
    failed commands, and `SMBS_NO_UPDATE_CHECK`.

## Distribution

Releases are built and published by
[`release.yml`](.github/workflows/release.yml) from the target list in
[`release-targets.json`](release-targets.json). The process and its gates
are in [docs/release.md](docs/release.md). `smbs version` reports the build
target, which `crates/smbs-cli/build.rs` records at compile time.
