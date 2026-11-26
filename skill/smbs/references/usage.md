# Retrieve and report rates

Use `smbs <command> --help` for the options the installed executable supports.
This workflow targets CLI contract version 1 (`metadata.contractVersion`).

## Pick the basis the task needs

- **Monthly average (월평균 매매기준율):** the average of a month's daily base
  rates. Typical use: translating income and expenses at average rates.
- **Month-end (월말 매매기준율):** the rate on the month's last business day.
  Typical use: translating balances at the reporting date. For a reporting
  date that is not a month end, use `daily` for that date.
- **Daily (기간별 매매기준율):** one row per business day.

If the user names a basis in Korean or by purpose, map it as above. If the
basis is ambiguous and it changes the answer, ask, or state which one you used.

## Currencies and units

`--currency` takes ISO codes, comma-separated or repeated (`USD,JPY` or
`--currency USD --currency JPY`); case does not matter. Unknown codes are
rejected before any request (exit 2, `parameter: "currency"`). `smbs currencies`
lists all 58 codes with Korean names and `unit`.

Each series reports `unit`. A `unit` of 100 means the rate is KRW per 100
units. Report it as given, and convert only when asked, saying that you did.

## Periods

- Monthly commands: `--from YYYY-MM`, optional `--to YYYY-MM` (inclusive).
  Data starts 1990-03.
- `daily`: `--from YYYY-MM-DD`, optional `--to YYYY-MM-DD` (inclusive), at
  most 99 years. Data starts 1990-03-02.
- One request per currency covers the whole range, and requests are paced
  1 s apart, so many currencies take a few seconds. Do not split ranges
  without a reason.

## Reading results

`result.series[]` holds one entry per currency, in the order requested:
`currency`, `name`, `unit`, `from`, `to`, `observations[]` with
`{month, rate}` or `{date, rate}`, and `sourceUrl`. Months or days without a
published rate are absent. `warnings[]` names currencies that returned no
rows.

For a single requested month or date, read that observation. If it is absent,
report that smbs.biz has no rate for it yet (the current month, a holiday, or
before publication) rather than inventing one.

## Files for the user

`--output path.xlsx` or `path.csv` also writes a table: one row per period,
one column per currency, with headers like `JPY (100)`. XLSX stores real dates
and numbers; CSV is UTF-8 with a BOM for Excel. An existing file is not
replaced unless `--overwrite` is given (exit 2 otherwise, before any request).
The envelope's `result.artifact` gives `path`, `bytes`, `sha256`, and `rows`.
A write failure after fetching is `output_failed`.

## Failures

| `error.code` | Meaning | Action |
| --- | --- | --- |
| `invalid_request` (exit 2) | Bad currency, period, path, or option | Fix the input with `--help` |
| `source_unavailable` | Network failure, timeout, or 5xx | Retry once; then report the blocked request |
| `rate_limited` | smbs.biz asked clients to slow down | Wait per `recoveryHint`; do not retry immediately |
| `source_changed`, `source_parse_failure` | smbs.biz answered in an unexpected shape | Report it; do not treat it as "no data" |
| `output_failed` | The `--output` file could not be written | Fix the path (on Windows, close the file in Excel) and rerun |

Keep request pacing enabled. `--request-interval-ms 0` is for local test
fixtures only.
