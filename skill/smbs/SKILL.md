---
name: smbs
description: "Retrieve Korean won base exchange rates (매매기준율) published by Seoul Money Brokerage Services (smbs.biz) with the smbs CLI: monthly average (월평균), month-end (월말), and daily (기간별) rates per currency, as JSON or CSV/Excel tables. Use when a request needs official 서울외국환중개 매매기준율 for a currency and period, such as month-end rates for foreign-currency translation or average rates for a fiscal year. Excludes other rate providers (bank TT rates, BOK ECOS), forecasts, currency conversion advice, and smbs development."
---

# smbs

Turn a rate request into bounded `smbs` queries and report the returned rates
with their units and provenance. smbs reads public smbs.biz pages and needs no
login or API key. Live retrieval needs network access to `www.smbs.biz`.

## Start with the available executable

Reuse a successful check from this execution context. Otherwise run
`smbs --help` once; recheck after an executable or environment change or a
relevant failure. A JSON `advisories[]` entry (an update notice) is not rate
data and does not request an upgrade.

- If `smbs` is missing, unusable, or setup or upgrade is requested, read
  [installation](references/installation.md). A source error or an
  unsupported option alone is not an installation failure.
- For rate retrieval, read [usage](references/usage.md), then the chosen
  command's `--help` for exact options.

## Choose the route

| User needs | Command |
| --- | --- |
| Average rate for each month (e.g. income statement translation) | `smbs monthly-average --currency USD --from 2025-01 --to 2025-12` |
| Rate at each month's end (e.g. balance sheet translation) | `smbs month-end --currency USD,JPY --from 2025-01 --to 2025-12` |
| Rates for specific dates or a date range | `smbs daily --currency EUR --from 2025-03-01 --to 2025-03-31` |
| Which currencies exist, and their quote units | `smbs currencies` |
| A spreadsheet for the user | add `--output rates.xlsx` (or `.csv`) |

Use the explicit period the user asked for. Months are `YYYY-MM`; dates are
`YYYY-MM-DD`. `--to` defaults to the current month or day in Korea.

## Rules that change the answer

- `rate` is KRW per `unit` units of the currency. JPY, IDR, and VND are
  quoted per **100** units; divide by 100 for a per-unit rate and say so.
- Only published periods have rows. The current month has no monthly row
  until it ends; weekends and holidays have no daily rows. A missing row is
  not zero. For "the rate on a date" that has no row, say so and offer the
  previous business day only if the user accepts that substitution.
- The month-end rate is the last business day's rate, so it equals that
  day's `daily` rate.
- An empty series with a warning means smbs.biz published nothing for that
  currency and period (for example, CNY after 2016), not a failed query.

## Interpret the process and finish

Stdout is one JSON document. Exit 0 is success. Exit 1 is an execution
failure; inspect `error.code`, `retryable`, `sourceUrl`, and `recoveryHint`.
Exit 2 is invalid input; fix it using `--help` instead of retrying. Retry a
retryable source failure once. Treat `source_changed` and
`source_parse_failure` as uncertainty, never as "no data". Do not
substitute another provider or period to manufacture an answer.

Finish with the requested rates or file, stating for each currency the basis
(monthly average, month-end, or daily), the period, the unit, and the
returned `sourceUrl`. For an `--output` file, report the `artifact` path. Note
any warnings, missing periods, or substituted dates.
