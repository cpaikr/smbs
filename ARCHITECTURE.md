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
  versioned process contract (`metadata.contractVersion`). It adds no parsing
  of its own.

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
- **Errors say whether a retry can help.** A transport failure, timeout, 408,
  or any other 5xx is `source_unavailable`. A 429, or a 503 with `Retry-After`, is
  `rate_limited`, and a numeric `Retry-After` appears in `recoveryHint`. Both
  are retryable. A redirect or other 4xx is `source_changed` and is not
  retryable. Nothing retries automatically.
