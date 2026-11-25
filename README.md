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
| Linux x64, glibc 2.35+ (Ubuntu 22.04+, Debian 12+) | `linux-x64-gnu` |
| Linux arm64, glibc 2.35+ | `linux-arm64-gnu` |
| macOS, Apple silicon | `darwin-arm64` |
| Windows x64 | `windows-x64-msvc` |

macOS and Linux:

```sh
curl -fsSL https://github.com/cpaikr/smbs/releases/latest/download/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://github.com/cpaikr/smbs/releases/latest/download/install.ps1 | iex
```

The binary goes to `~/.local/bin/smbs` or `%LOCALAPPDATA%\smbs\bin\smbs.exe`.
Set `SMBS_INSTALL_DIR` to choose another directory. If that directory is not
on `PATH`, the installer prints the command that adds it.

Check the install:

```sh
smbs --version
smbs --help
```

To upgrade, run the installer again. To pin a version, replace
`latest/download` with `download/vX.Y.Z`. There is no self-update command.

### Build from source

Requires Rust 1.89 or later.

```sh
cargo build --release -p smbs-cli
```

This builds `target/release/smbs` (`smbs.exe` on Windows).

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
invocation. stderr stays empty.

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

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features
cargo test --workspace --all-features   # includes CLI process tests on a loopback fixture
```

Tests never contact smbs.biz. The `fixture-origin` feature lets the CLI read
`SMBS_FIXTURE_ORIGIN` (loopback HTTP only). Release builds leave it off.
