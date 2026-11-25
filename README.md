# smbs

Read-only Rust SDK and CLI for Seoul Money Brokerage Services
([smbs.biz](http://www.smbs.biz)) monthly KRW base rates:
월평균 매매기준율 (monthly average) and 월말 매매기준율 (month-end).

It reads the public chart XML endpoints behind the site's rate pages. It needs
no login and does not use the Excel export. See the
[source contract](docs/source-contract.md) for observed wire behavior and
[ARCHITECTURE.md](ARCHITECTURE.md) for boundaries and request pacing.

## Build

Requires Rust 1.89 or later.

```sh
cargo build --release -p smbs-cli   # target/release/smbs
```

## Commands

```sh
smbs monthly-average --currency USD --from 2025-01 --to 2025-12
smbs month-end --currency USD,JPY --from 2024-01      # --to defaults to the current month in Korea
smbs currencies                                       # offline: codes, names, quote units
smbs version
smbs --help                                           # same as bare `smbs`
```

Each command prints one JSON line to stdout:

```json
{"ok":true,"operation":"month-end","result":{"basis":"month_end","from":"2025-01","to":"2025-02","series":[{"basis":"month_end","currency":"JPY","name":"일본 엔","unit":100,"from":"2025-01","to":"2025-02","observations":[{"month":"2025-01","rate":927.88}],"sourceUrl":"http://www.smbs.biz/..."}]},"metadata":{"contractVersion":"1","requestIntervalMs":1000,"requestIntervalSource":"default"},"warnings":[]}
```

`rate` is KRW per `unit` units of the currency. The current month has no row
until smbs.biz publishes it. Failures print
`{"ok":false,"operation":...,"error":{"code","message","retryable",...}}`.
Exit status is 0 for success, 1 for an execution failure, and 2 for an invalid
invocation. stderr stays empty.

## Request pacing

Every `smbs` process on a machine waits 1000 ms before each request,
coordinated through a shared lock file, so parallel scripts cannot burst the
site.

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
