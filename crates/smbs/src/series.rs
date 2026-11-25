use serde::Serialize;

use crate::{Currency, ErrorCode, SmbsError, YearMonth};

/// Which monthly 매매기준율 series to fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    /// 월평균 매매기준율: the month's average base rate.
    MonthlyAverage,
    /// 월말 매매기준율: the base rate on the month's last business day.
    MonthEnd,
}

impl Basis {
    pub(crate) const fn path(self) -> &'static str {
        match self {
            Self::MonthlyAverage => "/ExRate/MonAvgStdExRate_xml.jsp",
            Self::MonthEnd => "/ExRate/MonLastStdExRate_xml.jsp",
        }
    }

    /// Chart caption the source sends for this basis; any other caption means
    /// the endpoint no longer serves the series this client expects.
    const fn caption(self) -> &'static str {
        match self {
            Self::MonthlyAverage => "월평균 매매기준율",
            Self::MonthEnd => "월말 매매기준율",
        }
    }
}

/// A validated request for one currency's monthly series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeriesRequest {
    basis: Basis,
    currency: &'static Currency,
    from: YearMonth,
    to: YearMonth,
}

impl SeriesRequest {
    /// The currency is re-resolved against [`crate::CURRENCIES`], so a
    /// hand-built [`Currency`] cannot send an unlisted code or report a wrong
    /// name or unit.
    ///
    /// # Errors
    /// `invalid_request` when the currency is not listed, or `from` is after
    /// `to` or before [`YearMonth::EARLIEST`].
    pub fn new(
        basis: Basis,
        currency: &Currency,
        from: YearMonth,
        to: YearMonth,
    ) -> Result<Self, SmbsError> {
        let currency = Currency::parse(currency.code)?;
        if from < YearMonth::EARLIEST {
            return Err(SmbsError::invalid(
                format!(
                    "from must be {} or later; smbs.biz has no earlier monthly rates.",
                    YearMonth::EARLIEST
                ),
                "from",
                "The oldest observed rows are 1990-03.",
            ));
        }
        if from > to {
            return Err(SmbsError::invalid(
                format!("from ({from}) is after to ({to})."),
                "from",
                "Swap the months or widen the range.",
            ));
        }
        Ok(Self {
            basis,
            currency,
            from,
            to,
        })
    }

    pub const fn basis(&self) -> Basis {
        self.basis
    }

    pub const fn currency(&self) -> &'static Currency {
        self.currency
    }

    pub const fn from(&self) -> YearMonth {
        self.from
    }

    pub const fn to(&self) -> YearMonth {
        self.to
    }

    /// The source's `arr_value` query value, e.g. `USD_2025-01_2025-12`.
    /// Only ASCII letters, digits, `-`, and `_`, so it needs no escaping.
    pub(crate) fn arr_value(&self) -> String {
        format!("{}_{}_{}", self.currency.code, self.from, self.to)
    }
}

/// One month's KRW rate for `unit` units of the series currency.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Observation {
    pub month: YearMonth,
    pub rate: f64,
}

/// A fetched series. Months without a published rate (including the current,
/// incomplete month) are absent rather than null.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Series {
    pub basis: Basis,
    pub currency: &'static str,
    pub name: &'static str,
    pub unit: u16,
    pub from: YearMonth,
    pub to: YearMonth,
    pub observations: Vec<Observation>,
    pub source_url: String,
}

/// Parses a decoded chart XML document into observations for `request`.
pub(crate) fn parse_chart(
    text: &str,
    request: &SeriesRequest,
    source_url: &str,
) -> Result<Vec<Observation>, SmbsError> {
    let changed =
        |message: String| SmbsError::source(ErrorCode::SourceChanged, message, source_url);
    // The JSP emits blank lines before the XML declaration, which XML forbids.
    let document = roxmltree::Document::parse(text.trim_start()).map_err(|error| {
        SmbsError::source(
            ErrorCode::SourceParseFailure,
            format!("smbs.biz returned malformed chart XML: {error}."),
            source_url,
        )
    })?;
    let chart = document.root_element();
    if chart.tag_name().name() != "chart" {
        return Err(changed(format!(
            "Expected a <chart> document, got <{}>.",
            chart.tag_name().name()
        )));
    }
    let caption = chart.attribute("caption").unwrap_or_default();
    if caption != request.basis.caption() {
        return Err(changed(format!(
            "Expected chart caption \"{}\", got \"{caption}\".",
            request.basis.caption()
        )));
    }

    let mut observations: Vec<Observation> = Vec::new();
    for node in chart.children().filter(roxmltree::Node::is_element) {
        // Rows must be direct <set> children; any other container (such as a
        // multi-series <dataset>) would otherwise read as an empty series.
        match node.tag_name().name() {
            "set" => {}
            "styles" => continue,
            other => return Err(changed(format!("Unexpected chart element <{other}>."))),
        }
        let set = node;
        let label = set.attribute("label").unwrap_or_default();
        let value = set.attribute("value").unwrap_or_default();
        let month = YearMonth::parse_source_label(label)
            .ok_or_else(|| changed(format!("Unexpected row label \"{label}\".")))?;
        let rate = parse_rate(value)
            .ok_or_else(|| changed(format!("Unexpected rate \"{value}\" for {month}.")))?;
        if month < request.from || month > request.to {
            return Err(changed(format!(
                "Row {month} is outside the requested range {}..{}.",
                request.from, request.to
            )));
        }
        if observations.last().is_some_and(|last| last.month >= month) {
            return Err(changed(format!("Row {month} is out of order or repeated.")));
        }
        observations.push(Observation { month, rate });
    }
    Ok(observations)
}

/// Accepts plain positive decimals such as `1490.11`. Any decimal with at most
/// 15 significant digits round-trips exactly through `f64` formatting.
fn parse_rate(value: &str) -> Option<f64> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, "0"));
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || !digits(fraction) || whole.len() + fraction.len() > 15 {
        return None;
    }
    value.parse::<f64>().ok().filter(|rate| *rate > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str =
        "http://www.smbs.biz/ExRate/MonAvgStdExRate_xml.jsp?arr_value=USD_2025-01_2025-03";

    fn request(basis: Basis) -> SeriesRequest {
        SeriesRequest::new(
            basis,
            Currency::parse("USD").unwrap(),
            YearMonth::new(2025, 1).unwrap(),
            YearMonth::new(2025, 3).unwrap(),
        )
        .unwrap()
    }

    fn chart(caption: &str, rows: &str) -> String {
        format!(
            "\r\n\r\n \r\n<?xml version=\"1.0\" encoding=\"EUC-KR\"?>   \r\n<chart \r\n\tcaption='{caption}'\r\n\tyAxisName='Rate (Won)' >\r\n{rows}\r\n<styles><definition><style name='CaptionFont' type='font' size='12' /></definition></styles>\r\n</chart>\r\n"
        )
    }

    #[test]
    fn parses_fictional_rows_for_both_bases() {
        let rows =
            "<set label='2025.01' value='1000.5' />\r\n<set  label='2025.02' value='1001.25' />";
        let observations = parse_chart(
            &chart("월평균 매매기준율", rows),
            &request(Basis::MonthlyAverage),
            URL,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_string(&observations).unwrap(),
            r#"[{"month":"2025-01","rate":1000.5},{"month":"2025-02","rate":1001.25}]"#
        );
        let month_end = parse_chart(
            &chart("월말 매매기준율", rows),
            &request(Basis::MonthEnd),
            URL,
        )
        .unwrap();
        assert_eq!(month_end.len(), 2);
    }

    #[test]
    fn an_empty_chart_is_an_empty_series() {
        let observations = parse_chart(
            &chart("월말 매매기준율", ""),
            &request(Basis::MonthEnd),
            URL,
        )
        .unwrap();
        assert!(observations.is_empty());
    }

    #[test]
    fn unexpected_shapes_are_source_changed() {
        let req = request(Basis::MonthlyAverage);
        let ok = "월평균 매매기준율";
        for (caption, rows) in [
            ("월말 매매기준율", ""),
            (ok, "<set label='2025-01' value='1000' />"),
            (ok, "<set label='2025.01' value='1,000.00' />"),
            (ok, "<set label='2025.01' value='-1' />"),
            (ok, "<set label='2025.01' value='0' />"),
            (ok, "<set label='2025.01' />"),
            (ok, "<set label='2024.12' value='1000' />"),
            (ok, "<set label='2025.04' value='1000' />"),
            (
                ok,
                "<set label='2025.02' value='1' /><set label='2025.01' value='1' />",
            ),
            (
                ok,
                "<set label='2025.01' value='1' /><set label='2025.01' value='1' />",
            ),
            (
                ok,
                "<dataset><set label='2025.01' value='1000' /></dataset>",
            ),
            (ok, "<categories /><set label='2025.01' value='1000' />"),
        ] {
            let error = parse_chart(&chart(caption, rows), &req, URL).unwrap_err();
            assert_eq!(error.code, ErrorCode::SourceChanged, "{caption} {rows}");
            assert_eq!(error.source_url.as_deref(), Some(URL));
        }
    }

    #[test]
    fn non_xml_is_a_parse_failure() {
        let html = "\r\n\r\n잘못된 접근입니다.\r\n<br/>\r\n<a href=\"/\">돌아가기</a>";
        let error = parse_chart(html, &request(Basis::MonthEnd), URL).unwrap_err();
        assert_eq!(error.code, ErrorCode::SourceParseFailure);
    }

    #[test]
    fn request_validation_and_arr_value() {
        let usd = Currency::parse("usd").unwrap();
        let jan = YearMonth::new(2025, 1).unwrap();
        let dec = YearMonth::new(2025, 12).unwrap();
        assert_eq!(
            SeriesRequest::new(Basis::MonthEnd, usd, jan, dec)
                .unwrap()
                .arr_value(),
            "USD_2025-01_2025-12"
        );
        assert!(SeriesRequest::new(Basis::MonthEnd, usd, jan, jan).is_ok());
        assert_eq!(
            SeriesRequest::new(Basis::MonthEnd, usd, dec, jan)
                .unwrap_err()
                .parameter
                .as_deref(),
            Some("from")
        );
        let old = YearMonth::new(1989, 12).unwrap();
        assert!(SeriesRequest::new(Basis::MonthEnd, usd, old, jan).is_err());

        // Hand-built currencies resolve to the catalog or are rejected.
        let wrong_unit = Currency {
            code: "USD",
            name: "custom",
            unit: 100,
        };
        let request = SeriesRequest::new(Basis::MonthEnd, &wrong_unit, jan, dec).unwrap();
        assert_eq!(
            (request.currency().name, request.currency().unit),
            ("미국 달러", 1)
        );
        for code in ["XXX", "USD&x=1"] {
            let unlisted = Currency {
                code,
                name: "custom",
                unit: 1,
            };
            assert_eq!(
                SeriesRequest::new(Basis::MonthEnd, &unlisted, jan, dec)
                    .unwrap_err()
                    .parameter
                    .as_deref(),
                Some("currency")
            );
        }
    }
}
