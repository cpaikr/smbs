# Plan: daily rates, CSV/Excel output, release binaries

Status: proposed, awaiting approval (2026-11-25).

Three PRs, in this order, then a `v0.2.0` release. Each PR updates README,
ARCHITECTURE, and the source contract for what it changes.

## PR A: daily rates (`smbs daily`)

Source (observed 2026-11-25), same shape as the monthly endpoints:

- `GET /ExRate/StdExRate_xml.jsp?arr_value={CUR}_{YYYY-MM-DD}_{YYYY-MM-DD}`,
  `text/xml; charset=EUC-KR`, caption `기간별 매매기준율`, no login.
- Rows `<set color='c93749' label='YY.MM.DD' value='1337.2' />`, business
  days only, ascending. **Both ends inclusive** (unlike month-end).
- One request covers long ranges: USD 2020-01-01 to 2025-12-31 is 1,479 rows
  (90 KB). Data starts 1990-03-02. CNY has no rows after 2016 (the site
  stopped publishing it).
- The label year has two digits. The client picks the century that places the
  date inside the requested range, which is unambiguous for 1990-2089, and
  rejects anything else as `source_changed`.

Design:

- `smbs daily --currency USD,JPY --from 2025-01-01 [--to 2025-12-31]`.
  `--to` defaults to today in Korea. Observations are `{date, rate}`.
- New `Date` type (`YYYY-MM-DD`, validated calendar day). `SeriesRequest`
  becomes generic over the period: `Basis::Daily` uses dates, while the
  monthly bases keep `YearMonth`. Same allowlist, pacing, transport, and
  error codes.
- The body cap rises to 4 MiB for daily (36 years of JPY is about 600 KB).
- Same tests as the monthly path: parser fixtures, CLI process tests, and a
  live smoke test.

## PR B: CSV and Excel files (`--output`)

- `--output <path>` on `monthly-average`, `month-end`, and `daily` writes a
  file whose format comes from the extension: `.csv` or `.xlsx`. Anything
  else is `invalid_request`. stdout still carries the one JSON envelope,
  with `result.artifact = {path, format, bytes, sha256, rows}` beside the
  series, so automation can verify the file.
- **No-clobber** by default, with `--overwrite` to replace. Writes go to a
  temporary file in the target directory, then rename, so a failed run never
  leaves a partial file.
- **Layout: wide**, one row per period and one column per currency, like a
  rate table in a workbook. Headers are `month` or `date`, then `USD`,
  `JPY (100)` and so on, with the unit in the header. A missing rate is an
  empty cell.
- CSV: UTF-8 **with BOM** so Excel shows Korean names correctly, RFC 4180
  quoting, and periods written as `YYYY-MM` or `YYYY-MM-DD` text.
- XLSX via `rust_xlsxwriter` (pure Rust, no C dependencies). Rates are
  numeric cells. Periods are real Excel dates, formatted `yyyy-mm` or
  `yyyy-mm-dd`, so lookups and sorting work. One sheet named after the basis,
  with a frozen header row and a source line under the table.
- Tests: byte-level CSV checks, and XLSX checks that open the archive and
  read the sheet XML back. Cover no-clobber, atomic replacement, and an
  invalid extension before any network access.

## PR C: CI, release binaries, installers

Following mytech's standalone-distribution guidance, using ytm's native-runner
style. GitHub-hosted runners are free once the repo is public.

- `release-targets.json` is the single source for targets and asset names:
  `linux-x64-gnu`, `linux-arm64-gnu`, `darwin-arm64`, `windows-x64-msvc`.
- `ci.yml`: on PRs, fmt, clippy, and test (`--all-features`) on Linux. On
  pushes to `main` and on manual dispatch, the same on macOS and Windows,
  because pacing locks differ by OS.
- `release.yml`: manual `workflow_dispatch` on a `vX.Y.Z` tag, gated to
  the maintainer account. Steps:
  1. Check that the tag matches the workspace `Cargo.toml` version and that
     CI passed for that commit.
  2. Build on native runners.
  3. Package `smbs-<version>-<target>.tar.gz` (`.zip` on Windows), each with
     the binary and `LICENSE.md`.
  4. Produce `SHA256SUMS`.
  5. On each target's runner, run the generated installer and the installed
     binary's `--version`, `--help`, and `currencies`.
  6. Create a draft release, upload the assets, verify them, and publish.
     Published assets are never replaced; a fix ships as a new version.
- Installers `install.sh` and `install.ps1` are self-downloading with the
  SHA-256 hashes embedded, so one line installs the latest release:
  `curl -fsSL https://github.com/cpaikr/smbs/releases/latest/download/install.sh | sh`,
  and an `irm ... | iex` equivalent on Windows. Defaults are `~/.local/bin`
  and `%LOCALAPPDATA%\smbs\bin`, overridable with `SMBS_INSTALL_DIR`.
  Each installer verifies the checksum, runs the staged binary, and renames
  it into place atomically.
- `smbs version` gains the build target. **Deferred:** `smbs upgrade`,
  update advisories, and install receipts. Upgrading means rerunning the
  installer, as mytech recommends for a small tool.
- Docs: a README install section in mytech's order, and `docs/release.md`
  with the release runbook and recovery rules.

## Release

Bump the workspace version to `0.2.0` in PR C. After it merges and CI passes
on `main`, push tag `v0.2.0`, dispatch the release, and run the one-line
install on this Windows machine as the final check.

## Out of scope

`smbs upgrade` and self-update, crates.io publishing, signing and provenance
attestations, Node and Python bindings, and other smbs.biz pages.
