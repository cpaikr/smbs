# smbs.biz source contract

The subset of smbs.biz behavior this project supports. Claims are labeled
**observed** (dated probe), **project decision**, or **unknown**, following
mytech's provider-qualification practice. Observations were made on
2026-11-25.

## Endpoints (observed)

| Basis | Request |
| --- | --- |
| 월평균 매매기준율 | `GET http://www.smbs.biz/ExRate/MonAvgStdExRate_xml.jsp?arr_value={CUR}_{YYYY-MM}_{YYYY-MM}` |
| 월말 매매기준율 | `GET http://www.smbs.biz/ExRate/MonLastStdExRate_xml.jsp?arr_value={CUR}_{YYYY-MM}_{YYYY-MM}` |

- The pages `MonAvgStdExRate.jsp` and `MonLastStdExRate.jsp` load these
  endpoints as FusionCharts data. For USD, the average page instead loads
  `MonAvgStdExRateUSD_xml.jsp`, which formats `1527.30` as `1527.3`. The
  client uses the generic endpoint for every currency.
- No cookies, session, or login are needed.
- HTTPS on the same host fails certificate validation (curl exit 60), so the
  client uses plain HTTP.

## Response (observed)

- `200`, `Content-Type: text/xml; charset=EUC-KR`.
- The body has blank lines before `<?xml version="1.0" encoding="EUC-KR"?>`.
  The client trims them before parsing.
- The root `<chart>` has `caption='월평균 매매기준율'` or `caption='월말 매매기준율'`,
  then one `<set label='YYYY.MM' value='1490.11' />` per month in ascending
  order, followed by `<styles>`.
- One request covers any range. JPY from 1996-01 to 2026-11 returned 370 rows.
- Both bases start at 1990-03. Earlier months in the range are omitted.
- The current, incomplete month is omitted.
- Values are KRW per unit. The dropdown marks JPY, IDR, and VND as `(100)`.

## Failure behavior (observed)

| Input | Response |
| --- | --- |
| Unknown or lowercase currency (`XXX`, `usd`) | 200 XML with no `<set>` rows |
| Reversed range | 200 XML with no rows |
| Malformed `arr_value` | 200 `text/html` error page |
| Range ending in the future | 200, rows up to the last published month |

Because empty charts are ambiguous, the client normalizes codes to uppercase
and rejects codes outside its allowlist (**project decision**).

## Not used

The `*_excel.jsp` export (form POST) requires a server-side session. Without
one it returns "잘못된 접근입니다". With one it returns the same table as HTML
(**observed**).

## Unknown

- Rate limits, terms of reuse, and retention rules. The 1000 ms default
  interval is a **project decision**.
- Whether the currency list or endpoints change without notice.
