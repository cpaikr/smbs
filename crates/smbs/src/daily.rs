use serde::Serialize;

use crate::{Currency, Date, SmbsError};

pub(crate) const DAILY_PATH: &str = "/ExRate/StdExRate_xml.jsp";
const DAILY_CAPTION: &str = "기간별 매매기준율";
/// Labels carry a two-digit year, so a range must fit in one century window.
const MAX_SPAN_YEARS: u16 = 99;

/// A validated request for one currency's daily 매매기준율 (기간별 매매기준율).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DailyRequest {
    currency: &'static Currency,
    from: Date,
    to: Date,
}

impl DailyRequest {
    /// The currency is re-resolved against [`crate::CURRENCIES`].
    ///
    /// # Errors
    /// `invalid_request` when the currency is not listed, `from` is after `to`
    /// or before [`Date::EARLIEST`], or the range spans more than 99 years.
    pub fn new(currency: &Currency, from: Date, to: Date) -> Result<Self, SmbsError> {
        let currency = Currency::parse(currency.code)?;
        if from < Date::EARLIEST {
            return Err(SmbsError::invalid(
                format!(
                    "from must be {} or later; smbs.biz has no earlier daily rates.",
                    Date::EARLIEST
                ),
                "from",
                "The oldest observed row is 1990-03-02.",
            ));
        }
        if from > to {
            return Err(SmbsError::invalid(
                format!("from ({from}) is after to ({to})."),
                "from",
                "Swap the dates or widen the range.",
            ));
        }
        if to.year() - from.year() > MAX_SPAN_YEARS {
            return Err(SmbsError::invalid(
                format!("A daily range may span at most {MAX_SPAN_YEARS} years."),
                "to",
                "Split the range into shorter requests.",
            ));
        }
        Ok(Self { currency, from, to })
    }

    pub const fn currency(&self) -> &'static Currency {
        self.currency
    }

    pub const fn from(&self) -> Date {
        self.from
    }

    pub const fn to(&self) -> Date {
        self.to
    }

    /// `USD_2025-01-01_2025-12-31`; both ends are inclusive at the source
    /// (observed 2026-11-25).
    pub(crate) fn arr_value(&self) -> String {
        format!("{}_{}_{}", self.currency.code, self.from, self.to)
    }
}

/// One business day's KRW rate for `unit` units of the series currency.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct DailyObservation {
    pub date: Date,
    pub rate: f64,
}

/// A fetched daily series. Days without a published rate (weekends,
/// holidays) are absent rather than null.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailySeries {
    pub currency: &'static str,
    pub name: &'static str,
    pub unit: u16,
    pub from: Date,
    pub to: Date,
    pub observations: Vec<DailyObservation>,
    pub source_url: String,
}

pub(crate) fn parse_daily_chart(
    text: &str,
    request: &DailyRequest,
    source_url: &str,
) -> Result<Vec<DailyObservation>, SmbsError> {
    let (from, to) = (request.from, request.to);
    let rows = crate::chart::parse_rows(text, DAILY_CAPTION, source_url, from, to, |label| {
        Date::parse_source_label(label, from, to)
    })?;
    Ok(rows
        .into_iter()
        .map(|(date, rate)| DailyObservation { date, rate })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;

    const URL: &str =
        "http://www.smbs.biz/ExRate/StdExRate_xml.jsp?arr_value=USD_1999-12-30_2000-01-04";

    fn date(text: &str) -> Date {
        Date::parse(text, "date").unwrap()
    }

    fn request(from: &str, to: &str) -> DailyRequest {
        DailyRequest::new(Currency::parse("USD").unwrap(), date(from), date(to)).unwrap()
    }

    fn chart(caption: &str, rows: &str) -> String {
        format!(
            "\r\n\r\n<?xml version=\"1.0\" encoding=\"EUC-KR\"?>\r\n<chart caption='{caption}' yAxisName='Rate (Won)'>\r\n{rows}\r\n<styles><definition /></styles>\r\n<styles><application /></styles>\r\n</chart>\r\n"
        )
    }

    #[test]
    fn parses_fictional_rows_across_a_century_boundary() {
        let rows = "<set color='c93749' label='99.12.30' value='1000.5' />\r\n<set color='c93749' label='00.01.04' value='1001' />";
        let observations = parse_daily_chart(
            &chart("기간별 매매기준율", rows),
            &request("1999-12-30", "2000-01-04"),
            URL,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_string(&observations).unwrap(),
            r#"[{"date":"1999-12-30","rate":1000.5},{"date":"2000-01-04","rate":1001.0}]"#
        );
    }

    #[test]
    fn unexpected_rows_are_source_changed() {
        let req = request("2025-03-01", "2025-03-31");
        for (caption, rows) in [
            ("월평균 매매기준율", ""),
            ("기간별 매매기준율", "<set label='25.02.28' value='1' />"),
            ("기간별 매매기준율", "<set label='2025.03.03' value='1' />"),
            (
                "기간별 매매기준율",
                "<set label='25.03.04' value='1' /><set label='25.03.03' value='1' />",
            ),
            ("기간별 매매기준율", "<set label='25.03.03' value='' />"),
        ] {
            let error = parse_daily_chart(&chart(caption, rows), &req, URL).unwrap_err();
            assert_eq!(error.code, ErrorCode::SourceChanged, "{caption} {rows}");
        }
    }

    #[test]
    fn request_validation_and_arr_value() {
        assert_eq!(
            request("2025-01-01", "2025-12-31").arr_value(),
            "USD_2025-01-01_2025-12-31"
        );
        let usd = Currency::parse("USD").unwrap();
        let invalid = |from, to| {
            DailyRequest::new(usd, date(from), date(to))
                .unwrap_err()
                .parameter
        };
        assert_eq!(invalid("2025-02-01", "2025-01-31").as_deref(), Some("from"));
        assert_eq!(invalid("1989-12-31", "1990-03-02").as_deref(), Some("from"));
        assert_eq!(invalid("1990-01-01", "2090-01-01").as_deref(), Some("to"));
        assert!(DailyRequest::new(usd, date("1990-01-01"), date("2089-12-31")).is_ok());
    }
}
