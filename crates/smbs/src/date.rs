use std::fmt;

use serde::{Serialize, Serializer};

use crate::SmbsError;

/// A calendar date. Serialized and displayed as `YYYY-MM-DD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    year: u16,
    month: u8,
    day: u8,
}

impl Date {
    /// Earliest date accepted in requests. The oldest observed daily row is
    /// 1990-03-02.
    pub const EARLIEST: Self = Self {
        year: 1990,
        month: 1,
        day: 1,
    };

    /// # Errors
    /// `invalid_request` unless the year is 1000-9999 and the day exists.
    pub fn new(year: u16, month: u8, day: u8) -> Result<Self, SmbsError> {
        if !(1000..=9999).contains(&year)
            || !(1..=12).contains(&month)
            || day == 0
            || day > days_in_month(year, month)
        {
            return Err(SmbsError::invalid(
                format!("Invalid date {year:04}-{month:02}-{day:02}."),
                "date",
                "Use an existing calendar day in YYYY-MM-DD form.",
            ));
        }
        Ok(Self { year, month, day })
    }

    /// Parses `YYYY-MM-DD` exactly.
    ///
    /// # Errors
    /// `invalid_request` naming `parameter` when the text is not an existing day.
    pub fn parse(text: &str, parameter: &str) -> Result<Self, SmbsError> {
        let bytes = text.as_bytes();
        let shaped = bytes.len() == 10
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes
                .iter()
                .enumerate()
                .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit());
        shaped
            .then(|| {
                Self::new(
                    text[..4].parse().ok()?,
                    text[5..7].parse().ok()?,
                    text[8..].parse().ok()?,
                )
                .ok()
            })
            .flatten()
            .ok_or_else(|| {
                SmbsError::invalid(
                    format!("\"{text}\" is not an existing date in YYYY-MM-DD form."),
                    parameter,
                    "Use YYYY-MM-DD, for example 2025-01-31.",
                )
            })
    }

    /// Parses the source's `YY.MM.DD` row label. The century is the one that
    /// places the date inside `from..=to`; `None` if no century or more than
    /// one does.
    pub(crate) fn parse_source_label(text: &str, from: Self, to: Self) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 8
            || bytes[2] != b'.'
            || bytes[5] != b'.'
            || !bytes
                .iter()
                .enumerate()
                .all(|(index, byte)| index == 2 || index == 5 || byte.is_ascii_digit())
        {
            return None;
        }
        let short_year: u16 = text[..2].parse().ok()?;
        let month: u8 = text[3..5].parse().ok()?;
        let day: u8 = text[6..].parse().ok()?;
        let mut candidates = (from.year / 100..=to.year / 100)
            .filter_map(|century| Self::new(century * 100 + short_year, month, day).ok())
            .filter(|date| (from..=to).contains(date));
        let date = candidates.next()?;
        candidates.next().is_none().then_some(date)
    }

    pub const fn year(self) -> u16 {
        self.year
    }

    pub const fn month(self) -> u8 {
        self.month
    }

    pub const fn day(self) -> u8 {
        self.day
    }
}

const fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        _ => 31,
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

impl Serialize for Date {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(text: &str) -> Date {
        Date::parse(text, "date").unwrap()
    }

    #[test]
    fn parses_existing_days_only() {
        assert_eq!(date("2024-02-29").to_string(), "2024-02-29");
        assert_eq!(date("2000-02-29").day(), 29);
        for bad in [
            "2025-02-29",
            "1900-02-29",
            "2025-04-31",
            "2025-13-01",
            "2025-01-00",
            "2025-1-01",
            "2025/01/01",
            "25-01-01",
            "+025-01-01",
            "2025-01-01 ",
        ] {
            assert_eq!(
                Date::parse(bad, "from").unwrap_err().parameter.as_deref(),
                Some("from"),
                "{bad}"
            );
        }
    }

    #[test]
    fn source_labels_take_the_century_inside_the_range() {
        let label = |text, from, to| Date::parse_source_label(text, date(from), date(to));
        assert_eq!(
            label("90.03.02", "1990-01-01", "2026-12-31"),
            Some(date("1990-03-02"))
        );
        assert_eq!(
            label("25.03.31", "1990-01-01", "2026-12-31"),
            Some(date("2025-03-31"))
        );
        // Outside the requested range in every century.
        assert_eq!(label("25.03.31", "2025-04-01", "2025-12-31"), None);
        // Two centuries inside the range is ambiguous.
        assert_eq!(label("95.01.02", "1995-01-01", "2095-12-31"), None);
        for bad in ["2025.03.31", "25-03-31", "25.02.30", "25.3.31"] {
            assert_eq!(label(bad, "1990-01-01", "2026-12-31"), None, "{bad}");
        }
    }
}
