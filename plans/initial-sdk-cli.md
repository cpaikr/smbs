# Plan: initial smbs Rust SDK and CLI

Status: proposed, awaiting approval (2026-11-25).

## Goal

Fetch 서울외국환중개 (smbs.biz) 월평균 and 월말 매매기준율 series per currency
through a Rust SDK and a standalone `smbs` CLI, shaped like `../darty` and
following `../mytech` guidance. Local build and tests only; no GitHub repo or
release pipeline yet.

## Source evidence (observed 2026-11-25)

- `GET /ExRate/MonAvgStdExRate_xml.jsp?arr_value={CUR}_{YYYY-MM}_{YYYY-MM}` (월평균)
  and `GET /ExRate/MonLastStdExRate_xml.jsp?arr_value=...` (월말). No login,
  no cookies. `text/xml; charset=EUC-KR`, blank lines before the XML
  declaration, rows are `<set label='YYYY.MM' value='1490.11' />`.
- One currency per request; one request covers any range. 월말 starts 1990-03,
  월평균 at least 1990-03. The current incomplete month is omitted.
- An unknown currency, lowercase code, or reversed range returns 200 with zero
  rows, indistinguishable from "no data". A malformed `arr_value` returns an
  HTML error page.
- Excel (`*_excel.jsp`) needs a server-side session and returns the same table
  as HTML. Not used.
- 58 currencies in the page dropdown; JPY, IDR, VND are quoted per 100 units.

## Decisions

| Topic | Decision |
| --- | --- |
| Layout | Cargo workspace: `crates/smbs` (SDK), `crates/smbs-cli` (binary `smbs`). Edition 2024, `unsafe_code = forbid`, clippy pedantic deny (as darty). |
| I/O model | Blocking. No Node binding is planned, so no tokio. HTTP via `ureq` 3 (rustls), EUC-KR via `encoding_rs`, XML via `roxmltree`, CLI via clap derive, `serde`/`serde_json`, `thiserror`. |
| Transport | Fixed origin `http://www.smbs.biz` (the site has no HTTPS for these pages; to verify). No redirects followed. Timeouts: connect 5 s, total 30 s. Body cap 1 MiB. Project User-Agent. |
| Currency validation | Embedded table of the 58 observed codes with Korean name and quote unit (1 or 100). Unknown codes are rejected before any request, because the source silently returns empty for them. |
| Range | `--from YYYY-MM` required, `--to YYYY-MM` defaults to the current month. Reject `from > to` and months before 1990-01. |
| Pacing | darty's model: cross-process OS file lock in a per-user state dir, wait the interval under the lock before each request, largest concurrent interval wins, fail closed if state is unusable. Default **1000 ms** (project decision; no documented limit, small JSP site). Range 0-60000. Precedence: `--request-interval-ms` > `SMBS_REQUEST_INTERVAL_MS` > default. `SMBS_STATE_DIR` overrides the lock location (absolute path). Uses std `File::try_lock` (Rust 1.89+), no `fs4`. |
| Retries | None automatic. Failures are classified (`source_unavailable` retryable; `source_changed`/`source_parse_failure` not). Rate-limit-like statuses (429/503) report `Retry-After` if present. |
| Parsing checks | Decode EUC-KR, trim the preamble, parse XML, require the expected caption (월평균/월말 매매기준율), validate each label/value, ascending unique months within the requested range. Anything else is `source_changed`. |
| Result values | Rate as a JSON number (provider decimals round-trip through f64), plus `unit` (1 or 100) per currency. |

## CLI contract (v1)

```text
smbs monthly-average --currency USD,JPY --from 2025-01 [--to 2025-12] [--request-interval-ms N] [--pretty]
smbs month-end       --currency USD     --from 2025-01 [--to 2025-12] ...
smbs currencies      # offline: embedded code, name, unit list
smbs version
```

- `--currency` takes a comma list or repeats; codes are fetched one by one,
  paced; fail-fast on the first failure.
- stdout: exactly one JSON line per invocation.
  Success: `{"ok":true,"operation":"month-end","result":{"basis":"month_end","from":"2025-01","to":"2025-12","series":[{"currency":"USD","name":"미국 달러","unit":1,"observations":[{"month":"2025-01","rate":1455.79}]}]},"metadata":{"requestIntervalMs":1000,"requestIntervalSource":"default","sourceUrls":[...]},"warnings":[]}`
  Failure: `{"ok":false,"operation":...,"error":{"code","message","retryable","parameter?","recoveryHint?"}}`.
- Exit: 0 success, 1 execution failure, 2 invalid invocation (clap errors also
  become a JSON error envelope).
- Bare `smbs` prints exactly the `--help` text to stdout, exit 0. stderr empty
  in normal operation.
- Validation (currency, range, interval) runs before any state-dir or network
  access. `currencies`, `version`, and help never touch the network.

## Verification

- Unit: label/value parsing, range validation, interval parsing, currency
  table, pacing lock (cooldown survives held/cancelled permits).
- Fixture tests (cargo feature `fixture-origin`, loopback-only origin
  override): a std `TcpListener` server serving fictional EUC-KR XML,
  HTML error page, wrong caption, out-of-range rows, 503 with `Retry-After`,
  oversized body.
- CLI process tests: envelope shape, single newline-terminated stdout, exit
  codes, bare == `--help` byte-identical, invalid input makes no request.
- Live smoke (manual, outside `cargo test`): USD and JPY, both bases, short
  range, default pacing.
- Repo hygiene: `.gitattributes` `* text=auto eol=lf`, `.editorconfig`,
  `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test`.

## Docs

README (purpose, build, commands), ARCHITECTURE (boundaries, pacing, source
contract), `docs/source-contract.md` (observed wire behavior with evidence
labels). This plan closes once those land.

## Out of scope for now

Daily 기간별 매매기준율, CSV/Excel output, Node/Python bindings, GitHub
releases/installers, release/version checking, automatic retries.
