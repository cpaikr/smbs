# Architecture

`smbs` has one Rust implementation of two read-only smbs.biz operations,
shared by the SDK and the CLI. [README.md](README.md) owns commands and
configuration; [docs/source-contract.md](docs/source-contract.md) owns observed
wire behavior.

## Crates

```text
crates/smbs-cli (binary `smbs`)  ->  crates/smbs (SDK)  ->  http://www.smbs.biz
  argv, JSON envelope, exit status     validation, pacing, transport, parsing
```

- [`crates/smbs`](crates/smbs/src/lib.rs): `Client`, `SeriesRequest`,
  `PacingConfig`, the `Currency` table, and typed `SmbsError`s. Blocking I/O
  only; there is no async runtime and no language binding.
- [`crates/smbs-cli`](crates/smbs-cli/src/main.rs): a clap parser and the
  versioned process contract (`metadata.contractVersion`). It adds no parsing
  of its own.

## Request flow

1. The CLI parses arguments. Every input is validated before pacing state or
   the network is touched: currency against the allowlist, `YYYY-MM` months,
   `from <= to`, `from >= 1990-01`, and the pacing interval.
2. For each distinct currency in order, `Client::fetch`:
   1. acquires the pacing permit, waiting the interval under the lock;
   2. sends `GET <path>?arr_value=<CUR>_<from>_<to>` with no redirects
      (5 s connect, 30 s total, 1 MiB body cap);
   3. requires `200` and `text/xml`, decodes EUC-KR strictly, and releases
      the permit;
   4. parses the chart and checks the caption for the basis, the
      `YYYY.MM` labels, positive plain decimals, strictly ascending months,
      and the requested range.
3. The command stops at the first failure. Its envelope carries the error code
   and the public `sourceUrl`.

## Invariants

- **Fixed origin.** Requests go only to `http://www.smbs.biz`, except a
  loopback origin compiled in with the `fixture-origin` feature for tests.
- **Fail closed on source drift.** Anything the parser does not recognize is
  `source_changed` or `source_parse_failure`, never partial data.
- **Allowlisted currencies.** The source returns an empty chart for unknown
  codes, so only the dated list in
  [`currency.rs`](crates/smbs/src/currency.rs) is accepted.
- **Cross-process pacing.** [`pacing.rs`](crates/smbs/src/pacing.rs) locks
  `request-pacing-v1.lock` in the state directory and holds the lock from
  before the wait until the response is read. The file records the last
  interval, so the longest interval among processes applies to the next
  request. The file is never replaced. An unreadable state is an
  `internal_error`, and requests are never sent unpaced. Waiters are not
  served in order, so waiting more than 120 s for the lock gives up with a
  retryable `internal_error`. Pacing is local to one host. The model follows
  the mytech request-pacing practice and darty. Library callers configure it
  with `PacingConfig::new`; `PacingConfig::from_env` adds the environment
  variables.
- **Errors say whether a retry can help.** A transport failure, timeout, 408,
  or 5xx is `source_unavailable`. A 429, or a 503 with `Retry-After`, is
  `rate_limited`, and a numeric `Retry-After` appears in `recoveryHint`. Both
  are retryable. A redirect or other 4xx is `source_changed` and is not
  retryable. Nothing retries automatically.
