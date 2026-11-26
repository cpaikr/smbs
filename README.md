# smbs

Read-only Rust SDK and CLI for Seoul Money Brokerage Services
([smbs.biz](http://www.smbs.biz)) KRW base rates: 월평균 매매기준율 (monthly
average), 월말 매매기준율 (month-end), and 기간별 매매기준율 (daily).

It reads the public chart XML endpoints behind the site's rate pages. It needs
no login and does not use the Excel export. See the
[source contract](docs/source-contract.md) for observed wire behavior and
[ARCHITECTURE.md](ARCHITECTURE.md) for boundaries and request pacing.

## Install

Prebuilt binaries come from
[GitHub Releases](https://github.com/cpaikr/smbs/releases). They need no Rust
toolchain, and each installer checks the SHA-256 hash of what it downloads.

| Platform | Release target |
| --- | --- |
| Linux x64, glibc 2.34+ (Ubuntu 22.04+, Debian 12+, RHEL 9+) | `linux-x64-gnu` |
| Linux arm64, glibc 2.34+ | `linux-arm64-gnu` |
| macOS, Apple silicon | `darwin-arm64` |
| Windows x64 | `windows-x64-msvc` |

macOS and Linux (needs `curl` or `wget`, `tar`, and `sha256sum` or `shasum`):

```sh
curl -fsSL https://github.com/cpaikr/smbs/releases/latest/download/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://github.com/cpaikr/smbs/releases/latest/download/install.ps1 | iex
```

The binary goes to `~/.local/bin/smbs` or `%LOCALAPPDATA%\smbs\bin\smbs.exe`.
Set `SMBS_INSTALL_DIR` to choose another directory. If that directory is not
on `PATH`, the installer prints how to add it. On macOS and Linux that is an
`export` for the current shell plus the profile files that make it stick. On
Windows it is the command that updates your user `PATH` permanently.

Check the install:

```sh
smbs --version
smbs --help
```

### Upgrade

```sh
smbs version --check   # compare with the latest release; never installs
smbs upgrade --check   # same, for an installer-managed binary
smbs upgrade           # download, verify, and replace this binary
```

`smbs upgrade` only replaces a binary that the installer or a previous
upgrade put in place. Those leave a `.smbs-receipt.json` beside the binary,
recording its version, platform, path, and SHA-256. Binaries built from
source or copied elsewhere are refused; upgrade those the way you installed
them. Before replacing anything, `smbs upgrade` checks the release archive's
SHA-256 against the release manifest and runs the new binary's `--version`.
If the swap fails, the previous binary is restored. If restoring also fails,
the command reports `upgrade_recovery_required` with a `recoveryHint`.
Follow that hint, usually by running `smbs upgrade` again.

- **Concurrent runs:** an upgrade keeps a `.smbs-upgrade.lock` file beside
  the binary, so two upgrades cannot run at once. The lock file stays there;
  it is harmless.
- **Windows leftovers:** the replaced `smbs.exe` cannot be deleted while it
  is running, so it may stay as `.smbs-old-*.exe` until the next upgrade or
  install, which removes it.
- **Interrupted upgrades:** if an upgrade is interrupted after the new binary
  is in place, run `smbs upgrade` again to finish. Rerunning the installer
  always works too.
- **Read-only check:** `smbs upgrade --check` never changes any files.

Installs from before 0.3.0 have no receipt. Once 0.3.0 or later is
published, run the installer once more and `smbs upgrade` works from then on.
If you installed with `SMBS_INSTALL_DIR`, set it to the same directory again,
so the new binary replaces the one on your `PATH`. Rerunning the installer
the same way also always works as an upgrade. To pin a version, replace
`latest/download` with `download/vX.Y.Z` in the install command.

#### Update notices

After a successful rate command (`monthly-average`, `month-end`, or `daily`),
the JSON may include `advisories`, for example
`[{"code":"update_available","message":"smbs 0.4.0 is available ...","releaseUrl":"..."}]`.
It never changes the result or exit status.

- **When it appears:** only in these cases, each with a `code`, plus
  `freshness` and `ageSeconds` describing the evidence:
  - `update_available`: a newer release exists for this platform.
  - `update_incomplete_distribution`: a newer release exists, but without a
    binary for this platform.
  - `update_check_problem`: the release information or the cache cannot be
    used, for example a release format this version does not understand or
    an unwritable cache directory. Shown at once.
  - `update_check_unavailable`: no check has succeeded for a week. On a
    machine that is often offline this stays quiet for the first week;
    `smbs version --check` always reports the details.
- **Cost:** the latest-release check is cached for 24 hours. A refresh never
  delays a command by more than 1.5 s; a check that takes longer finishes or
  is dropped in the background. After an unsuccessful attempt it waits an
  hour (a day for persistent problems) before trying again.
- **Turning it off:** set `SMBS_NO_UPDATE_CHECK` to any non-empty value other
  than `0`. `smbs version --check` and `smbs upgrade` still check when asked.
- **Cache location:** `%LOCALAPPDATA%\smbs\cache`, `~/Library/Caches/smbs`,
  or `$XDG_CACHE_HOME/smbs` (`~/.cache/smbs`). Set `SMBS_CACHE_DIR` to an
  absolute directory to move it.

### Build from source

Requires Rust 1.89 or later. `rust-toolchain.toml` pins the toolchain that CI
and releases use.

```sh
cargo build --release -p smbs-cli
```

This builds `target/release/smbs` (`smbs.exe` on Windows). The examples
below call `smbs`, so either put `target/release` on `PATH` or call the
binary by its path.

## Commands

```sh
smbs monthly-average --currency USD --from 2025-01 --to 2025-12
smbs month-end --currency USD,JPY --from 2024-01      # --to defaults to the current month in Korea
smbs daily --currency USD --from 2025-01-01 --to 2025-12-31   # business days; --to defaults to today in Korea
smbs currencies                                       # offline: codes, names, quote units
smbs version
smbs --help                                           # same as bare `smbs`
```

Each command prints one JSON line to stdout:

```json
{"ok":true,"operation":"month-end","result":{"basis":"month_end","from":"2025-01","to":"2025-01","series":[{"basis":"month_end","currency":"JPY","name":"일본 엔","unit":100,"from":"2025-01","to":"2025-01","observations":[{"month":"2025-01","rate":927.88}],"sourceUrl":"http://www.smbs.biz/..."}]},"metadata":{"contractVersion":"1","requestIntervalMs":1000,"requestIntervalSource":"default"},"warnings":[]}
```

`rate` is KRW per `unit` units of the currency. Daily results have basis
`daily`, and their observations carry `date` instead of `month`. The current month, and today until it is published,
have no row. Failures print
`{"ok":false,"operation":...,"error":{"code","message","retryable",...}}`.
Exit status is 0 for success, 1 for an execution failure, and 2 for an invalid
invocation. stderr stays empty, except that `smbs upgrade` prints progress
lines there when stderr is a terminal.

## CSV and Excel files

Add `--output <path>` to any rate command to also write a table. The file
extension picks the format.

```sh
smbs month-end --currency USD,JPY,EUR --from 2025-01 --to 2025-12 --output month-end-2025.xlsx
smbs daily --currency USD,JPY --from 2025-03-01 --to 2025-03-31 --output march.csv
```

- **Layout:** one row per month or date, one column per currency. Headers show
  the quote unit, for example `JPY (100)`. A cell is empty where a currency
  has no rate.
- **`.xlsx`:** rates are numbers and periods are real Excel dates (formatted
  `yyyy-mm` or `yyyy-mm-dd`). The sheet is named after the command, the header
  row is frozen, and a source line sits under the table.
- **`.csv`:** UTF-8 with a BOM so Excel shows Korean text correctly, and CRLF
  line endings.
- **Existing files** are never replaced unless you pass `--overwrite`.
  `--overwrite` swaps in a new file: a symlink at the path is replaced rather
  than followed, and the old file's permissions are not kept. A wrong
  extension, a missing directory, a directory at the path, or an existing
  file without `--overwrite` is reported (exit 2) before any request is sent.
- **Atomic writes:** the file is written under a temporary `.smbs-*.tmp` name
  in the same directory and then moved into place, so a failed run never
  leaves a partial file. Only a killed process can leave a `.smbs-*.tmp`
  behind.
- **Write failures:** if writing fails after the fetch, the error code is
  `output_failed` (exit 1) and the fetched series are not returned. On
  Windows, check that the file isn't open in Excel.

The JSON on stdout still carries the series, plus
`result.artifact = {path, format, bytes, sha256, rows}`.

## Request pacing

`smbs` processes that share a state directory wait 1000 ms before each
request, coordinated through one lock file, so parallel scripts cannot burst
the site. The default directory is per user. Processes run by other OS users,
on other hosts, or with a different `SMBS_STATE_DIR` are not coordinated.

| Setting | Effect |
| --- | --- |
| `--request-interval-ms <0-60000>` | Interval for one invocation |
| `SMBS_REQUEST_INTERVAL_MS` | Interval when the flag is absent |
| `SMBS_STATE_DIR` | Absolute directory for the lock file (default `%LOCALAPPDATA%\smbs`, `$XDG_STATE_HOME/smbs`, or `~/.local/state/smbs`) |

`0` disables pacing. Use it only against a local fixture.

## Agent skill

The consumer skill source is in [`skill/smbs`](skill/smbs/SKILL.md). It guides
agents to the right command for a rate request (monthly average, month-end,
or daily), keeps quote units and missing periods straight, and loads
installation guidance only when the executable is needed.

Copy the complete `skill/smbs` directory into a skill location supported by
your agent, including `references/` and `agents/`. The installed skill can apply
implicitly to smbs.biz base-rate requests or be invoked explicitly with `$smbs`.

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features
cargo test --workspace --all-features   # includes CLI process tests on a loopback fixture
```

Tests never contact smbs.biz. The `fixture-origin` feature lets the CLI read
`SMBS_FIXTURE_ORIGIN` (loopback HTTP only). Release builds leave it off.
