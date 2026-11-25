//! Wide rate tables for file output: one row per period, one column per currency.

use std::collections::BTreeMap;

use smbs::{Basis, DailySeries, Date, Series, YearMonth};

/// A table row's period. Monthly and daily tables never mix kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Period {
    Month(YearMonth),
    Day(Date),
}

#[derive(Debug, PartialEq)]
pub struct Table {
    /// Worksheet name and CSV-independent label, e.g. `month-end`.
    pub name: &'static str,
    /// Korean source title, e.g. `월말 매매기준율`.
    pub title: &'static str,
    /// `month` or `date`.
    pub period_header: &'static str,
    /// One per currency: `USD`, `JPY (100)`.
    pub currency_headers: Vec<String>,
    /// Ascending periods with one cell per currency; `None` where that
    /// currency has no published rate.
    pub rows: Vec<(Period, Vec<Option<f64>>)>,
}

impl Table {
    pub fn monthly(basis: Basis, series: &[Series]) -> Self {
        let (name, title) = match basis {
            Basis::MonthlyAverage => ("monthly-average", "월평균 매매기준율"),
            Basis::MonthEnd => ("month-end", "월말 매매기준율"),
        };
        let columns = series.iter().map(|s| {
            let rates = s
                .observations
                .iter()
                .map(|o| (Period::Month(o.month), o.rate));
            (header(s.currency, s.unit), rates.collect())
        });
        Self::build(name, title, "month", columns.collect())
    }

    pub fn daily(series: &[DailySeries]) -> Self {
        let columns = series.iter().map(|s| {
            let rates = s.observations.iter().map(|o| (Period::Day(o.date), o.rate));
            (header(s.currency, s.unit), rates.collect())
        });
        Self::build("daily", "기간별 매매기준율", "date", columns.collect())
    }

    fn build(
        name: &'static str,
        title: &'static str,
        period_header: &'static str,
        columns: Vec<(String, Vec<(Period, f64)>)>,
    ) -> Self {
        let mut by_period: BTreeMap<Period, Vec<Option<f64>>> = BTreeMap::new();
        for (index, (_, rates)) in columns.iter().enumerate() {
            for (period, rate) in rates {
                by_period
                    .entry(*period)
                    .or_insert_with(|| vec![None; columns.len()])[index] = Some(*rate);
            }
        }
        Self {
            name,
            title,
            period_header,
            currency_headers: columns.into_iter().map(|(header, _)| header).collect(),
            rows: by_period.into_iter().collect(),
        }
    }
}

fn header(code: &str, unit: u16) -> String {
    if unit == 1 {
        code.to_owned()
    } else {
        format!("{code} ({unit})")
    }
}

#[cfg(test)]
mod tests {
    use smbs::{DailyObservation, Observation};

    use super::*;

    fn month(text: &str) -> YearMonth {
        YearMonth::parse(text, "month").unwrap()
    }

    fn series(currency: &'static str, unit: u16, rows: &[(&str, f64)]) -> Series {
        Series {
            basis: Basis::MonthEnd,
            currency,
            name: "",
            unit,
            from: month("2025-01"),
            to: month("2025-03"),
            observations: rows
                .iter()
                .map(|(m, rate)| Observation {
                    month: month(m),
                    rate: *rate,
                })
                .collect(),
            source_url: String::new(),
        }
    }

    #[test]
    fn merges_currencies_into_one_row_per_period() {
        let table = Table::monthly(
            Basis::MonthEnd,
            &[
                series("USD", 1, &[("2025-01", 1.5), ("2025-03", 3.5)]),
                series("JPY", 100, &[("2025-02", 9.25), ("2025-03", 9.5)]),
            ],
        );
        assert_eq!(table.name, "month-end");
        assert_eq!(table.currency_headers, ["USD", "JPY (100)"]);
        assert_eq!(
            table.rows,
            [
                (Period::Month(month("2025-01")), vec![Some(1.5), None]),
                (Period::Month(month("2025-02")), vec![None, Some(9.25)]),
                (Period::Month(month("2025-03")), vec![Some(3.5), Some(9.5)]),
            ]
        );
    }

    #[test]
    fn daily_tables_use_dates() {
        let date = |text| Date::parse(text, "date").unwrap();
        let table = Table::daily(&[DailySeries {
            basis: smbs::DailyBasis::Daily,
            currency: "EUR",
            name: "",
            unit: 1,
            from: date("2025-03-28"),
            to: date("2025-03-31"),
            observations: vec![DailyObservation {
                date: date("2025-03-31"),
                rate: 1.0,
            }],
            source_url: String::new(),
        }]);
        assert_eq!(table.period_header, "date");
        assert_eq!(
            table.rows,
            [(Period::Day(date("2025-03-31")), vec![Some(1.0)])]
        );
    }
}
