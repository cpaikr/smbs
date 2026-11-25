use std::fmt;

use serde::{Serialize, Serializer};

use crate::SmbsError;

/// A calendar month. Serialized and displayed as `YYYY-MM`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct YearMonth {
    year: u16,
    month: u8,
}

impl YearMonth {
    /// Earliest month accepted in requests. The source's oldest observed
    /// rows are 1990-03 for both bases.
    pub const EARLIEST: Self = Self {
        year: 1990,
        month: 1,
    };

    /// # Errors
    /// `invalid_request` when the year is outside 1000-9999 or the month is not 1-12.
    pub fn new(year: u16, month: u8) -> Result<Self, SmbsError> {
        if !(1000..=9999).contains(&year) || !(1..=12).contains(&month) {
            return Err(SmbsError::invalid(
                format!("Invalid month {year:04}-{month:02}."),
                "month",
                "Use YYYY-MM with a month from 01 through 12.",
            ));
        }
        Ok(Self { year, month })
    }

    /// Parses `YYYY-MM` exactly (four-digit year, two-digit month).
    ///
    /// # Errors
    /// `invalid_request` naming `parameter` when the text is not a valid month.
    pub fn parse(text: &str, parameter: &str) -> Result<Self, SmbsError> {
        parse_with_separator(text, b'-').ok_or_else(|| {
            SmbsError::invalid(
                format!("\"{text}\" is not a month in YYYY-MM form."),
                parameter,
                "Use YYYY-MM, for example 2025-01.",
            )
        })
    }

    /// Parses the source's `YYYY.MM` row label.
    pub(crate) fn parse_source_label(text: &str) -> Option<Self> {
        parse_with_separator(text, b'.')
    }

    pub const fn year(self) -> u16 {
        self.year
    }

    pub const fn month(self) -> u8 {
        self.month
    }
}

fn parse_with_separator(text: &str, separator: u8) -> Option<YearMonth> {
    let bytes = text.as_bytes();
    if bytes.len() != 7
        || bytes[4] != separator
        || !bytes[..4].iter().chain(&bytes[5..]).all(u8::is_ascii_digit)
    {
        return None;
    }
    let year = text[..4].parse().ok()?;
    let month = text[5..].parse().ok()?;
    YearMonth::new(year, month).ok()
}

impl fmt::Display for YearMonth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}", self.year, self.month)
    }
}

impl Serialize for YearMonth {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_strict_forms() {
        let month = YearMonth::parse("2025-01", "from").unwrap();
        assert_eq!((month.year(), month.month()), (2025, 1));
        assert_eq!(month.to_string(), "2025-01");
        assert_eq!(
            YearMonth::parse_source_label("1990.03"),
            Some(YearMonth::new(1990, 3).unwrap())
        );
        for bad in [
            "2025-1",
            "2025-13",
            "2025-00",
            "25-01",
            "2025/01",
            "2025.01",
            " 2025-01",
            "２０25-01",
            "+025-01",
        ] {
            let error = YearMonth::parse(bad, "from").unwrap_err();
            assert_eq!(error.parameter.as_deref(), Some("from"), "{bad}");
        }
        assert_eq!(YearMonth::parse_source_label("2025-01"), None);
    }

    #[test]
    fn orders_chronologically() {
        assert!(YearMonth::new(2024, 12).unwrap() < YearMonth::new(2025, 1).unwrap());
    }
}
